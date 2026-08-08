//! The service registry: which user-space daemons exist, what authority each
//! is declared to hold, and whether one is running right now.
//!
//! A service is a named entry pairing a declared capability set with whatever
//! task is currently serving it. `build_service_registry` reads the boot plan
//! into the table, `ensure_virtio_block_service` starts the block daemon on
//! demand, and `refresh_virtio_service_state` reconciles the table against
//! what the scheduler says actually happened to the task.
//!
//! Third and last of the things W11 found under the "cooperative multitasking
//! scheduler" banner that are not a scheduler. This one is genuinely adjacent
//! to it - reconciling state means reading the task table - but adjacency is
//! not membership, and the direction of the dependency is one way.
//!
//! Boot hart only: services are declared, started and reconciled from the
//! console path.

use core::sync::atomic::Ordering;

use crate::sched::{MAX_TASKS, TEXIT, TSTATE, TaskState, reclaim_task_resources, run_foreground_processes, run_scheduler_from, spawn_process_at};
use crate::abi::{
    BLK_OP_CLIENT_REQ, BLK_OP_DAEMON, BLK_REQ_FAULT_DEMO, BLK_REQ_STOP,
    FIRST_FOREGROUND_TASK, VIRTIO_SERVICE_TASK,
};
use crate::audit::record_event;
use crate::arch::timer::TICKS;
use crate::mm::global::Global;
use crate::{kprintln, run_registered_virtio_client_status, virtio_dma_pa, KernelCapability, KernelPlan, ProcessSpec, ServiceKind, TaskKind, TASK_BLOCK_READ, TASK_BLOCK_WRITE, TASK_DEVICE_VIRTIO_BLK, TASK_IPC, TASK_PRINT, VIRTIO_BLK_ELF};

#[derive(Clone, Copy, PartialEq)]
enum ServiceState {
    Unused,
    Declared,
    Starting,
    Stopping,
    Running,
    Restarting,
    Faulted,
    Stopped,
}

#[derive(Clone, Copy)]
struct ServiceEntry {
    name: &'static str,
    kind: ServiceKind,
    state: ServiceState,
    task: usize,
    caps: usize,
    grants: usize,
    fault: &'static str,
    restart_count: usize,
    last_exit: usize,
    last_started_tick: u64,
}

const EMPTY_SERVICE: ServiceEntry = ServiceEntry {
    name: "",
    kind: ServiceKind::Init,
    state: ServiceState::Unused,
    task: usize::MAX,
    caps: 0,
    grants: 0,
    fault: "",
    restart_count: 0,
    last_exit: 0,
    last_started_tick: 0,
};

const MAX_SERVICES: usize = 8;
// Boot hart only, as above; the registry is written by build/ensure/refresh
// and read by print_services, all console-path.
static SERVICES: Global<[ServiceEntry; MAX_SERVICES]> = Global::new([EMPTY_SERVICE; MAX_SERVICES]);
static SERVICE_COUNT: Global<usize> = Global::new(0);

fn service_state_name(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Unused => "Unused",
        ServiceState::Declared => "Declared",
        ServiceState::Starting => "Starting",
        ServiceState::Stopping => "Stopping",
        ServiceState::Running => "Running",
        ServiceState::Restarting => "Restarting",
        ServiceState::Faulted => "Faulted",
        ServiceState::Stopped => "Stopped",
    }
}

fn task_caps_for(service: &str, plan: &KernelPlan) -> usize {
    let mut caps = TASK_PRINT;
    for seed in &plan.capability_seeds {
        if seed.service != service {
            continue;
        }
        match seed.capability {
            KernelCapability::SendIpc => caps |= TASK_IPC,
            KernelCapability::OpenVirtioDevice => {
                caps |= TASK_DEVICE_VIRTIO_BLK | TASK_BLOCK_READ | TASK_BLOCK_WRITE
            }
            KernelCapability::OpenCairnRoot => caps |= TASK_BLOCK_READ | TASK_BLOCK_WRITE,
            KernelCapability::StartService
            | KernelCapability::AllocateFrames
            | KernelCapability::MapAddressSpace
            | KernelCapability::OpenWasmRuntime => {}
        }
    }
    caps
}

fn service_index(name: &str) -> Option<usize> {
    unsafe {
        let mut i = 0usize;
        while i < *SERVICE_COUNT.get() {
            if (*SERVICES.get())[i].name == name {
                return Some(i);
            }
            i += 1;
        }
    }
    None
}

pub(crate) fn build_service_registry(plan: &KernelPlan) {
    unsafe {
        *SERVICE_COUNT.get() = 0;
        for service in &plan.services {
            if *SERVICE_COUNT.get() >= MAX_SERVICES {
                break;
            }
            let caps = task_caps_for(service.name, plan);
            let grants = match service.kind {
                ServiceKind::VirtioBlock => 0b11,
                ServiceKind::Cairn => 0b01,
                _ => 0,
            };
            (*SERVICES.get())[*SERVICE_COUNT.get()] = ServiceEntry {
                name: service.name,
                kind: service.kind,
                state: ServiceState::Declared,
                task: usize::MAX,
                caps,
                grants,
                fault: "",
                restart_count: 0,
                last_exit: 0,
                last_started_tick: 0,
            };
            *SERVICE_COUNT.get() += 1;
        }
    }
    kprintln!(
        "[dezh-boot] service registry built from boot plan ({} services)",
        unsafe { *SERVICE_COUNT.get() }
    );
}

pub(crate) fn refresh_virtio_service_state() {
    if let Some(i) = service_index("virtio-block") {
        unsafe {
            let task = (*SERVICES.get())[i].task;
            if task < MAX_TASKS {
                if (*TSTATE.get())[task] == TaskState::Blocked || (*TSTATE.get())[task] == TaskState::Ready {
                    (*SERVICES.get())[i].state = ServiceState::Running;
                    (*SERVICES.get())[i].fault = "";
                } else if (*TSTATE.get())[task] == TaskState::Done && (*TEXIT.get())[task] == 0 {
                    (*SERVICES.get())[i].state = ServiceState::Stopped;
                    (*SERVICES.get())[i].fault = "manual stop";
                    (*SERVICES.get())[i].last_exit = (*TEXIT.get())[task];
                    reclaim_task_resources(task);
                } else if (*TSTATE.get())[task] == TaskState::Done {
                    (*SERVICES.get())[i].state = ServiceState::Faulted;
                    (*SERVICES.get())[i].fault = "driver exited or faulted";
                    (*SERVICES.get())[i].last_exit = (*TEXIT.get())[task];
                    reclaim_task_resources(task);
                }
            }
        }
    }
}

pub(crate) fn ensure_virtio_block_service(_plan: &KernelPlan) -> Option<usize> {
    let idx = service_index("virtio-block")?;
    unsafe {
        let task = (*SERVICES.get())[idx].task;
        if (*SERVICES.get())[idx].state == ServiceState::Running
            && task < MAX_TASKS
            && ((*TSTATE.get())[task] == TaskState::Blocked || (*TSTATE.get())[task] == TaskState::Ready)
        {
            return Some(task);
        }
        if (*SERVICES.get())[idx].state == ServiceState::Stopped {
            kprintln!("[services] virtio-block unavailable: service is Stopped; use `svc-restart virtio-block`");
            return None;
        }
        if (*SERVICES.get())[idx].state == ServiceState::Faulted {
            kprintln!("[services] virtio-block unavailable: service is Faulted; use `svc-restart virtio-block`");
            return None;
        }
        (*SERVICES.get())[idx].state = ServiceState::Starting;
        (*SERVICES.get())[idx].task = VIRTIO_SERVICE_TASK;
        (*SERVICES.get())[idx].fault = "";
        (*SERVICES.get())[idx].last_started_tick = TICKS.load(Ordering::Relaxed);
        let caps = (*SERVICES.get())[idx].caps;
        kprintln!(
            "[services] starting virtio-block from boot registry as task {VIRTIO_SERVICE_TASK}"
        );
        let spec = ProcessSpec::new(VIRTIO_BLK_ELF, caps, BLK_OP_DAEMON)
            .args(virtio_dma_pa(), 0, 0)
            .virtio_blk()
            .virtio_dma();
        if !spawn_process_at(VIRTIO_SERVICE_TASK, &spec, TaskKind::Daemon) {
            (*SERVICES.get())[idx].state = ServiceState::Faulted;
            (*SERVICES.get())[idx].fault = "driver launch failed: out of frames";
            return None;
        }
    }
    run_scheduler_from(VIRTIO_SERVICE_TASK);
    refresh_virtio_service_state();
    unsafe {
        if (*SERVICES.get())[idx].state == ServiceState::Running {
            kprintln!(
                "[services] virtio-block Running (task {})",
                (*SERVICES.get())[idx].task
            );
            Some((*SERVICES.get())[idx].task)
        } else {
            kprintln!("[services] virtio-block Faulted: {}", (*SERVICES.get())[idx].fault);
            None
        }
    }
}

pub(crate) fn virtio_service_is_running() -> bool {
    refresh_virtio_service_state();
    if let Some(i) = service_index("virtio-block") {
        unsafe {
            return (*SERVICES.get())[i].state == ServiceState::Running;
        }
    }
    false
}

/// How many declared services are running right now. `print_status` wants the
/// count and nothing else; handing it the table would re-export the registry
/// to answer a question the registry can answer itself.
/// Which declared service, if any, a task is currently serving. Another query
/// the registry answers about itself rather than handing out the table.
pub(crate) fn service_for_task(task: usize) -> &'static str {
    unsafe {
        let mut i = 0usize;
        while i < *SERVICE_COUNT.get() {
            if (*SERVICES.get())[i].task == task {
                return (*SERVICES.get())[i].name;
            }
            i += 1;
        }
    }
    "-"
}

pub(crate) fn running_service_count() -> usize {
    unsafe {
        let mut n = 0usize;
        let mut i = 0usize;
        while i < *SERVICE_COUNT.get() {
            if (*SERVICES.get())[i].state == ServiceState::Running {
                n += 1;
            }
            i += 1;
        }
        n
    }
}

pub(crate) fn print_services() {
    refresh_virtio_service_state();
    unsafe {
        let count = *SERVICE_COUNT.get();
        kprintln!("runtime services ({} total):", count);
        let mut i = 0usize;
        while i < count {
            let s = (*SERVICES.get())[i];
            kprintln!(
                "  - {:<13} {:?} state={} task={} caps={:#x} grants={:#x} restarts={} last_exit={} started_tick={} {}",
                s.name,
                s.kind,
                service_state_name(s.state),
                s.task,
                s.caps,
                s.grants,
                s.restart_count,
                s.last_exit,
                s.last_started_tick,
                s.fault
            );
            i += 1;
        }
    }
}

pub(crate) fn svc_stop_virtio(_plan: &KernelPlan) {
    refresh_virtio_service_state();
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    let daemon;
    unsafe {
        if (*SERVICES.get())[idx].state != ServiceState::Running {
            kprintln!(
                "[services] virtio-block stop skipped: state={}",
                service_state_name((*SERVICES.get())[idx].state)
            );
            return;
        }
        daemon = (*SERVICES.get())[idx].task;
        (*SERVICES.get())[idx].state = ServiceState::Stopping;
        (*SERVICES.get())[idx].fault = "manual stop requested";
    }
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    kprintln!("[services] stopping virtio-block task={daemon} with typed STOP");
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, 0, BLK_REQ_STOP)
            .virtio_dma(),
    ]);
    let st = unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] };
    refresh_virtio_service_state();
    unsafe {
        kprintln!(
            "[services] svc-stop virtio-block status={} state={}",
            st,
            service_state_name((*SERVICES.get())[idx].state)
        );
    }
    record_event("console", "svc.stop", "virtio-block", "done");
}

pub(crate) fn svc_restart_virtio(_plan: &KernelPlan) {
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    refresh_virtio_service_state();
    unsafe {
        let task = (*SERVICES.get())[idx].task;
        if (*SERVICES.get())[idx].state == ServiceState::Running && task < MAX_TASKS {
            kprintln!("[services] restart requires stopped/faulted service; use svc-stop first");
            return;
        }
        (*SERVICES.get())[idx].state = ServiceState::Restarting;
        (*SERVICES.get())[idx].fault = "";
        (*SERVICES.get())[idx].task = usize::MAX;
        (*SERVICES.get())[idx].restart_count += 1;
    }
    let _ = ensure_virtio_block_service(_plan);
    refresh_virtio_service_state();
    unsafe {
        kprintln!(
            "[services] svc-restart virtio-block state={} restart_count={}",
            service_state_name((*SERVICES.get())[idx].state),
            (*SERVICES.get())[idx].restart_count
        );
    }
    record_event("console", "svc.restart", "virtio-block", "done");
}

pub(crate) fn svc_fault_demo_virtio(plan: &KernelPlan) {
    refresh_virtio_service_state();
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    unsafe {
        if (*SERVICES.get())[idx].state != ServiceState::Running {
            kprintln!(
                "[services] fault-demo skipped: state={}",
                service_state_name((*SERVICES.get())[idx].state)
            );
            return;
        }
    }
    let st = run_registered_virtio_client_status(plan, BLK_REQ_FAULT_DEMO, "");
    refresh_virtio_service_state();
    unsafe {
        kprintln!(
            "[services] svc-fault-demo virtio-block request_status={} state={} last_exit={}",
            st,
            service_state_name((*SERVICES.get())[idx].state),
            (*SERVICES.get())[idx].last_exit
        );
    }
    record_event("console", "svc.fault-demo", "virtio-block", "done");
}

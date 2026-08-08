//! Cooperative multitasking over U-mode tasks.
//!
//! Several U-mode tasks share the CPU by yielding (round-robin). Each has a
//! full register frame (saved and restored by `utrap`), its own 64 KiB stack
//! carved from the top of the user region, and its own capability set. Timer
//! preemption is a future refinement; for now switches happen on yield/exit.
//!
//! This is what was left under the "cooperative multitasking scheduler" banner
//! after steps 11-14 removed the event ledger, the IPC/block ABI, the service
//! registry and the block-daemon client. The move itself is pure relocation:
//! step 15 already converted every table here to `Global<T>`, so this commit
//! changes no semantics at all.
//!
//! Boot hart only for the tables below. `smp` runs tasks on secondary harts
//! through its own path today; W13 is where the two become one scheduler, and
//! where every `Global` here needs a concurrency argument stronger than
//! "only one hart reaches it".

use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::dev::plic::{EXT_IRQS, SCAUSE_EXTERNAL};
use crate::proc::loader::{ProcessSpec, EMPTY_TASK_RESOURCES, TaskKind, TaskResources};
use crate::mm::paging::set_active_task_mem;
use crate::mm::paging::{task_stack_top};
use crate::abi::{
    typed_word, FIRST_FOREGROUND_TASK, IPC_OP_TIMEOUT, IPC_SERVICE_SYSTEM, IPC_STATUS_TIMEOUT,
};
use crate::arch::finisher::{shutdown, FINISH_FAIL};
use crate::arch::timer::{rdtime, sbi_set_timer, QUANTUM, TICKS, TIMER_DELTA};
use crate::mm::global::Global;
use crate::proc::loader::{build_address_space, kernel_satp, proc_satp, USER_STACK_TOP};
use crate::{kprintln, plic_handle, reclaim_resources, restore_kernel_ctx, run_first, trap_entry, utrap, Uart, TASK_IPC, TASK_PRINT, TASK_TIME};
// Every one of these is used as a MATCH PATTERN below. A const that is not in
// scope does not fail to compile there - it silently becomes an irrefutable
// binding that matches everything, collapsing the whole syscall dispatch into
// its first arm. `cargo build` accepted exactly that; only the clippy gate's
// unreachable-pattern lint caught it. They are named explicitly, not globbed,
// so a future move cannot lose one quietly.
use crate::{SYS_DENIED, SYS_EXIT, SYS_IRQ_WAIT, SYS_NULL, SYS_PRINT, SYS_PRINTNUM, SYS_RECV, SYS_RECV_TIMEOUT, SYS_REPORT, SYS_SEND, SYS_UPTIME, SYS_YIELD};

pub(crate) const MAX_TASKS: usize = 4;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TaskState {
    Unused,
    Ready,
    Blocked, // waiting on msg_recv until a message arrives
    Done,
}

pub(crate) static TEXIT: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);

#[derive(Clone, Copy)]
struct IpcStats {
    sends: usize,
    receives: usize,
    denied_sends: usize,
    timeouts: usize,
    queue_full: usize,
    max_depth: usize,
}

static IPC_STATS: Global<IpcStats> = Global::new(IpcStats {
    sends: 0,
    receives: 0,
    denied_sends: 0,
    timeouts: 0,
    queue_full: 0,
    max_depth: 0,
});

// Small FIFO mailbox per task for capability-passing IPC. A message carries a
// small payload plus a *granted* capability set (attenuated to what the sender
// holds). Bounded queues avoid the classic service overwrite bug: two clients
// can enqueue while a service is busy, but unbounded memory growth is still
// impossible.
const MAILBOX_DEPTH: usize = 4;

#[derive(Clone, Copy)]
struct IpcMessage {
    from: usize,
    len: usize,
    grant: usize,
    sender_caps: usize, // kernel-attested caps the sender held at send time
    word: usize, // a register-passed scalar (used by the value-IPC / Cairn demo)
    buf: [u8; 64],
}

const EMPTY_IPC_MESSAGE: IpcMessage = IpcMessage {
    from: 0,
    len: 0,
    grant: 0,
    sender_caps: 0,
    word: 0,
    buf: [0; 64],
};

#[derive(Clone, Copy)]
struct Mailbox {
    head: usize,
    tail: usize,
    count: usize,
    slots: [IpcMessage; MAILBOX_DEPTH],
}

const EMPTY_MAILBOX: Mailbox = Mailbox {
    head: 0,
    tail: 0,
    count: 0,
    slots: [EMPTY_IPC_MESSAGE; MAILBOX_DEPTH],
};

static MBOX: Global<[Mailbox; MAX_TASKS]> = Global::new([EMPTY_MAILBOX; MAX_TASKS]);

static TRECV_WAITING: Global<[bool; MAX_TASKS]> = Global::new([false; MAX_TASKS]);
static TRECV_DEADLINE: Global<[u64; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TRECV_PTR: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TRECV_LEN: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);

static FRAMES: Global<[[usize; 32]; MAX_TASKS]> = Global::new([[0; 32]; MAX_TASKS]);
pub(crate) static TSTATE: Global<[TaskState; MAX_TASKS]> = Global::new([TaskState::Unused; MAX_TASKS]);
/// Tasks parked until a device interrupt arrives (see `SYS_IRQ_WAIT`).
pub(crate) static TIRQ_WAITING: Global<[bool; MAX_TASKS]> = Global::new([false; MAX_TASKS]);
pub(crate) static TCAPS: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TPERS: Global<[u8; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
// each task's address space (satp)
static TSATP: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
pub(crate) static TRES: Global<[TaskResources; MAX_TASKS]> = Global::new([EMPTY_TASK_RESOURCES; MAX_TASKS]);
static CURRENT: Global<usize> = Global::new(0);

fn clear_mailbox(i: usize) {
    unsafe {
        (*MBOX.get())[i] = EMPTY_MAILBOX;
        (*TRECV_WAITING.get())[i] = false;
        (*TRECV_DEADLINE.get())[i] = 0;
        (*TRECV_PTR.get())[i] = 0;
        (*TRECV_LEN.get())[i] = 0;
    }
}

unsafe fn recv_message_into(task: usize, frame: &mut [usize]) -> bool {
    if (*MBOX.get())[task].count == 0 {
        return false;
    }
    let head = (*MBOX.get())[task].head;
    let msg = (*MBOX.get())[task].slots[head];
    let n = msg.len.min(frame[F_A1]);
    if n > 0 {
        let dst = core::slice::from_raw_parts_mut(frame[F_A0] as *mut u8, n);
        dst.copy_from_slice(&msg.buf[..n]);
    }
    (*TCAPS.get())[task] |= msg.grant;
    (*MBOX.get())[task].slots[head] = EMPTY_IPC_MESSAGE;
    (*MBOX.get())[task].head = (head + 1) % MAILBOX_DEPTH;
    (*MBOX.get())[task].count -= 1;
    frame[F_A0] = n;
    frame[F_A1] = msg.from;
    frame[F_A2] = msg.word;
    // Services check the SENDER's authority (not their own) against this
    // kernel-attested value; a client cannot forge it from user space.
    frame[F_A3] = msg.sender_caps;
    (*IPC_STATS.get()).receives += 1;
    true
}

unsafe fn expire_recv_timeouts() {
    let now = TICKS.load(Ordering::Relaxed);
    let mut i = 0usize;
    while i < MAX_TASKS {
        if (*TRECV_WAITING.get())[i] && (*TSTATE.get())[i] == TaskState::Blocked && (*TRECV_DEADLINE.get())[i] <= now {
            if (*MBOX.get())[i].count > 0 {
                (*TRECV_WAITING.get())[i] = false;
                (*TSTATE.get())[i] = TaskState::Ready;
            } else {
                (*TRECV_WAITING.get())[i] = false;
                (*TRECV_DEADLINE.get())[i] = 0;
                (*TRECV_PTR.get())[i] = 0;
                (*TRECV_LEN.get())[i] = 0;
                (*FRAMES.get())[i][F_SEPC] += 4;
                (*FRAMES.get())[i][F_A0] = IPC_STATUS_TIMEOUT;
                (*FRAMES.get())[i][F_A1] = usize::MAX;
                (*FRAMES.get())[i][F_A2] =
                    typed_word(IPC_SERVICE_SYSTEM, IPC_OP_TIMEOUT, 0, IPC_STATUS_TIMEOUT, 0);
                (*TSTATE.get())[i] = TaskState::Ready;
                (*IPC_STATS.get()).timeouts += 1;
            }
        }
        i += 1;
    }
}

pub(crate) fn task_kind_name(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Empty => "-",
        TaskKind::Foreground => "foreground",
        TaskKind::Daemon => "daemon",
        TaskKind::LegacyBakedTask => "legacy",
    }
}

pub(crate) fn reclaim_task_resources(slot: usize) {
    unsafe {
        if slot >= MAX_TASKS || (*TRES.get())[slot].count == 0 {
            (*TSATP.get())[slot] = 0;
            return;
        }
        reclaim_resources(&mut (*TRES.get())[slot]);
        (*TSATP.get())[slot] = 0;
        (*TCAPS.get())[slot] = 0;
        (*TPERS.get())[slot] = PERS_NATIVE;
        clear_mailbox(slot);
    }
}

pub(crate) fn task_owned_frames(slot: usize) -> usize {
    unsafe {
        if slot < MAX_TASKS {
            (*TRES.get())[slot].count
        } else {
            0
        }
    }
}

pub(crate) fn owned_frames_by_kind(kind: TaskKind) -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TRES.get())[i].kind == kind {
                total += (*TRES.get())[i].count;
            }
            i += 1;
        }
        total
    }
}

pub(crate) fn process_owned_frames() -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            total += (*TRES.get())[i].count;
            i += 1;
        }
        total
    }
}

fn reclaim_finished_foreground_tasks() {
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            if (*TSTATE.get())[i] == TaskState::Done {
                reclaim_task_resources(i);
            }
            i += 1;
        }
    }
}

// A task's syscall personality: which ABI its `ecall`s speak.
pub(crate) const PERS_NATIVE: u8 = 0; // Dezh native syscalls (SYS_*)
pub(crate) const PERS_LINUX: u8 = 1; // Linux RISC-V syscall ABI, serviced by the Pol layer

// Frame index of the third arg register a2 = x12 -> 11.
const F_A2: usize = 11;

// Linux (riscv64 generic) syscall numbers we recognize; everything else ENOSYS.
pub(crate) const LINUX_WRITE: usize = 64;
pub(crate) const LINUX_EXIT: usize = 93;
const LINUX_EXIT_GROUP: usize = 94;
// Linux negative errno values, as returned in a0.
const LINUX_EBADF: usize = (-9i64) as usize;
const LINUX_EACCES: usize = (-13i64) as usize;
const LINUX_ENOSYS: usize = (-38i64) as usize;

// Frame index of register xN is N-1; a0=x10 -> 9, a1=x11 -> 10, a7=x17 -> 16,
// sp=x2 -> 1, sepc -> 31.
pub(crate) const F_A0: usize = 9;
pub(crate) const F_A1: usize = 10;
const F_A3: usize = 12;
const F_A4: usize = 13;
pub(crate) const F_A7: usize = 16;
pub(crate) const F_SP: usize = 1;
pub(crate) const F_SEPC: usize = 31;

fn frame_ptr(i: usize) -> *mut usize {
    unsafe { core::ptr::addr_of_mut!((*FRAMES.get())[i]) as *mut usize }
}

unsafe fn pick_next() -> Option<usize> {
    expire_recv_timeouts();
    for off in 0..MAX_TASKS {
        let i = (*CURRENT.get() + 1 + off) % MAX_TASKS;
        if (*TSTATE.get())[i] == TaskState::Ready {
            return Some(i);
        }
    }
    None
}

/// Pick the next Ready task and return its frame, or longjmp back to the console
/// if every task is finished.
/// Only tasks parked on a DEVICE may make the kernel idle. A task blocked on
/// IPC is waiting for another task, not for hardware - idling for it would wait
/// for an interrupt that is never coming.
unsafe fn any_irq_waiting() -> bool {
    (0..MAX_TASKS).any(|i| (*TIRQ_WAITING.get())[i] && (*TSTATE.get())[i] == TaskState::Blocked)
}

/// Idle until hardware makes someone runnable. We service the PLIC by hand
/// rather than relying on interrupt delivery: on trap entry the hardware clears
/// `sstatus.SIE`, so a pending interrupt would wake `wfi` but never be taken,
/// and the blocked task would never be woken. Claiming it here closes that hole.
unsafe fn idle_until_device() {
    const IDLE_LIMIT: u64 = 50_000_000;
    let mut spins = 0u64;
    while pick_next().is_none() && any_irq_waiting() {
        asm!("wfi");
        plic_handle();
        spins += 1;
        if spins > IDLE_LIMIT {
            // A device that never reports back must not wedge the machine.
            break;
        }
    }
}

unsafe fn schedule_or_return() -> *const usize {
    // Nothing ready but something blocked means work is pending on a device:
    // wait for it instead of abandoning the run. This is what makes blocking on
    // I/O possible at all - without it the kernel returns from the task loop and
    // orphans the sleeping driver.
    if pick_next().is_none() && any_irq_waiting() {
        idle_until_device();
    }
    match pick_next() {
        Some(i) => {
            *CURRENT.get() = i;
            set_active_task_mem(i); // give the new task its private stack, hide others
                                    // Switch to the task's address space (own satp for a loaded process,
                                    // the shared kernel satp for a baked task).
            asm!("csrw satp, {}", in(reg) (*TSATP.get())[i]);
            asm!("sfence.vma");
            frame_ptr(i) as *const usize
        }
        None => restore_kernel_ctx(),
    }
}

#[no_mangle]
extern "C" fn utrap_handler(frame_ptr: *mut usize) -> *const usize {
    let scause: usize;
    unsafe { asm!("csrr {}, scause", out(reg) scause) };
    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);
    let frame = unsafe { core::slice::from_raw_parts_mut(frame_ptr, 32) };

    unsafe {
        let cur = *CURRENT.get(); // snapshot before any reschedule (avoids &static_mut)
        if interrupt {
            // Supervisor timer = preemption: the running task's full frame is
            // already saved, so round-robin to the next ready task. A task that
            // never yields can no longer monopolize the CPU.
            if code == 5 {
                TICKS.fetch_add(1, Ordering::Relaxed);
                sbi_set_timer(rdtime() + QUANTUM);
                let _ = cur;
                return schedule_or_return();
            }
            // A device finished. Service it and resume the task: the machine no
            // longer has to spin waiting for hardware.
            if code == SCAUSE_EXTERNAL {
                plic_handle();
                return frame_ptr;
            }
            kprintln!("\n[dezh-boot] unexpected interrupt in task (scause={scause:#x}) -- halting");
            shutdown(FINISH_FAIL);
        }

        // A task that touches memory outside its region is killed (thesis at the
        // hardware boundary still holds for scheduled tasks).
        if matches!(code, 12 | 13 | 15) {
            let stval: usize;
            asm!("csrr {}, stval", out(reg) stval);
            kprintln!(
                "  [kernel] task {} DENIED: faulted on {stval:#x} (outside its grant) -- killing",
                cur
            );
            (*TSTATE.get())[cur] = TaskState::Done;
            (*TEXIT.get())[cur] = SYS_DENIED;
            return schedule_or_return();
        }

        if code == 8 {
            frame[F_SEPC] += 4; // resume after the ecall
            let caps = (*TCAPS.get())[cur];

            // Pol: a Linux-personality task speaks the Linux syscall ABI. We
            // translate each Linux syscall into a capability-checked Dezh action;
            // anything we do not support returns ENOSYS, just like the user-space
            // Linux personality spike (D014).
            if (*TPERS.get())[cur] == PERS_LINUX {
                match frame[F_A7] {
                    LINUX_WRITE => {
                        let fd = frame[F_A0];
                        if fd == 1 || fd == 2 {
                            if caps & TASK_PRINT != 0 {
                                let s = core::slice::from_raw_parts(
                                    frame[F_A1] as *const u8,
                                    frame[F_A2],
                                );
                                for &b in s {
                                    Uart.putc(b);
                                }
                                frame[F_A0] = frame[F_A2]; // bytes written
                            } else {
                                kprintln!(
                                    "  [pol/linux] write(fd={fd}) DENIED: task lacks PRINT capability -> -EACCES"
                                );
                                frame[F_A0] = LINUX_EACCES;
                            }
                        } else {
                            frame[F_A0] = LINUX_EBADF;
                        }
                        return frame_ptr;
                    }
                    LINUX_EXIT | LINUX_EXIT_GROUP => {
                        kprintln!("  [pol/linux] app exit (code {})", frame[F_A0]);
                        (*TSTATE.get())[cur] = TaskState::Done;
                        (*TEXIT.get())[cur] = frame[F_A0];
                        return schedule_or_return();
                    }
                    other => {
                        kprintln!("  [pol/linux] unsupported syscall {other} -> ENOSYS");
                        frame[F_A0] = LINUX_ENOSYS;
                        return frame_ptr;
                    }
                }
            }

            match frame[F_A7] {
                SYS_YIELD => {
                    (*TSTATE.get())[cur] = TaskState::Ready;
                    return schedule_or_return();
                }
                SYS_EXIT => {
                    kprintln!("  [kernel] task {} exited (code {})", cur, frame[F_A0]);
                    (*TSTATE.get())[cur] = TaskState::Done;
                    (*TEXIT.get())[cur] = frame[F_A0];
                    return schedule_or_return();
                }
                SYS_PRINT => {
                    if caps & TASK_PRINT != 0 {
                        let s = core::slice::from_raw_parts(frame[F_A0] as *const u8, frame[F_A1]);
                        for &b in s {
                            Uart.putc(b);
                        }
                        frame[F_A0] = 0;
                    } else {
                        kprintln!("  [kernel] DENIED print: task {cur} holds no PRINT capability");
                        frame[F_A0] = SYS_DENIED;
                    }
                    return frame_ptr;
                }
                SYS_UPTIME => {
                    if caps & TASK_TIME != 0 {
                        frame[F_A0] = TICKS.load(Ordering::Relaxed) as usize;
                    } else {
                        frame[F_A0] = SYS_DENIED;
                    }
                    return frame_ptr;
                }
                SYS_NULL => {
                    // Minimal syscall: the cheapest possible round trip.
                    return frame_ptr;
                }
                SYS_PRINTNUM => {
                    kprintln!("{}", frame[F_A0]);
                    frame[F_A0] = 0;
                    return frame_ptr;
                }
                SYS_REPORT => {
                    let ticks = frame[F_A0];
                    let iters = frame[F_A1];
                    // QEMU `virt` time CSR is 10 MHz => 1 tick = 100 ns.
                    let ns = ticks.saturating_mul(100).checked_div(iters).unwrap_or(0);
                    kprintln!(
                        "  [bench] ecall round-trip: ~{ns} ns/call  ({ticks} ticks / {iters} calls, QEMU-emulated)"
                    );
                    frame[F_A0] = 0;
                    return frame_ptr;
                }
                SYS_SEND => {
                    // msg_send(to=a0, ptr=a1, len=a2, grant_caps=a3)
                    if caps & TASK_IPC == 0 {
                        kprintln!("  [kernel] DENIED send: task {cur} holds no IPC capability");
                        (*IPC_STATS.get()).denied_sends += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let to = frame[F_A0];
                    let len = frame[F_A2].min(64);
                    let requested = frame[F_A3];
                    if to >= MAX_TASKS
                        || (*TSTATE.get())[to] == TaskState::Unused
                        || (*TSTATE.get())[to] == TaskState::Done
                    {
                        (*IPC_STATS.get()).denied_sends += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // ATTENUATION: a sender can only delegate capabilities it
                    // itself holds — never widen. (caps = sender's TCAPS.)
                    let granted = requested & caps;
                    if (*MBOX.get())[to].count == MAILBOX_DEPTH {
                        (*IPC_STATS.get()).queue_full += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let tail = (*MBOX.get())[to].tail;
                    let msg = &mut (*MBOX.get())[to].slots[tail];
                    if len > 0 {
                        let src = core::slice::from_raw_parts(frame[F_A1] as *const u8, len);
                        msg.buf[..len].copy_from_slice(src);
                    }
                    msg.len = len;
                    msg.from = cur;
                    msg.grant = granted;
                    msg.sender_caps = caps;
                    msg.word = frame[F_A4]; // register-passed scalar (value-IPC)
                    (*MBOX.get())[to].tail = (tail + 1) % MAILBOX_DEPTH;
                    (*MBOX.get())[to].count += 1;
                    (*IPC_STATS.get()).sends += 1;
                    if (*MBOX.get())[to].count > (*IPC_STATS.get()).max_depth {
                        (*IPC_STATS.get()).max_depth = (*MBOX.get())[to].count;
                    }
                    if (*TSTATE.get())[to] == TaskState::Blocked {
                        (*TRECV_WAITING.get())[to] = false;
                        (*TSTATE.get())[to] = TaskState::Ready;
                    }
                    frame[F_A0] = 0;
                    return frame_ptr;
                }
                SYS_RECV => {
                    // msg_recv(dest=a0, dest_cap=a1) -> bytes received in a0.
                    // Blocks (restartably) until a message is present.
                    if caps & TASK_IPC == 0 {
                        kprintln!("  [kernel] DENIED recv: task {cur} holds no IPC capability");
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    if recv_message_into(cur, frame) {
                        return frame_ptr;
                    } else {
                        // Re-run the ecall when we are scheduled again.
                        frame[F_SEPC] -= 4;
                        (*TSTATE.get())[cur] = TaskState::Blocked;
                        return schedule_or_return();
                    }
                }
                SYS_RECV_TIMEOUT => {
                    if caps & TASK_IPC == 0 {
                        kprintln!(
                            "  [kernel] DENIED recv-timeout: task {cur} holds no IPC capability"
                        );
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    if recv_message_into(cur, frame) {
                        return frame_ptr;
                    }
                    let timeout = frame[F_A2] as u64;
                    if timeout == 0 {
                        frame[F_A0] = IPC_STATUS_TIMEOUT;
                        frame[F_A1] = usize::MAX;
                        frame[F_A2] = typed_word(
                            IPC_SERVICE_SYSTEM,
                            IPC_OP_TIMEOUT,
                            0,
                            IPC_STATUS_TIMEOUT,
                            0,
                        );
                        (*IPC_STATS.get()).timeouts += 1;
                        return frame_ptr;
                    }
                    (*TRECV_WAITING.get())[cur] = true;
                    (*TRECV_PTR.get())[cur] = frame[F_A0];
                    (*TRECV_LEN.get())[cur] = frame[F_A1];
                    (*TRECV_DEADLINE.get())[cur] = TICKS.load(Ordering::Relaxed).saturating_add(timeout);
                    frame[F_SEPC] -= 4;
                    (*TSTATE.get())[cur] = TaskState::Blocked;
                    return schedule_or_return();
                }
                SYS_IRQ_WAIT => {
                    // Restartable: park the task and rewind past the ecall, so on
                    // wake it re-runs, sees the advanced count, and returns.
                    let prev = frame[F_A0];
                    let now = EXT_IRQS.load(Ordering::Relaxed) as usize;
                    if now != prev {
                        frame[F_A0] = now;
                        return frame_ptr;
                    }
                    (*TIRQ_WAITING.get())[cur] = true;
                    frame[F_SEPC] -= 4;
                    (*TSTATE.get())[cur] = TaskState::Blocked;
                    return schedule_or_return();
                }
                _ => {
                    frame[F_A0] = SYS_DENIED;
                    return frame_ptr;
                }
            }
        }

        kprintln!("\n[dezh-boot] unexpected trap in task (scause={scause:#x}) -- halting");
        shutdown(FINISH_FAIL);
    }
}

/// Set up `specs` as Ready tasks and run them round-robin until all finish.
/// Each spec is (entry, caps). Returns when every task is Done.
pub(crate) fn run_tasks(specs: &[(usize, usize, u8)]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        // Index form is still deliberate, for a different reason than before:
        // the tables are `Global<T>` now, and the iterator rewrite would need a
        // reference into `(*TSTATE.get())` - exactly the aliasing `Global` is
        // here to prevent. Indexing through the raw pointer never makes one.
        #[allow(clippy::needless_range_loop)]
        for i in 0..MAX_TASKS {
            reclaim_task_resources(i);
            (*TSTATE.get())[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        for (i, &(entry, caps, pers)) in specs.iter().take(n).enumerate() {
            let f = &mut (*FRAMES.get())[i];
            *f = [0; 32];
            f[F_SEPC] = entry;
            f[F_SP] = task_stack_top(i); // each task owns a private 2 MiB stack region
            (*TCAPS.get())[i] = caps;
            (*TPERS.get())[i] = pers;
            (*TSATP.get())[i] = kernel_satp(); // baked tasks share the kernel address space
            (*TRES.get())[i] = EMPTY_TASK_RESOURCES;
            (*TRES.get())[i].kind = TaskKind::LegacyBakedTask;
            (*TSTATE.get())[i] = TaskState::Ready;
        }
        *CURRENT.get() = 0;
        set_active_task_mem(0); // expose only task 0's stack region to start
                                // Switch to the multitasking trap path and arm the preemption timer.
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        run_first(frame_ptr(0) as *const usize);
        // Returned via restore_kernel_ctx once every task is Done.
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA); // restore the console uptime cadence
    }
}

/// Load and run several separate programs as real processes: each gets its own
/// ELF, its own address space (satp), an id in a0, and a capability set. They
/// run concurrently under the preemptive scheduler, isolated from one another,
/// and return to the console once all have exited. Zero ambient authority — a
/// process holds only the capabilities passed here (no fork).
pub(crate) fn run_processes(specs: &[ProcessSpec]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        // A loaded process must not see any baked-task stack region.
        set_active_task_mem(usize::MAX);
        // Index form is still deliberate, for a different reason than before:
        // the tables are `Global<T>` now, and the iterator rewrite would need a
        // reference into `(*TSTATE.get())` - exactly the aliasing `Global` is
        // here to prevent. Indexing through the raw pointer never makes one.
        #[allow(clippy::needless_range_loop)]
        for i in 0..MAX_TASKS {
            reclaim_task_resources(i);
            (*TSTATE.get())[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        let mut launched = 0usize;
        let mut first_ready = usize::MAX;
        for (i, spec) in specs.iter().take(n).enumerate() {
            let Some(build) = build_address_space(spec, TaskKind::Foreground) else {
                kprintln!("  [kernel] process launch failed: out of frames");
                continue;
            };
            let f = &mut (*FRAMES.get())[i];
            *f = [0; 32];
            f[F_SEPC] = build.entry;
            f[F_SP] = USER_STACK_TOP; // each process has its own stack in its own space
            f[F_A0] = spec.arg0;
            f[F_A1] = spec.arg1;
            f[F_A2] = spec.arg2;
            f[F_A3] = spec.arg3;
            (*TCAPS.get())[i] = spec.caps;
            (*TPERS.get())[i] = spec.personality;
            (*TSATP.get())[i] = proc_satp(build.root);
            (*TRES.get())[i] = build.resources;
            (*TSTATE.get())[i] = TaskState::Ready;
            if first_ready == usize::MAX {
                first_ready = i;
            }
            launched += 1;
        }
        if launched == 0 {
            return;
        }
        *CURRENT.get() = first_ready;
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) (*TSATP.get())[first_ready]); // enter the first process's address space
        asm!("sfence.vma");
        run_first(frame_ptr(first_ready) as *const usize);
        // Back in the kernel address space once every process has exited.
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TSTATE.get())[i] == TaskState::Done {
                reclaim_task_resources(i);
            }
            i += 1;
        }
    }
}

pub(crate) fn run_scheduler_from(first: usize) {
    unsafe {
        *CURRENT.get() = first;
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) (*TSATP.get())[first]);
        asm!("sfence.vma");
        run_first(frame_ptr(first) as *const usize);
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
    }
}

pub(crate) fn spawn_process_at(slot: usize, spec: &ProcessSpec, kind: TaskKind) -> bool {
    unsafe {
        reclaim_task_resources(slot);
        let Some(build) = build_address_space(spec, kind) else {
            kprintln!("  [kernel] process launch failed: out of frames");
            (*TSTATE.get())[slot] = TaskState::Unused;
            clear_mailbox(slot);
            return false;
        };
        let f = &mut (*FRAMES.get())[slot];
        *f = [0; 32];
        f[F_SEPC] = build.entry;
        f[F_SP] = USER_STACK_TOP;
        f[F_A0] = spec.arg0;
        f[F_A1] = spec.arg1;
        f[F_A2] = spec.arg2;
        f[F_A3] = spec.arg3;
        (*TCAPS.get())[slot] = spec.caps;
        (*TPERS.get())[slot] = spec.personality;
        (*TSATP.get())[slot] = proc_satp(build.root);
        (*TRES.get())[slot] = build.resources;
        (*TEXIT.get())[slot] = 0;
        clear_mailbox(slot);
        (*TSTATE.get())[slot] = TaskState::Ready;
        true
    }
}

fn clear_foreground_tasks() {
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            reclaim_task_resources(i);
            (*TSTATE.get())[i] = TaskState::Unused;
            clear_mailbox(i);
            (*TCAPS.get())[i] = 0;
            (*TEXIT.get())[i] = 0;
            i += 1;
        }
    }
}

pub(crate) fn run_foreground_processes(specs: &[ProcessSpec]) {
    let n = specs.len().min(MAX_TASKS - FIRST_FOREGROUND_TASK);
    set_active_task_mem(usize::MAX);
    clear_foreground_tasks();
    let mut launched = 0usize;
    let mut first_ready = usize::MAX;
    for (i, spec) in specs.iter().take(n).enumerate() {
        let slot = FIRST_FOREGROUND_TASK + i;
        if spawn_process_at(slot, spec, TaskKind::Foreground) {
            if first_ready == usize::MAX {
                first_ready = slot;
            }
            launched += 1;
        }
    }
    if launched == 0 {
        return;
    }
    run_scheduler_from(first_ready);
    reclaim_finished_foreground_tasks();
}

pub(crate) fn print_ipcstat() {
    unsafe {
        let stats = *IPC_STATS.get();
        kprintln!(
            "ipcstat: sends={} receives={} denied_sends={} timeouts={} queue_full={} max_depth={}",
            stats.sends,
            stats.receives,
            stats.denied_sends,
            stats.timeouts,
            stats.queue_full,
            stats.max_depth
        );
    }
}

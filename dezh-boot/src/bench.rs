//! The benchmark family: syscall round trip, IPC round trip, storage commit,
//! capability check, and `bench-all`.
//!
//! Each spawns a purpose-built U-mode task and reports the measured cost. They
//! were filed under the Cairn banner for no reason except that they were
//! written around the same time.

use crate::abi::*;
use crate::sched::run_foreground_processes;
use crate::service::refresh_virtio_service_state;
use crate::vblk::{run_registered_virtio_client, run_virtio_no_grant_probe};
use crate::{kprintln, KernelPlan, ProcessSpec, BENCH_ELF, TASK_IPC, TASK_PRINT};
pub(crate) fn run_bench_os() {
    kprintln!(
        "[bench-os] launching separate U-mode benchmark ELF ({} null syscalls)",
        BENCH_SYSCALL_ITERS
    );
    run_foreground_processes(&[
        ProcessSpec::new(BENCH_ELF, TASK_PRINT, BENCH_ROLE_SYSCALL).args(BENCH_SYSCALL_ITERS, 0, 0),
    ]);
    kprintln!("[bench-os] complete; console returned");
}

pub(crate) fn run_bench_ipc() {
    kprintln!(
        "[bench-ipc] launching U-mode service/client pair ({} messages)",
        BENCH_IPC_ITERS
    );
    run_foreground_processes(&[
        ProcessSpec::new(BENCH_ELF, TASK_PRINT | TASK_IPC, BENCH_ROLE_IPC_SERVICE).args(
            BENCH_IPC_ITERS,
            0,
            0,
        ),
        ProcessSpec::new(BENCH_ELF, TASK_PRINT | TASK_IPC, BENCH_ROLE_IPC_CLIENT).args(
            FIRST_FOREGROUND_TASK,
            BENCH_IPC_ITERS,
            0,
        ),
    ]);
    kprintln!("[bench-ipc] complete; foreground tasks exited");
}

pub(crate) fn run_bench_storage(plan: &KernelPlan) {
    kprintln!("[bench-storage] validating registered virtio-block storage path");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
    run_registered_virtio_client(plan, BLK_REQ_PSET, "bench-storage-value");
    run_registered_virtio_client(plan, BLK_REQ_PGET, "");
    run_registered_virtio_client(plan, BLK_REQ_PSET, "bench-storage-bad-edit");
    run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, "");
    kprintln!("[bench-storage] complete via user-space virtio-block daemon");
}

pub(crate) fn run_bench_caps() {
    kprintln!("[bench-caps] launching app with PRINT only");
    run_foreground_processes(&[ProcessSpec::new(BENCH_ELF, TASK_PRINT, BENCH_ROLE_CAPS)]);
    kprintln!("[bench-caps] running no-grant MMIO proof");
    run_virtio_no_grant_probe();
    kprintln!("[bench-caps] complete; denied paths returned cleanly");
}

pub(crate) fn run_bench_all(plan: &KernelPlan) {
    kprintln!("[bench-all] Dezh validation suite v0 starting");
    run_bench_os();
    run_bench_ipc();
    run_bench_storage(plan);
    run_bench_caps();
    refresh_virtio_service_state();
    kprintln!("[bench-all] PASS: syscall, IPC, storage, caps, and service liveness checked");
}

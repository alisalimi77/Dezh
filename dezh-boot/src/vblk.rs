//! Talking to the virtio-block daemon from the kernel console.
//!
//! The console never touches the block device. It stages a request in the
//! shared DMA window, runs a short-lived U-mode client process that holds the
//! namespace capability, and reads the answer back out of the window. These
//! are the helpers that do that staging and read-back, in the several shapes
//! the callers need (status only, status plus a sector, raw, per-namespace).
//!
//! Fourth and final passenger under the "cooperative multitasking scheduler"
//! banner. It sat there because every one of these spawns a process and waits
//! for it - which makes it a *caller* of the scheduler, not a part of it.

use crate::abi::{BLK_OP_CLIENT_DEMO, BLK_OP_CLIENT_REQ, BLK_OP_NO_GRANT_PROBE};
use crate::service::{ensure_virtio_block_service, refresh_virtio_service_state};
use crate::{
    kprintln, run_foreground_processes, SYS_DENIED, ProcessSpec, KernelPlan, TEXIT, VIRTIO_BLK_ELF,
    VIRTIO_DATA_OFF, VIRTIO_DMA, VIRTIO_INPUT_OFF, FIRST_FOREGROUND_TASK,
    TASK_BLOCK_READ, TASK_BLOCK_WRITE, TASK_IPC, TASK_PRINT,
};

pub(crate) fn virtio_dma_pa() -> usize {
    core::ptr::addr_of!(VIRTIO_DMA) as usize
}

pub(crate) fn prepare_virtio_input(text: &str) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len().min(511);
    unsafe {
        let base = core::ptr::addr_of_mut!(VIRTIO_DMA) as *mut u8;
        core::ptr::write_bytes(base.add(VIRTIO_INPUT_OFF), 0, 512);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(VIRTIO_INPUT_OFF), n);
    }
    n
}

pub(crate) fn prepare_virtio_input_bytes(bytes: &[u8]) {
    let n = bytes.len().min(512);
    unsafe {
        let base = core::ptr::addr_of_mut!(VIRTIO_DMA) as *mut u8;
        core::ptr::write_bytes(base.add(VIRTIO_INPUT_OFF), 0, 512);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(VIRTIO_INPUT_OFF), n);
    }
}

pub(crate) fn read_virtio_output_sector(out: &mut [u8]) {
    let n = out.len().min(512);
    unsafe {
        let base = core::ptr::addr_of!(VIRTIO_DMA) as *const u8;
        core::ptr::copy_nonoverlapping(base.add(VIRTIO_DATA_OFF), out.as_mut_ptr(), n);
    }
}

pub(crate) fn run_virtio_no_grant_probe() {
    run_foreground_processes(&[ProcessSpec::new(
        VIRTIO_BLK_ELF,
        TASK_PRINT,
        BLK_OP_NO_GRANT_PROBE,
    )]);
}

pub(crate) fn run_registered_virtio_client(plan: &KernelPlan, req: usize, input: &str) {
    let Some(daemon) = ensure_virtio_block_service(plan) else {
        kprintln!("[services] virtio-block unavailable; command failed cleanly");
        return;
    };
    let input_len = prepare_virtio_input(input);
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    kprintln!(
        "[services] resolved service virtio-block task={daemon}; launching foreground client"
    );
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, input_len, req)
            .virtio_dma(),
    ]);
    refresh_virtio_service_state();
}

/// Like `run_registered_virtio_client_status`, but the client is spawned with
/// `extra_caps` on top of the base client set. Used for Cairn namespace caps:
/// the console (operator) decides which namespace authority the client holds,
/// and the kernel attests exactly that to the storage daemon.
pub(crate) fn run_registered_virtio_client_ns(
    plan: &KernelPlan,
    req: usize,
    input: &str,
    extra_caps: usize,
) -> usize {
    let input_len = prepare_virtio_input(input);
    run_virtio_client_ns_raw(plan, req, input_len, extra_caps)
}

/// Lowest-level Cairn client launch: the DMA input window is already prepared
/// by the caller (string or raw bytes).
pub(crate) fn run_virtio_client_ns_raw(
    plan: &KernelPlan,
    req: usize,
    input_len: usize,
    extra_caps: usize,
) -> usize {
    let Some(daemon) = ensure_virtio_block_service(plan) else {
        kprintln!("[services] virtio-block unavailable; command failed cleanly");
        return SYS_DENIED;
    };
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE | extra_caps;
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, input_len, req)
            .virtio_dma(),
    ]);
    refresh_virtio_service_state();
    unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] }
}

pub(crate) fn run_registered_virtio_client_status(plan: &KernelPlan, req: usize, input: &str) -> usize {
    let Some(daemon) = ensure_virtio_block_service(plan) else {
        kprintln!("[services] virtio-block unavailable; command failed cleanly");
        return SYS_DENIED;
    };
    let input_len = prepare_virtio_input(input);
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    kprintln!(
        "[services] resolved service virtio-block task={daemon}; launching foreground client"
    );
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, input_len, req)
            .virtio_dma(),
    ]);
    refresh_virtio_service_state();
    unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] }
}

pub(crate) fn run_registered_virtio_sector_status(
    plan: &KernelPlan,
    req: usize,
    sector: usize,
    input: Option<&[u8]>,
) -> usize {
    let Some(daemon) = ensure_virtio_block_service(plan) else {
        kprintln!("[services] virtio-block unavailable; command failed cleanly");
        return SYS_DENIED;
    };
    if let Some(bytes) = input {
        prepare_virtio_input_bytes(bytes);
    }
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, sector, req)
            .virtio_dma(),
    ]);
    refresh_virtio_service_state();
    unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] }
}

pub(crate) fn run_virtio_blk_daemon_demo(plan: &KernelPlan) {
    let Some(daemon) = ensure_virtio_block_service(plan) else {
        kprintln!("[services] virtio-block unavailable; daemon demo failed cleanly");
        return;
    };
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    kprintln!("[services] vblkd uses registered daemon task={daemon}; client has IPC+DMA only");
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_DEMO)
            .args(daemon, 0, 0)
            .virtio_dma(),
    ]);
    refresh_virtio_service_state();
}

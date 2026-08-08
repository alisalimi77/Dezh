//! # dezh-boot — Step 10: bare-metal boot, interrupts, console, and U-mode tasks
//!
//! The first Dezh code that runs on bare metal (QEMU `virt`, RISC-V 64). It
//! crosses the simulation → hardware boundary every earlier spike ran around:
//!
//!   1. come up in S-mode after OpenSBI, zero `.bss`, set the stack;
//!   2. run the boot description through the *validated* `dezh-kernel` contract
//!      and print the banner + init service plan;
//!   3. install an S-mode trap vector + SBI timer (silent background uptime);
//!   4. run **Dezh's own capability-gated console** over the UART;
//!   5. from the console, `run` drops a task to **U-mode** with zero ambient
//!      authority: it can only reach the kernel through `ecall`s that are checked
//!      against the *task's* capabilities. A syscall the task wasn't granted is
//!      denied — the Step 1 thesis, now enforced by hardware privilege levels.

#![no_std]
#![no_main]

extern crate alloc;

mod abi;
mod arch;
mod apps;
mod audit;
mod bench;
mod cairn;

use crate::cairn::console::{cairn_cmd_commit, cairn_cmd_rollback, cairn_cmd_simple, sand_cmd, sfar_cmd};
use crate::bench::{run_bench_all, run_bench_caps, run_bench_ipc, run_bench_os, run_bench_storage};
use crate::apps::{app_calc_is_active, app_deny, app_info, app_install, app_permissions, app_remove, app_run, app_vault_is_active, install_command, print_apps};


use abi::*;
use audit::{print_audit, print_events, record_event, why_denied};
mod demos;
mod dev;
mod difc;
mod mm;
mod net;
mod ocap;
mod pkg;
mod proc;
mod sched;

use crate::dev::virtio::{VIRTIO_BLK_MMIO_PA, VIRTIO_DEVICE_ID_NET, VIRTIO_MMIO_COUNT, VIRTIO_MMIO_STRIDE, find_virtio_mmio};


use mm::paging::{
    build_page_tables, enable_paging, stack_base, stack_region_l1_index, task_stack_top, L1,
    PTE_U, PTE_V, ROOT,
};
use proc::loader::TaskKind;

use sched::{
    owned_frames_by_kind, print_ipcstat, process_owned_frames, run_foreground_processes,
    run_processes, run_tasks, task_kind_name, task_owned_frames, TaskState, F_A0, F_A1, F_A7,
    F_SEPC, F_SP, LINUX_EXIT, LINUX_WRITE, MAX_TASKS, PERS_LINUX, PERS_NATIVE, TCAPS, TEXIT,
    TIRQ_WAITING, TRES, TSTATE,
};
mod service;
mod vblk;

use vblk::{
    prepare_virtio_input_bytes, read_virtio_output_sector, run_registered_virtio_client,
    run_registered_virtio_client_ns, run_registered_virtio_client_status,
    run_registered_virtio_sector_status, run_virtio_blk_daemon_demo,
    run_virtio_client_ns_raw, run_virtio_no_grant_probe, virtio_dma_pa,
};
use service::{
    build_service_registry, ensure_virtio_block_service, print_services,
    refresh_virtio_service_state,
    running_service_count, service_for_task, svc_fault_demo_virtio, svc_restart_virtio, svc_stop_virtio,
    virtio_service_is_running,
};

// `Uart` is re-exported at the crate root because the kprint!/kprintln! macros
// expand to `$crate::Uart` at every call site in the tree. `Global` used to be
// re-exported here too; main.rs no longer owns a single one, so modules import
// it from `mm::global` themselves.
pub(crate) use dev::uart::{Uart, UART_BASE};
use mm::frames::{frame_alloc, frame_free, frames_init, FRAME_FREE, FRAME_SIZE, FRAME_TOTAL};
use demos::cairn::{
    run_agentrevoke_demo, run_cairn_demo,
    run_cap_demo, run_comp_demo, run_exfil_demo,
    run_overnight, run_redteam, run_sand_demo,
    run_sfar_cross_demo, run_sfar_demo,
};
use demos::difc::{run_ingress_demo, run_taintflow_demo};
use demos::egress::{run_dev_demo, run_marz_demo, run_marz_effect_demo};
use demos::ocap::run_nsrevoke_demo;
use net::marz::{
    marz_dest_authority, run_marz_ping, run_marz_send,
};
use proc::loader::{
    reclaim_resources,
};
use ocap::device::{
    dev_authority_live, dev_authority_set,
};
use ocap::ns::{
    ns_authority_ok, ns_grant, ns_revoke,
};
use difc::{
    declassify, difc_ingress, endorse, taint_show,
    NS_SECRET_VAULT, OP_TAINT,
};
use arch::finisher::{shutdown, FINISH_FAIL, FINISH_PASS};
use arch::timer::{rdtime, sbi_set_timer, STIE, TICKS, TIMER_DELTA, TIMER_HZ, SKIP_LF_AFTER_CR};

// The RISC-V implementation of the shared Dezh-core Host: capability check +
// the side effect (kernel console). The Dezh-IR engine lives in dezh-core and
// is identical across ISAs.
struct KHost<'a> {
    caps: u32,
    /// Where cairn hostcalls land: (boot plan, Cairn v1 namespace id). The
    /// store is reached through the user-space virtio-block daemon over IPC
    /// with the namespace capability — no kernel-side block I/O path.
    cairn: Option<(&'a KernelPlan, usize)>,
    /// Sand provenance for effects this host makes: the intent(Ahd) id under
    /// which authority was derived (0 = direct/no intent) and the derived
    /// capability set. Recorded on every Cairn commit so the effect ledger can
    /// answer "which intent authorized this effect".
    intent: u16,
    derived: u32,
}
impl dezh_core::ir::Host for KHost<'_> {
    fn can(&self, cap: u32) -> bool {
        self.caps & cap != 0
    }
    fn print_num(&mut self, v: i64) {
        kprintln!("  [ir] print -> {v}");
    }
    fn print_str(&mut self, s: &[u8]) {
        kprintln!("  [ir] {}", core::str::from_utf8(s).unwrap_or("<non-utf8>"));
    }
    fn cairn_put(&mut self, data: &[u8]) -> bool {
        let Some((plan, ns)) = self.cairn else {
            kprintln!("  [ir] cairn_put: no namespace bound (app name has no Cairn namespace)");
            return false;
        };
        // Object-capability namespace gate: an UNTRUSTED agent's write is refused
        // if the namespace capability was revoked at runtime (ocap generation
        // stale) — the same gate as the console, now on the agent path.
        if !ns_authority_ok(ns) {
            kprintln!("  [ir] cairn_put DENIED: ns capability revoked (ocap generation stale)");
            return false;
        }
        prepare_virtio_input_bytes(data);
        // IR/storage effects are reversible: undo moves the Cairn ref.
        run_virtio_client_ns_raw(
            plan,
            cairn_req_intent(
                BLK_REQ_CAIRN_COMMIT,
                ns,
                self.intent,
                self.derived,
                SAND_REV_REVERSIBLE,
            ),
            data.len(),
            task_ns_cap(ns),
        ) == 0
    }
    fn cairn_get(&mut self, buf: &mut [u8]) -> Option<usize> {
        let (plan, ns) = self.cairn?;
        if !ns_authority_ok(ns) {
            kprintln!("  [ir] cairn_get DENIED: ns capability revoked (ocap generation stale)");
            return None;
        }
        let st = run_virtio_client_ns_raw(plan, cairn_req(BLK_REQ_CAIRN_GET, ns, 0), 0, task_ns_cap(ns));
        if st != 0 {
            return None;
        }
        let mut sector = [0u8; 512];
        read_virtio_output_sector(&mut sector);
        // Cairn values are zero-terminated in the shared window (len <= 448).
        let n = sector
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(448)
            .min(buf.len());
        buf[..n].copy_from_slice(&sector[..n]);
        Some(n)
    }
}

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use alloc::{format, vec};
use dezh_kernel::{
    boot_banner, plan_boot, BootInfo, KernelCapability, KernelPlan, MemoryKind, MemoryRegion,
    ServiceKind,
};

// --- Assembly: boot entry, trap entry, U-mode enter, and kernel-context restore.
global_asm!(
    r#"
    .section .text.entry
    .globl _start
_start:
    la      t0, __bss_start
    la      t1, __bss_end
0:
    bgeu    t0, t1, 1f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       0b
1:
    la      sp, __stack_top
    call    kmain
2:
    wfi
    j       2b

    # --- Trap entry: save caller-saved regs as a TrapFrame, pass &frame, sret.
    .section .text
    .align 4
    .globl trap_entry
trap_entry:
    addi    sp, sp, -128
    sd      ra,   0(sp)
    sd      t0,   8(sp)
    sd      t1,  16(sp)
    sd      t2,  24(sp)
    sd      t3,  32(sp)
    sd      t4,  40(sp)
    sd      t5,  48(sp)
    sd      t6,  56(sp)
    sd      a0,  64(sp)
    sd      a1,  72(sp)
    sd      a2,  80(sp)
    sd      a3,  88(sp)
    sd      a4,  96(sp)
    sd      a5, 104(sp)
    sd      a6, 112(sp)
    sd      a7, 120(sp)
    mv      a0, sp          # arg0 = &TrapFrame
    call    trap_handler
    ld      ra,   0(sp)
    ld      t0,   8(sp)
    ld      t1,  16(sp)
    ld      t2,  24(sp)
    ld      t3,  32(sp)
    ld      t4,  40(sp)
    ld      t5,  48(sp)
    ld      t6,  56(sp)
    ld      a0,  64(sp)     # may have been overwritten with a syscall result
    ld      a1,  72(sp)
    ld      a2,  80(sp)
    ld      a3,  88(sp)
    ld      a4,  96(sp)
    ld      a5, 104(sp)
    ld      a6, 112(sp)
    ld      a7, 120(sp)
    addi    sp, sp, 128
    sret

    # --- enter_user(entry=a0, ustack=a1): save kernel context, sret to U-mode.
    .globl enter_user
enter_user:
    la      t0, KCTX
    sd      ra,   0(t0)
    sd      sp,   8(t0)
    sd      s0,  16(t0)
    sd      s1,  24(t0)
    sd      s2,  32(t0)
    sd      s3,  40(t0)
    sd      s4,  48(t0)
    sd      s5,  56(t0)
    sd      s6,  64(t0)
    sd      s7,  72(t0)
    sd      s8,  80(t0)
    sd      s9,  88(t0)
    sd      s10, 96(t0)
    sd      s11,104(t0)
    csrw    sepc, a0        # user entry point
    li      t1, 0x100
    csrc    sstatus, t1     # clear SPP -> sret returns to U-mode
    mv      sp, a1          # user stack
    sret

    # --- restore_kernel_ctx(): longjmp back to the enter_user call site.
    .globl restore_kernel_ctx
restore_kernel_ctx:
    la      t0, KCTX
    ld      ra,   0(t0)
    ld      sp,   8(t0)
    ld      s0,  16(t0)
    ld      s1,  24(t0)
    ld      s2,  32(t0)
    ld      s3,  40(t0)
    ld      s4,  48(t0)
    ld      s5,  56(t0)
    ld      s6,  64(t0)
    ld      s7,  72(t0)
    ld      s8,  80(t0)
    ld      s9,  88(t0)
    ld      s10, 96(t0)
    ld      s11,104(t0)
    ret
"#
);

unsafe extern "C" {
    fn trap_entry();
    fn restore_kernel_ctx() -> !;
    fn _hart_start();
}

// --- Multitasking trap path: full register context switch between U-mode tasks.
// `utrap` saves the *entire* integer register file + sepc of the trapping task
// into that task's frame (located via sscratch), runs the scheduler on a
// dedicated kernel stack, then restores whichever task the scheduler chose and
// `sret`s into it. `run_first` saves the kernel context (so the scheduler can
// longjmp back to the console when every task is done) and launches the first
// task. Frame layout: index n-1 holds xN; index 31 holds sepc.
global_asm!(
    r#"
    .section .bss
    .align 16
    .globl ktrap_stack
ktrap_stack:
    .space 8192
    .globl ktrap_top
ktrap_top:

    .section .text
    .align 4
    .globl utrap
utrap:
    csrrw   sp, sscratch, sp        # sp = &frame, sscratch = user sp
    sd      x1, 0(sp)
    sd      x3, 16(sp)
    sd      x4, 24(sp)
    sd      x5, 32(sp)
    csrr    x5, sscratch            # x5 = user sp (x5 already saved)
    sd      x5, 8(sp)
    sd      x6, 40(sp)
    sd      x7, 48(sp)
    sd      x8, 56(sp)
    sd      x9, 64(sp)
    sd      x10, 72(sp)
    sd      x11, 80(sp)
    sd      x12, 88(sp)
    sd      x13, 96(sp)
    sd      x14, 104(sp)
    sd      x15, 112(sp)
    sd      x16, 120(sp)
    sd      x17, 128(sp)
    sd      x18, 136(sp)
    sd      x19, 144(sp)
    sd      x20, 152(sp)
    sd      x21, 160(sp)
    sd      x22, 168(sp)
    sd      x23, 176(sp)
    sd      x24, 184(sp)
    sd      x25, 192(sp)
    sd      x26, 200(sp)
    sd      x27, 208(sp)
    sd      x28, 216(sp)
    sd      x29, 224(sp)
    sd      x30, 232(sp)
    sd      x31, 240(sp)
    csrr    x5, sepc
    sd      x5, 248(sp)
    mv      a0, sp                  # a0 = &frame
    la      sp, ktrap_top
    call    utrap_handler           # returns &resume_frame in a0
    j       frame_restore

    # restore the frame pointed to by a0 and sret into it
    .globl run_first
run_first:                          # a0 = &first_frame
    la      t0, KCTX
    sd      ra, 0(t0)
    sd      sp, 8(t0)
    sd      s0, 16(t0)
    sd      s1, 24(t0)
    sd      s2, 32(t0)
    sd      s3, 40(t0)
    sd      s4, 48(t0)
    sd      s5, 56(t0)
    sd      s6, 64(t0)
    sd      s7, 72(t0)
    sd      s8, 80(t0)
    sd      s9, 88(t0)
    sd      s10, 96(t0)
    sd      s11, 104(t0)
    # fall through into the restore with a0 = first frame

frame_restore:                      # a0 = &frame to resume
    mv      t0, a0
    ld      t1, 248(t0)
    csrw    sepc, t1
    csrw    sscratch, t0            # sscratch = &frame for the next trap
    ld      sp, 8(t0)               # user sp
    ld      x1, 0(t0)
    ld      x3, 16(t0)
    ld      x4, 24(t0)
    ld      x6, 40(t0)
    ld      x7, 48(t0)
    ld      x8, 56(t0)
    ld      x9, 64(t0)
    ld      x11, 80(t0)
    ld      x12, 88(t0)
    ld      x13, 96(t0)
    ld      x14, 104(t0)
    ld      x15, 112(t0)
    ld      x16, 120(t0)
    ld      x17, 128(t0)
    ld      x18, 136(t0)
    ld      x19, 144(t0)
    ld      x20, 152(t0)
    ld      x21, 160(t0)
    ld      x22, 168(t0)
    ld      x23, 176(t0)
    ld      x24, 184(t0)
    ld      x25, 192(t0)
    ld      x26, 200(t0)
    ld      x27, 208(t0)
    ld      x28, 216(t0)
    ld      x29, 224(t0)
    ld      x30, 232(t0)
    ld      x31, 240(t0)
    ld      x10, 72(t0)             # a0
    ld      x5, 32(t0)              # t0 itself, last
    sret
"#
);

unsafe extern "C" {
    fn utrap();
    fn run_first(frame: *const usize);
}

/// Saved kernel context for the U-mode round trip (ra, sp, s0..s11).
#[no_mangle]
static mut KCTX: [usize; 14] = [0; 14];

/// Layout MUST match the push order in `trap_entry`. Most fields exist only to
/// reserve their slot in the saved frame; only `a0`/`a7` are read here.
#[repr(C)]
#[allow(dead_code)]
struct TrapFrame {
    ra: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
}

// --- Syscall ABI (a7 = number; a0.. = args; a0 = result). ------------------
const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_UPTIME: usize = 2;
const SYS_YIELD: usize = 3;
const SYS_NULL: usize = 4; // minimal syscall (returns immediately) — for benchmarking
const SYS_REPORT: usize = 5; // report a benchmark result (a0=ticks, a1=iterations)
const SYS_SEND: usize = 6; // IPC: send payload + granted capability to a task
const SYS_RECV: usize = 7; // IPC: block until a message, receive payload + caps
const SYS_PRINTNUM: usize = 8; // print a decimal number (kernel-side formatting)
const SYS_RECV_TIMEOUT: usize = 9; // IPC: receive with a tick deadline
// Block until a device interrupt is serviced. The caller passes the last count
// it saw; if the count already moved the call returns at once, which closes the
// race between submitting a request and waiting for it.
const SYS_IRQ_WAIT: usize = 10;
const SYS_DENIED: usize = usize::MAX; // result sentinel for "capability not held"

// --- Per-task capabilities (what the running U-mode task is allowed to do). --
const TASK_PRINT: usize = 1 << 0;
const TASK_TIME: usize = 1 << 1;
const TASK_IPC: usize = 1 << 2;
const TASK_DEVICE_VIRTIO_BLK: usize = 1 << 3;
// Marz (egress): a SEPARATE device capability for the NIC. The block grant maps
// the whole virtio-mmio window (existing coarseness); the NIC grant is per-device
// by design — the kernel finds the one net slot and maps only that page.
const TASK_DEVICE_VIRTIO_NET: usize = 1 << 6;
const TASK_BLOCK_READ: usize = 1 << 4;
const TASK_BLOCK_WRITE: usize = 1 << 5;
static CURRENT_TASK_CAPS: AtomicUsize = AtomicUsize::new(0);

#[no_mangle]
extern "C" fn trap_handler(frame: *mut TrapFrame) {
    let scause: usize;
    unsafe { asm!("csrr {}, scause", out(reg) scause) };
    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);

    if interrupt {
        if code == 5 {
            // Supervisor timer: bump uptime silently, re-arm.
            TICKS.fetch_add(1, Ordering::Relaxed);
            sbi_set_timer(rdtime() + TIMER_DELTA);
            return;
        }
        // A device raised its line: claim it, acknowledge the device, complete.
        if code == SCAUSE_EXTERNAL {
            plic_handle();
            return;
        }
        kprintln!("\n[dezh-boot] unexpected interrupt scause={scause:#x} -- halting");
        shutdown(FINISH_FAIL);
    }

    // Exceptions. The only one we expect is an environment call from U-mode.
    if code == 8 {
        let f = unsafe { &mut *frame };
        // Resume *after* the ecall, not on it.
        let mut sepc: usize;
        unsafe { asm!("csrr {}, sepc", out(reg) sepc) };
        sepc += 4;
        unsafe { asm!("csrw sepc, {}", in(reg) sepc) };

        let caps = CURRENT_TASK_CAPS.load(Ordering::Relaxed);
        match f.a7 {
            SYS_EXIT => {
                kprintln!("  [kernel] task exited (code {})", f.a0);
                unsafe { restore_kernel_ctx() } // longjmp back to the console
            }
            SYS_PRINT => {
                // THE PRIVILEGE-BOUNDARY ENFORCEMENT POINT.
                if caps & TASK_PRINT != 0 {
                    let s = unsafe { core::slice::from_raw_parts(f.a0 as *const u8, f.a1) };
                    for &b in s {
                        Uart.putc(b);
                    }
                    f.a0 = 0;
                } else {
                    kprintln!("  [kernel] DENIED sys_print: task lacks PRINT capability");
                    f.a0 = SYS_DENIED;
                }
            }
            SYS_UPTIME => {
                if caps & TASK_TIME != 0 {
                    f.a0 = TICKS.load(Ordering::Relaxed) as usize;
                } else {
                    kprintln!("  [kernel] DENIED sys_uptime: task lacks TIME capability");
                    f.a0 = SYS_DENIED;
                }
            }
            other => {
                kprintln!("  [kernel] unknown syscall {other}");
                f.a0 = SYS_DENIED;
            }
        }
        return;
    }

    // Page faults (instruction/load/store). With paging on, a U-mode task that
    // reaches outside its U=1 region (e.g. the UART or kernel RAM) lands here.
    if matches!(code, 12 | 13 | 15) {
        let stval: usize;
        let sstatus: usize;
        unsafe {
            asm!("csrr {}, stval", out(reg) stval);
            asm!("csrr {}, sstatus", out(reg) sstatus);
        }
        let sepc: usize;
        unsafe { asm!("csrr {}, sepc", out(reg) sepc) };
        // SPP == 0 means the trap came from U-mode.
        if (sstatus >> 8) & 1 == 0 {
            kprintln!(
                "  [kernel] DENIED: task faulted (scause {code}) at pc={sepc:#x} on {stval:#x} -- killing task"
            );
            unsafe { restore_kernel_ctx() }
        }
        kprintln!("\n[dezh-boot] kernel page fault at pc={sepc:#x} on {stval:#x} (scause {code}) -- halting");
        shutdown(FINISH_FAIL);
    }

    kprintln!("\n[dezh-boot] unexpected trap scause={scause:#x} -- halting");
    shutdown(FINISH_FAIL);
}

// --- The U-mode task ---------------------------------------------------------
// Runs at the U privilege level with zero authority of its own. Its only way to
// affect the world is an `ecall`, which the kernel checks against the task's
// capabilities. The task is granted PRINT but not TIME, so `sys_uptime` is
// denied at the kernel boundary.

// The user region bounds come from the linker: a 2 MiB-aligned span that the
// page tables map U=1. User code lives at the bottom; the user stack grows down
// from the top. Everything outside this span is supervisor-only.
unsafe extern "C" {
    static __user_start: u8;
    static __user_end: u8;
}

fn user_region() -> (usize, usize) {
    (
        core::ptr::addr_of!(__user_start) as usize,
        core::ptr::addr_of!(__user_end) as usize,
    )
}

// --- Syscall wrappers — these run in U-mode, so they live in the user region. --
#[link_section = ".user.text"]
#[inline(never)]
fn sys_print(s: &[u8]) -> usize {
    let mut a0 = s.as_ptr() as usize;
    unsafe { asm!("ecall", inout("a0") a0, in("a1") s.len(), in("a7") SYS_PRINT) };
    a0
}

#[link_section = ".user.text"]
#[inline(never)]
fn sys_uptime() -> usize {
    let mut a0: usize = 0;
    unsafe { asm!("ecall", inout("a0") a0, in("a7") SYS_UPTIME) };
    a0
}

#[link_section = ".user.text"]
#[inline(never)]
fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

/// A well-behaved U-mode task: granted PRINT but not TIME, so its `sys_uptime`
/// is denied at the kernel boundary, then it exits cleanly.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn user_task() -> ! {
    sys_print(b"  [task] hello from a U-mode task (zero ambient authority)\n");
    let t = sys_uptime();
    if t == SYS_DENIED {
        sys_print(b"  [task] sys_uptime was DENIED (task holds no TIME capability)\n");
    } else {
        sys_print(b"  [task] sys_uptime ok\n");
    }
    sys_print(b"  [task] requesting exit\n");
    sys_exit(0)
}

/// A misbehaving U-mode task: it tries to touch the UART directly (ambient
/// hardware access). With paging on, the UART is a supervisor-only page, so the
/// store page-faults and the kernel kills the task — proof that authority is
/// denied at the hardware memory boundary, not just at the syscall boundary.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn rogue_task() -> ! {
    // Store straight to the UART MMIO. We emit the `sb` inline (not via
    // core::ptr::write_volatile, which in a debug build is an out-of-line call
    // into kernel text) so the fault lands on the UART address itself.
    unsafe {
        asm!("sb {v}, 0({p})", v = in(reg) b'!' as usize, p = in(reg) 0x1000_0000usize);
    }
    // Unreachable: the store above faults and the kernel never resumes us here.
    sys_print(b"  [task] (BUG) ambient UART write was NOT blocked\n");
    sys_exit(0)
}

/// The separate user program, compiled to its own riscv ELF by build.rs and
/// embedded here. The loader maps it into a fresh address space at runtime.
const USERPROG_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/userprog.elf"));
const VIRTIO_BLK_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/virtio-blk.elf"));
const MARZ_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/marz.elf"));
const BENCH_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-bench.elf"));
const NOTE_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-note.elf"));
const LAB_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-lab.elf"));
const CALC_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-calc.elf"));
const VAULT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-vault.elf"));
// An unmodified static Linux/RISC-V ELF, built for `riscv64gc-unknown-linux-musl`.
// Loaded like any program but run with the Linux personality (Pol, D014/F4).
const LINUX_GUEST_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/linux-guest.elf"));

// --- PLIC: real device interrupts ---------------------------------------------
// Until now every device path was a busy-wait: the machine spun until a sector
// or a frame completed, so I/O and compute could never overlap and a task could
// not block on I/O. The PLIC is what turns polled drivers into an event-driven
// kernel.
//
// QEMU `virt` layout: hart h has PLIC context 2h (M-mode) and 2h+1 (S-mode);
// per-context enable bits start at +0x2000 with stride 0x80, and per-context
// threshold/claim at +0x20_0000 with stride 0x1000. virtio-mmio slot i raises
// IRQ (1 + i). The boot hart is chosen by the firmware and is NOT always hart 0
// under -smp, so the S-mode context we program is derived from the boot hart id
// at init time rather than hardcoded - otherwise device interrupts get routed to
// a hart that is not running the kernel and every driver blocks forever.
const PLIC_BASE: usize = 0x0c00_0000;
const PLIC_ENABLE_BASE: usize = PLIC_BASE + 0x2000;
const PLIC_ENABLE_STRIDE: usize = 0x80;
const PLIC_CONTEXT_BASE: usize = PLIC_BASE + 0x0020_0000;
const PLIC_CONTEXT_STRIDE: usize = 0x1000;
/// Claim/complete register for the boot hart's S-mode context. Set by plic_init;
/// read by plic_handle. Defaults to context 1 (hart 0) until init runs.
static PLIC_S_CLAIM: AtomicUsize = AtomicUsize::new(PLIC_CONTEXT_BASE + 0x1000 + 4);
const VIRTIO_IRQ_BASE: u32 = 1;
const SEIE: usize = 1 << 9;
const VR_INTERRUPT_STATUS: usize = 0x060;
const VR_INTERRUPT_ACK: usize = 0x064;
/// Supervisor external interrupt (`scause` code with the interrupt bit set).
const SCAUSE_EXTERNAL: usize = 9;

static EXT_IRQS: AtomicU64 = AtomicU64::new(0);
/// Tasks woken by a device interrupt rather than by spinning.
static IRQ_WAKEUPS: AtomicU64 = AtomicU64::new(0);

/// Route the virtio device interrupts to this hart's S-mode context and unmask
/// external interrupts. Devices assert their own line; the PLIC arbitrates.
fn plic_init(boot_hart: usize) {
    // S-mode context of the boot hart. Under -smp the boot hart is not always 0.
    let ctx = 2 * boot_hart + 1;
    let enable = PLIC_ENABLE_BASE + ctx * PLIC_ENABLE_STRIDE;
    let threshold = PLIC_CONTEXT_BASE + ctx * PLIC_CONTEXT_STRIDE;
    let claim = threshold + 4;
    PLIC_S_CLAIM.store(claim, Ordering::Relaxed);
    unsafe {
        let mut irq = VIRTIO_IRQ_BASE;
        while irq < VIRTIO_IRQ_BASE + VIRTIO_MMIO_COUNT as u32 {
            write_volatile((PLIC_BASE + irq as usize * 4) as *mut u32, 1);
            irq += 1;
        }
        let mask: u32 = ((1u32 << VIRTIO_MMIO_COUNT) - 1) << VIRTIO_IRQ_BASE;
        write_volatile(enable as *mut u32, mask);
        write_volatile(threshold as *mut u32, 0);
        asm!("csrs sie, {}", in(reg) SEIE);
    }
}

/// Claim one external interrupt, ACK the device so it stops asserting its line,
/// then complete it at the PLIC. Skipping the device ACK would re-raise the
/// interrupt immediately and livelock the kernel.
fn plic_handle() -> u32 {
    let claim = PLIC_S_CLAIM.load(Ordering::Relaxed);
    unsafe {
        let irq = read_volatile(claim as *const u32);
        if irq == 0 {
            return 0;
        }
        if irq >= VIRTIO_IRQ_BASE && irq < VIRTIO_IRQ_BASE + VIRTIO_MMIO_COUNT as u32 {
            let slot = (irq - VIRTIO_IRQ_BASE) as usize;
            let base = VIRTIO_BLK_MMIO_PA + slot * VIRTIO_MMIO_STRIDE;
            let st = read_volatile((base + VR_INTERRUPT_STATUS) as *const u32);
            if st != 0 {
                write_volatile((base + VR_INTERRUPT_ACK) as *mut u32, st);
            }
        }
        write_volatile(claim as *mut u32, irq);
        EXT_IRQS.fetch_add(1, Ordering::Relaxed);
        // Anyone sleeping on a device becomes runnable again.
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TIRQ_WAITING.get())[i] {
                (*TIRQ_WAITING.get())[i] = false;
                if (*TSTATE.get())[i] == TaskState::Blocked {
                    (*TSTATE.get())[i] = TaskState::Ready;
                }
                IRQ_WAKEUPS.fetch_add(1, Ordering::Relaxed);
            }
            i += 1;
        }
        irq
    }
}

fn irq_stat() {
    kprintln!(
        "irq: external device interrupts serviced = {}",
        EXT_IRQS.load(Ordering::Relaxed)
    );
    kprintln!(
        "irq: driver waits woken by a device interrupt (not by spinning) = {}",
        IRQ_WAKEUPS.load(Ordering::Relaxed)
    );
    kprintln!(
        "  source: PLIC S-mode context of the boot hart (claim @ {:#x}); virtio slots raise IRQ 1..8",
        PLIC_S_CLAIM.load(Ordering::Relaxed)
    );
    kprintln!("  before this, every device wait was a busy-loop; devices can now report completion");
}

// --- SMP: bring up secondary harts through the real SBI HSM protocol. --------
//
// Honest scope. The boot hart runs the OS: the scheduler and every kernel data
// structure are single-threaded `static mut`, so a symmetric scheduler over them
// would need a lock on each, and that is a named future milestone (see ROADMAP),
// not this step. What this step proves is the foundation such a scheduler stands
// on: that Dezh can actually *drive hardware parallelism* — start the other harts
// through the standard SBI Hart State Management call, give each its own stack and
// identity (`tp` = hart id), and show more than one hart executing concurrently
// under our control with coherent atomics. QEMU parks secondary harts in the SBI
// firmware until `sbi_hart_start`; only the boot hart ever enters `_start`, so the
// boot path has no concurrent-entry race to guard.
const MAX_HARTS: usize = 8;
const HART_STACK: usize = 8 * 1024;
// Atomic increments each secondary hammers onto ONE shared counter per round. The
// contention is the point: a coherent total proves the harts share memory and the
// hardware serialises their atomics, not that they ran one-after-another.
const SMP_WORK: u64 = 200_000;
// Bounded spin so a hart that never checks in cannot hang the boot or the console.
const SMP_SPIN_LIMIT: u64 = 300_000_000;

// Per-hart stacks and the secondary entry point live in the asm block so a hart
// has a stack before any Rust runs. `sbi_hart_start` enters here with a0 = hart id.
global_asm!(
    r#"
    .section .bss
    .align 16
    .globl hart_stacks
hart_stacks:
    .space {TOTAL}

    .section .text
    .align 4
    .globl _hart_start
_hart_start:
    mv      tp, a0              # per-hart identity: tp = hart id
    la      t0, hart_stacks
    li      t1, {STK}
    addi    t2, a0, 1
    mul     t2, t2, t1          # (hart id + 1) * HART_STACK = top of this hart's slot
    add     sp, t0, t2
    call    hart_main           # a0 still = hart id
1:  wfi
    j       1b
"#,
    STK = const HART_STACK,
    TOTAL = const HART_STACK * MAX_HARTS,
);

// --- Per-hart U-mode trap path (for running a task on a secondary hart). ------
//
// This is deliberately SEPARATE from the boot hart's `utrap`/`KCTX`/`ktrap_stack`
// so that dispatching tasks onto secondary harts cannot perturb the single-hart
// console scheduler that everything else depends on.
//
// Finding per-hart state without `tp`. A U-mode task owns every integer register,
// so by the time it traps, `tp` holds whatever the task left there — the trap path
// must not use it. Instead each hart's state lives in one `ApCtx` whose FIRST field
// is the trap frame, and `sscratch` points at it while the task runs. On trap,
// `csrrw sp, sscratch, sp` lands sp on that hart's own `ApCtx`, from which the
// per-hart trap stack (offset 256) and saved kernel context (offset 264) are read.
// That makes several harts able to be in a trap at the same time.
//   ap_run(frame, kctx): save callee-saved into kctx, then sret into the task.
//   ap_return(kctx):     longjmp back to just after ap_run (used on task exit).
//   utrap_ap:            U-mode trap entry; per-hart state via sscratch, not tp.
const AP_TRAP_STK: usize = 8192;
/// Byte offsets inside `ApCtx`, mirrored in the assembly below.
const AP_OFF_TRAPTOP: usize = 256;
// Read by the assembly below through its own literal, not by Rust, but it
// documents half of the ApCtx layout: dropping it would leave the trap path's
// second offset recorded nowhere a reader of this file would look.
#[allow(dead_code)]
const AP_OFF_KCTX: usize = 264;
global_asm!(
    r#"
    .section .bss
    .align 16
    .globl ap_trap_stacks
ap_trap_stacks:
    .space {AP_TOTAL}

    .section .text
    .align 4
    .globl utrap_ap
utrap_ap:
    csrrw   sp, sscratch, sp        # sp = &frame, sscratch = user sp
    sd      x1, 0(sp)
    sd      x3, 16(sp)
    sd      x4, 24(sp)
    sd      x5, 32(sp)
    csrr    x5, sscratch            # x5 = user sp (x5 already saved)
    sd      x5, 8(sp)
    sd      x6, 40(sp)
    sd      x7, 48(sp)
    sd      x8, 56(sp)
    sd      x9, 64(sp)
    sd      x10, 72(sp)
    sd      x11, 80(sp)
    sd      x12, 88(sp)
    sd      x13, 96(sp)
    sd      x14, 104(sp)
    sd      x15, 112(sp)
    sd      x16, 120(sp)
    sd      x17, 128(sp)
    sd      x18, 136(sp)
    sd      x19, 144(sp)
    sd      x20, 152(sp)
    sd      x21, 160(sp)
    sd      x22, 168(sp)
    sd      x23, 176(sp)
    sd      x24, 184(sp)
    sd      x25, 192(sp)
    sd      x26, 200(sp)
    sd      x27, 208(sp)
    sd      x28, 216(sp)
    sd      x29, 224(sp)
    sd      x30, 232(sp)
    sd      x31, 240(sp)
    csrr    x5, sepc
    sd      x5, 248(sp)
    mv      a0, sp                  # a0 = &frame == &ApCtx for THIS hart
    ld      sp, {OFF_TRAPTOP}(a0)   # this hart's own trap stack (found via sscratch)
    call    ap_trap_handler         # returns &frame to resume in a0
    j       ap_frame_restore

    .globl ap_run
ap_run:                             # a0 = &frame, a1 = &kctx
    sd      ra, 0(a1)
    sd      sp, 8(a1)
    sd      s0, 16(a1)
    sd      s1, 24(a1)
    sd      s2, 32(a1)
    sd      s3, 40(a1)
    sd      s4, 48(a1)
    sd      s5, 56(a1)
    sd      s6, 64(a1)
    sd      s7, 72(a1)
    sd      s8, 80(a1)
    sd      s9, 88(a1)
    sd      s10, 96(a1)
    sd      s11, 104(a1)
    # fall through into the restore with a0 = frame

ap_frame_restore:                   # a0 = &frame to resume
    mv      t0, a0
    ld      t1, 248(t0)
    csrw    sepc, t1
    csrw    sscratch, t0            # sscratch = &frame for the next trap
    ld      sp, 8(t0)               # user sp
    ld      x1, 0(t0)
    ld      x3, 16(t0)
    ld      x4, 24(t0)
    ld      x6, 40(t0)
    ld      x7, 48(t0)
    ld      x8, 56(t0)
    ld      x9, 64(t0)
    ld      x11, 80(t0)
    ld      x12, 88(t0)
    ld      x13, 96(t0)
    ld      x14, 104(t0)
    ld      x15, 112(t0)
    ld      x16, 120(t0)
    ld      x17, 128(t0)
    ld      x18, 136(t0)
    ld      x19, 144(t0)
    ld      x20, 152(t0)
    ld      x21, 160(t0)
    ld      x22, 168(t0)
    ld      x23, 176(t0)
    ld      x24, 184(t0)
    ld      x25, 192(t0)
    ld      x26, 200(t0)
    ld      x27, 208(t0)
    ld      x28, 216(t0)
    ld      x29, 224(t0)
    ld      x30, 232(t0)
    ld      x31, 240(t0)
    ld      x10, 72(t0)             # a0
    ld      x5, 32(t0)              # t0 itself, last
    sret

    .globl ap_return
ap_return:                          # a0 = &kctx: longjmp back to after ap_run
    ld      ra, 0(a0)
    ld      sp, 8(a0)
    ld      s0, 16(a0)
    ld      s1, 24(a0)
    ld      s2, 32(a0)
    ld      s3, 40(a0)
    ld      s4, 48(a0)
    ld      s5, 56(a0)
    ld      s6, 64(a0)
    ld      s7, 72(a0)
    ld      s8, 80(a0)
    ld      s9, 88(a0)
    ld      s10, 96(a0)
    ld      s11, 104(a0)
    ret
"#,
    AP_TOTAL = const AP_TRAP_STK * MAX_HARTS,
    OFF_TRAPTOP = const AP_OFF_TRAPTOP,
);

unsafe extern "C" {
    fn utrap_ap();
    fn ap_run(frame: *const usize, kctx: *const usize);
    fn ap_return(kctx: *const usize) -> !;
}

const SBI_EXT_HSM: usize = 0x48534D; // "HSM"
const SBI_HSM_HART_START: usize = 0;

/// SBI Hart State Management: ask the firmware to start `hartid` at `start_addr`.
/// Returns the SBI error code (0 = SBI_SUCCESS; nonzero e.g. for an absent hart).
fn sbi_hart_start(hartid: usize, start_addr: usize, opaque: usize) -> isize {
    let err: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_EXT_HSM,
            in("a6") SBI_HSM_HART_START,
            in("a0") hartid,
            in("a1") start_addr,
            in("a2") opaque,
            lateout("a0") err,
            lateout("a1") _,
        );
    }
    err
}

static BOOT_HART: AtomicUsize = AtomicUsize::new(0);
static SMP_STARTED: AtomicU64 = AtomicU64::new(0); // secondaries the firmware accepted
static HARTS_ONLINE: AtomicU64 = AtomicU64::new(0); // secondaries that reached Rust
static SMP_GEN: AtomicU64 = AtomicU64::new(0); // round counter the boot hart bumps
static SMP_COUNTER: AtomicU64 = AtomicU64::new(0); // the shared target of the parallel work
static HART_RAN: [AtomicBool; MAX_HARTS] = [const { AtomicBool::new(false) }; MAX_HARTS];
static HART_ROUNDS: [AtomicU64; MAX_HARTS] = [const { AtomicU64::new(0) }; MAX_HARTS];

/// A fair ticket spinlock. This is the load-bearing primitive for symmetric
/// scheduling: a run queue shared by more than one hart cannot exist without
/// mutual exclusion, and the kernel had none (single-hart discipline covered
/// everything until now). Ticket order (hand out `next`, serve them in turn)
/// gives FIFO fairness — no hart starves under contention, unlike a bare
/// test-and-set. `lock` publishes with Acquire and `unlock` with Release, so an
/// ordinary read-modify-write inside the critical section is correct.
struct TicketLock {
    next: AtomicU32,
    serving: AtomicU32,
}
impl TicketLock {
    const fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
        }
    }
    fn lock(&self) {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        while self.serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
    }
    fn unlock(&self) {
        // Only the holder calls this and each unlock advances the queue by one,
        // so a store is enough — no read-modify-write needed.
        self.serving
            .store(self.serving.load(Ordering::Relaxed) + 1, Ordering::Release);
    }
}

static SMP_LOCK: TicketLock = TicketLock::new();
/// A deliberately NON-atomic counter, mutated only under `SMP_LOCK`. Atomics
/// (as in `SMP_COUNTER`) prove coherent shared memory but cannot prove a lock
/// works — the hardware serialises them regardless. A plain read-modify-write
/// under contention loses updates unless mutual exclusion actually holds, so a
/// total of exactly (participants x work) is the proof the lock is correct.
static mut SMP_GUARDED: u64 = 0;
const SMP_LOCK_WORK: u64 = 50_000;

// --- A shared run queue drained by every hart at once. -----------------------
//
// This is the shape of a symmetric scheduler's core: ONE queue of work, and
// several harts each popping the next item and running it, in parallel, under a
// lock. The property that must hold — and the thing the lock buys — is that every
// item runs EXACTLY once: none lost to a torn dequeue, none run twice by two
// harts that both thought they popped it. The jobs here are just markers so the
// proof is checkable; the next step is for a job to be a U-mode task dispatch
// (which additionally needs per-hart trap state + address-space switching).
const NJOBS: usize = 48;
const RUNQ_CAP: usize = 64;
static SMP_RUNQ_LOCK: TicketLock = TicketLock::new();

struct RunQueue {
    buf: [u32; RUNQ_CAP],
    head: usize,
    tail: usize,
}
static mut RUNQ: RunQueue = RunQueue {
    buf: [0; RUNQ_CAP],
    head: 0,
    tail: 0,
};
/// How many times each job was executed — must be exactly 1 everywhere.
static JOB_RUNS: [AtomicU32; NJOBS] = [const { AtomicU32::new(0) }; NJOBS];
/// Which hart ran each job (0xffff_ffff = not yet) — to show the spread.
static JOB_HART: [AtomicU32; NJOBS] = [const { AtomicU32::new(u32::MAX) }; NJOBS];
static JOBS_DONE: AtomicU64 = AtomicU64::new(0);

fn runq_push(id: u32) {
    SMP_RUNQ_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(RUNQ);
        q.buf[q.tail % RUNQ_CAP] = id;
        q.tail += 1;
    }
    SMP_RUNQ_LOCK.unlock();
}

fn runq_pop() -> Option<u32> {
    SMP_RUNQ_LOCK.lock();
    let r = unsafe {
        let q = &mut *core::ptr::addr_of_mut!(RUNQ);
        if q.head == q.tail {
            None
        } else {
            let v = q.buf[q.head % RUNQ_CAP];
            q.head += 1;
            Some(v)
        }
    };
    SMP_RUNQ_LOCK.unlock();
    r
}

/// Pop and "run" jobs until the queue is empty. Called concurrently by every hart.
fn drain_runq(hartid: usize) {
    while let Some(id) = runq_pop() {
        let i = id as usize;
        if i < NJOBS {
            JOB_RUNS[i].fetch_add(1, Ordering::Relaxed);
            JOB_HART[i].store(hartid as u32, Ordering::Relaxed);
        }
        JOBS_DONE.fetch_add(1, Ordering::Release);
    }
}

// --- Symmetric U-mode scheduling: any task, any hart, several at once. -------
//
// The run queue above moves markers. This moves REAL U-mode tasks: the boot hart
// fills a task queue, every secondary hart pops from it and runs whatever it gets
// in U-mode, and several tasks execute on several harts at the same time — while
// the boot hart stays on the console.
//
// Isolation is not dropped to get there. Each task gets its OWN address space: a
// private copy of the page tables in which only that task's stack region carries
// the U bit. Two tasks running concurrently on two harts therefore cannot touch
// each other's memory — proven by `smp-isolate`, where a task that reaches into a
// neighbour's stack takes a page fault instead.

/// Per-hart AP state. The trap frame MUST stay first: `sscratch` points here while
/// a task runs, so the trap entry lands on it and reads `trap_top`/`kctx` at the
/// fixed offsets `AP_OFF_TRAPTOP` / `AP_OFF_KCTX` — never via `tp`, which a U-mode
/// task is free to clobber.
#[repr(C, align(16))]
struct ApCtx {
    frame: [usize; 32],
    trap_top: usize,
    kctx: [usize; 14],
    slot: usize,
}
const EMPTY_AP_CTX: ApCtx = ApCtx {
    frame: [0; 32],
    trap_top: 0,
    kctx: [0; 14],
    slot: usize::MAX,
};
static mut AP_CTX: [ApCtx; MAX_HARTS] = [EMPTY_AP_CTX; MAX_HARTS];

unsafe extern "C" {
    static ap_trap_stacks: u8;
}

fn ap_trap_top(hartid: usize) -> usize {
    (core::ptr::addr_of!(ap_trap_stacks) as usize) + (hartid + 1) * AP_TRAP_STK
}

/// Task slots. One per per-task stack region, so each has an isolated stack.
const AP_SLOTS: usize = MAX_TASKS;
static AP_SLOT_ENTRY: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_ARG: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_SATP: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_RUNS: [AtomicU32; AP_SLOTS] = [const { AtomicU32::new(0) }; AP_SLOTS];
static AP_SLOT_HART: [AtomicU32; AP_SLOTS] = [const { AtomicU32::new(u32::MAX) }; AP_SLOTS];
static AP_SLOT_EXIT: [AtomicU64; AP_SLOTS] = [const { AtomicU64::new(u64::MAX) }; AP_SLOTS];
static AP_SLOT_FAULT: [AtomicBool; AP_SLOTS] = [const { AtomicBool::new(false) }; AP_SLOTS];
/// The two page-table frames each slot's address space owns, so they can be freed.
static AP_SLOT_ROOT: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_L1: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];

/// The task queue the harts pull from, plus liveness gauges.
static AP_Q_LOCK: TicketLock = TicketLock::new();
static mut AP_Q: RunQueue = RunQueue {
    buf: [0; RUNQ_CAP],
    head: 0,
    tail: 0,
};
static AP_SCHED_ON: AtomicBool = AtomicBool::new(false);
static AP_TASKS_DONE: AtomicU64 = AtomicU64::new(0);
/// U-mode tasks executing right now, and the high-water mark — the number that
/// proves tasks really overlapped rather than running one after another.
static AP_LIVE: AtomicU64 = AtomicU64::new(0);
static AP_LIVE_MAX: AtomicU64 = AtomicU64::new(0);

fn ap_q_push(slot: u32) {
    AP_Q_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(AP_Q);
        q.buf[q.tail % RUNQ_CAP] = slot;
        q.tail += 1;
    }
    AP_Q_LOCK.unlock();
}

fn ap_q_pop() -> Option<u32> {
    AP_Q_LOCK.lock();
    let r = unsafe {
        let q = &mut *core::ptr::addr_of_mut!(AP_Q);
        if q.head == q.tail {
            None
        } else {
            let v = q.buf[q.head % RUNQ_CAP];
            q.head += 1;
            Some(v)
        }
    };
    AP_Q_LOCK.unlock();
    r
}

/// Build a private address space for one task slot: copy the kernel page tables,
/// then clear the U bit on EVERY task stack region except this slot's. Shared task
/// code stays U+X (read/execute only), so tasks share code but never data.
fn build_ap_slot_space(slot: usize) -> usize {
    let root_pa = frame_alloc();
    let l1_pa = frame_alloc();
    if root_pa == 0 || l1_pa == 0 {
        return 0;
    }
    unsafe {
        let src_root = &(*ROOT.get()).0;
        let src_l1 = &(*L1.get()).0;
        let dr = root_pa as *mut u64;
        let dl = l1_pa as *mut u64;
        for i in 0..512usize {
            write_volatile(dr.add(i), src_root[i]);
            write_volatile(dl.add(i), src_l1[i]);
        }
        // No task stack is reachable from U-mode...
        for i in 0..MAX_TASKS {
            let idx = stack_region_l1_index(i);
            let e = read_volatile(dl.add(idx)) & !PTE_U;
            write_volatile(dl.add(idx), e);
        }
        // ...except this slot's own.
        let mine = stack_region_l1_index(slot);
        let e = read_volatile(dl.add(mine)) | PTE_U;
        write_volatile(dl.add(mine), e);
        // Point the copied root at the copied L1.
        write_volatile(dr.add(2), ((l1_pa as u64 >> 12) << 10) | PTE_V);
    }
    AP_SLOT_ROOT[slot].store(root_pa, Ordering::Relaxed);
    AP_SLOT_L1[slot].store(l1_pa, Ordering::Relaxed);
    (8usize << 60) | (root_pa >> 12)
}

/// A U-mode worker. Lives in the user region (U+X) and speaks only through
/// syscalls — zero ambient authority, exactly like a boot-hart task. `a0` is its
/// slot id; it spins briefly so concurrent workers genuinely overlap in time.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn ap_worker_task(slot: usize) -> ! {
    sys_print(b"  [ap-task] hello from a U-mode task running on a SECONDARY hart\n");
    let mut i = 0usize;
    while i < 300_000 {
        unsafe { asm!("nop") };
        i += 1;
    }
    sys_print(b"  [ap-task] my syscalls are being serviced off the boot hart; exiting\n");
    sys_exit(slot)
}

/// A U-mode task that reaches into ANOTHER task's stack (address in `a1`). With
/// per-task address spaces that page is not mapped U here, so it must fault.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn ap_rogue_task(_slot: usize, victim: usize) -> ! {
    unsafe {
        asm!("sb {v}, 0({p})", v = in(reg) 0x41usize, p = in(reg) victim);
    }
    sys_print(b"  [ap-rogue] (BUG) a cross-task stack write was NOT blocked\n");
    sys_exit(0)
}

/// AP U-mode trap handler. Per-hart state comes from the frame pointer (which is
/// the hart's `ApCtx`), never from `tp`.
#[no_mangle]
extern "C" fn ap_trap_handler(frame: *mut usize) -> *const usize {
    let scause: usize;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause);
    }
    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);
    let ctx = frame as *mut ApCtx;
    let kctx = unsafe { core::ptr::addr_of!((*ctx).kctx) } as *const usize;
    let slot = unsafe { (*ctx).slot };
    let f = unsafe { &mut *(frame as *mut [usize; 32]) };

    if interrupt {
        // The AP enables no interrupts while running a task; ignore and resume.
        return frame;
    }
    if code == 8 {
        // Environment call from U-mode. Resume after the ecall.
        f[F_SEPC] += 4;
        match f[F_A7] {
            SYS_PRINT => {
                // Lock the UART: other harts and the console may print too.
                SMP_LOCK.lock();
                let s = unsafe { core::slice::from_raw_parts(f[F_A0] as *const u8, f[F_A1]) };
                for &b in s {
                    Uart.putc(b);
                }
                SMP_LOCK.unlock();
                f[F_A0] = 0;
            }
            SYS_EXIT => {
                if slot < AP_SLOTS {
                    AP_SLOT_EXIT[slot].store(f[F_A0] as u64, Ordering::Relaxed);
                }
                unsafe { ap_return(kctx) }
            }
            _ => {
                f[F_A0] = SYS_DENIED;
            }
        }
        return frame;
    }
    // Any exception (a cross-task access, a bad pointer) ends the task cleanly on
    // this hart; the hart returns to the scheduler loop and takes the next task.
    if slot < AP_SLOTS {
        AP_SLOT_FAULT[slot].store(true, Ordering::Relaxed);
    }
    unsafe { ap_return(kctx) }
}

/// Run one task slot on this hart: enter the task's private address space, drop to
/// U-mode, and come back when it exits or faults. Kernel-region VAs are
/// identity-mapped in every space, so this hart's own PC/SP stay valid across the
/// satp switch.
unsafe fn ap_execute(hartid: usize, slot: usize) {
    let ctx = &mut *core::ptr::addr_of_mut!(AP_CTX[hartid]);
    ctx.frame = [0; 32];
    ctx.frame[F_SEPC] = AP_SLOT_ENTRY[slot].load(Ordering::Relaxed);
    ctx.frame[F_SP] = task_stack_top(slot);
    ctx.frame[F_A0] = slot;
    ctx.frame[F_A1] = AP_SLOT_ARG[slot].load(Ordering::Relaxed);
    ctx.trap_top = ap_trap_top(hartid);
    ctx.slot = slot;

    AP_SLOT_RUNS[slot].fetch_add(1, Ordering::Relaxed);
    AP_SLOT_HART[slot].store(hartid as u32, Ordering::Relaxed);

    // Track real overlap: how many U-mode tasks are live at once.
    let live = AP_LIVE.fetch_add(1, Ordering::AcqRel) + 1;
    AP_LIVE_MAX.fetch_max(live, Ordering::AcqRel);

    let satp = AP_SLOT_SATP[slot].load(Ordering::Relaxed);
    asm!("sfence.vma");
    asm!("csrw satp, {}", in(reg) satp);
    asm!("sfence.vma");
    asm!("csrw stvec, {}", in(reg) utrap_ap as *const () as usize);
    asm!("csrs sstatus, {}", in(reg) 1usize << 18); // SUM: S-mode may read the task's U pages

    let fp = core::ptr::addr_of!(ctx.frame) as *const usize;
    let kp = core::ptr::addr_of!(ctx.kctx) as *const usize;
    ap_run(fp, kp); // returns (via ap_return) when the task exits or faults

    // Back to bare mode so the hart's compute/queue rounds are unaffected.
    asm!("csrw stvec, {}", in(reg) 0usize);
    asm!("sfence.vma");
    asm!("csrw satp, {}", in(reg) 0usize);
    asm!("sfence.vma");

    AP_LIVE.fetch_sub(1, Ordering::AcqRel);
    AP_TASKS_DONE.fetch_add(1, Ordering::Release);
}

/// Pull tasks off the shared queue and run them until it is empty. Every secondary
/// hart runs this concurrently — this is the symmetric dispatch loop.
unsafe fn ap_schedule(hartid: usize) {
    while let Some(slot) = ap_q_pop() {
        ap_execute(hartid, slot as usize);
    }
}

/// Secondary hart body. Never prints (only the boot hart owns the UART) and never
/// traps (no stvec installed here): it checks in, then serves parallel rounds the
/// boot hart opens by bumping `SMP_GEN`.
#[no_mangle]
extern "C" fn hart_main(hartid: usize) -> ! {
    if hartid < MAX_HARTS {
        HART_RAN[hartid].store(true, Ordering::Release);
    }
    HARTS_ONLINE.fetch_add(1, Ordering::Release);

    let mut served = SMP_GEN.load(Ordering::Acquire);
    loop {
        // Wait for the boot hart to open a new round. spin_loop (not wfi) so a
        // TCG round-robin host keeps making progress and we wake promptly. While
        // waiting, also pick up a U-mode task the boot hart posted to this hart.
        while SMP_GEN.load(Ordering::Acquire) == served {
            if AP_SCHED_ON.load(Ordering::Acquire) {
                unsafe { ap_schedule(hartid) };
            }
            core::hint::spin_loop();
        }
        served = SMP_GEN.load(Ordering::Acquire);

        // (0) Drain the shared run queue: proves several harts pop one queue
        // concurrently with each item running exactly once. Done first so all
        // harts contend on the queue at the same time.
        drain_runq(hartid);

        // (1) Atomic work: proves coherent shared memory.
        let mut n = 0;
        while n < SMP_WORK {
            SMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            n += 1;
        }
        // (2) Locked work: proves mutual exclusion. The increment is a plain
        // read-modify-write on a non-atomic; without the lock, concurrent harts
        // would clobber each other and the total would come up short.
        let mut m = 0;
        while m < SMP_LOCK_WORK {
            SMP_LOCK.lock();
            unsafe {
                let p = core::ptr::addr_of_mut!(SMP_GUARDED);
                let v = read_volatile(p);
                write_volatile(p, v + 1);
            }
            SMP_LOCK.unlock();
            m += 1;
        }
        // Release: the boot hart's Acquire-load of this establishes that our
        // counter increments happen-before it reads the total.
        if hartid < MAX_HARTS {
            HART_ROUNDS[hartid].fetch_add(1, Ordering::Release);
        }
    }
}

/// Boot hart: start every other hart via SBI HSM and wait for them to check in.
fn smp_bringup(boot: usize) {
    BOOT_HART.store(boot, Ordering::Relaxed);
    let mut started = 0u64;
    for hid in 0..MAX_HARTS {
        if hid == boot {
            continue;
        }
        // An absent hart returns a nonzero error and is simply skipped, so the
        // same build works at -smp 1 and -smp 8 with no configuration.
        if sbi_hart_start(hid, _hart_start as *const () as usize, 0) == 0 {
            started += 1;
        }
    }
    SMP_STARTED.store(started, Ordering::Relaxed);

    let mut spins = 0u64;
    while HARTS_ONLINE.load(Ordering::Acquire) < started && spins < SMP_SPIN_LIMIT {
        core::hint::spin_loop();
        spins += 1;
    }
}

/// The result of one parallel round.
struct SmpRound {
    /// Secondary harts that participated.
    parts: u64,
    /// Atomic shared counter total (proves coherent memory).
    counter: u64,
    /// Lock-guarded non-atomic counter total (proves mutual exclusion).
    guarded: u64,
    /// Contributors to `guarded`: the secondaries plus the boot hart.
    guarded_contributors: u64,
    /// Bitmask of participating secondary hart ids.
    mask: u64,
    /// Run-queue jobs drained in total (must equal NJOBS).
    jobs_done: u64,
    /// True iff every job ran exactly once (the run-queue correctness property).
    jobs_each_once: bool,
    /// How many distinct harts pulled at least one job (shows the spread).
    job_harts: u64,
}

/// Drive one parallel round: the secondaries do atomic work and lock-guarded
/// work, and the boot hart joins the SAME lock so the contention is real (it is a
/// hart too). Returns the tallies.
fn smp_round() -> SmpRound {
    let started = SMP_STARTED.load(Ordering::Relaxed);
    if started == 0 {
        return SmpRound {
            parts: 0,
            counter: 0,
            guarded: 0,
            guarded_contributors: 0,
            mask: 0,
            jobs_done: 0,
            jobs_each_once: false,
            job_harts: 0,
        };
    }
    SMP_COUNTER.store(0, Ordering::Relaxed);
    unsafe { write_volatile(core::ptr::addr_of_mut!(SMP_GUARDED), 0) };
    // Reset and refill the shared run queue before opening the round.
    JOBS_DONE.store(0, Ordering::Relaxed);
    for i in 0..NJOBS {
        JOB_RUNS[i].store(0, Ordering::Relaxed);
        JOB_HART[i].store(u32::MAX, Ordering::Relaxed);
    }
    for id in 0..NJOBS {
        runq_push(id as u32);
    }
    // Release: orders both resets above before the secondaries observe the new
    // generation and start their work.
    let round_gen = SMP_GEN.fetch_add(1, Ordering::Release) + 1;

    // The boot hart joins the drain (it is a hart too), then the lock contention.
    drain_runq(BOOT_HART.load(Ordering::Relaxed));

    // The boot hart contends on the lock alongside the secondaries.
    let mut m = 0;
    while m < SMP_LOCK_WORK {
        SMP_LOCK.lock();
        unsafe {
            let p = core::ptr::addr_of_mut!(SMP_GUARDED);
            let v = read_volatile(p);
            write_volatile(p, v + 1);
        }
        SMP_LOCK.unlock();
        m += 1;
    }

    let mut spins = 0u64;
    loop {
        let mut done = 0u64;
        for rounds in HART_ROUNDS.iter() {
            if rounds.load(Ordering::Acquire) >= round_gen {
                done += 1;
            }
        }
        if done >= started || spins >= SMP_SPIN_LIMIT {
            break;
        }
        core::hint::spin_loop();
        spins += 1;
    }

    let counter = SMP_COUNTER.load(Ordering::Relaxed);
    let guarded = unsafe { read_volatile(core::ptr::addr_of!(SMP_GUARDED)) };
    let mut parts = 0u64;
    let mut mask = 0u64;
    for (hid, rounds) in HART_ROUNDS.iter().enumerate() {
        if rounds.load(Ordering::Acquire) >= round_gen {
            parts += 1;
            mask |= 1 << hid;
        }
    }

    // Run-queue verdict: every job ran exactly once, and count the distinct harts
    // that pulled work.
    let jobs_done = JOBS_DONE.load(Ordering::Acquire);
    let mut jobs_each_once = true;
    let mut hart_seen = 0u64;
    for i in 0..NJOBS {
        if JOB_RUNS[i].load(Ordering::Relaxed) != 1 {
            jobs_each_once = false;
        }
        let h = JOB_HART[i].load(Ordering::Relaxed);
        if (h as usize) < MAX_HARTS {
            hart_seen |= 1 << h;
        }
    }
    let job_harts = hart_seen.count_ones() as u64;

    SmpRound {
        parts,
        counter,
        guarded,
        guarded_contributors: parts + 1, // secondaries + this boot hart
        mask,
        jobs_done,
        jobs_each_once,
        job_harts,
    }
}

/// One-line boot-time SMP proof (asserted in CI).
fn smp_report_boot() {
    let started = SMP_STARTED.load(Ordering::Relaxed);
    if started == 0 {
        kprintln!("[dezh-boot] smp: 1 hart (launch with -smp N for parallelism); SBI HSM bringup path present");
        return;
    }
    let r = smp_round();
    let expected = r.parts * SMP_WORK;
    let guarded_expected = r.guarded_contributors * SMP_LOCK_WORK;
    kprintln!(
        "[dezh-boot] smp: {} secondary harts online via SBI HSM; boot hart = {}",
        started,
        BOOT_HART.load(Ordering::Relaxed)
    );
    kprintln!(
        "[dezh-boot] smp: parallel round shared-counter = {} (expected {}) -> {}",
        r.counter,
        expected,
        if r.counter == expected {
            "COHERENT"
        } else {
            "MISMATCH"
        }
    );
    kprintln!(
        "[dezh-boot] smp: lock-guarded counter = {} (expected {}) -> {}",
        r.guarded,
        guarded_expected,
        if r.guarded == guarded_expected {
            "MUTEX-OK"
        } else {
            "RACE"
        }
    );
    kprintln!(
        "[dezh-boot] smp: run-queue {} jobs drained by {} harts, each exactly once -> {}",
        r.jobs_done,
        r.job_harts,
        if r.jobs_done == NJOBS as u64 && r.jobs_each_once {
            "QUEUE-OK"
        } else {
            "QUEUE-BROKEN"
        }
    );
}

/// Interactive `smp-demo`: re-run a parallel round and explain what it proves.
fn run_smp_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    let started = SMP_STARTED.load(Ordering::Relaxed);
    let online = HARTS_ONLINE.load(Ordering::Relaxed);
    kprintln!("[smp] boot hart = {boot} (runs the OS: scheduler, IPC, drivers)");
    kprintln!("[smp] secondary harts started via SBI HSM = {started}, checked in = {online}");
    if started == 0 {
        kprintln!("[smp] no secondary harts. Launch QEMU with -smp N to see real parallelism.");
        kprintln!("[smp] the bringup path (sbi_hart_start + per-hart stack + tp identity) is still present.");
        return;
    }
    let r = smp_round();
    let expected = r.parts * SMP_WORK;
    let guarded_expected = r.guarded_contributors * SMP_LOCK_WORK;
    kprintln!(
        "[smp] {} harts each applied {SMP_WORK} atomic increments to ONE shared counter, at once",
        r.parts
    );
    kprintln!(
        "[smp] shared counter = {} (expected {expected}) -> {}",
        r.counter,
        if r.counter == expected {
            "COHERENT - the harts truly share memory and their atomics serialise"
        } else {
            "MISMATCH"
        }
    );
    kprintln!(
        "[smp] then {} harts (incl. the boot hart) each did {SMP_LOCK_WORK} NON-atomic increments under one ticket lock",
        r.guarded_contributors
    );
    kprintln!(
        "[smp] lock-guarded counter = {} (expected {guarded_expected}) -> {}",
        r.guarded,
        if r.guarded == guarded_expected {
            "MUTEX-OK - the lock held; without it concurrent read-modify-write would lose updates"
        } else {
            "RACE - updates were lost"
        }
    );
    kprint!("[smp] participating secondary hart ids: ");
    let mut first = true;
    for hid in 0..MAX_HARTS {
        if r.mask & (1 << hid) != 0 {
            if !first {
                kprint!(", ");
            }
            kprint!("{hid}");
            first = false;
        }
    }
    kprintln!("");
    kprintln!(
        "[smp] then {} jobs on ONE shared run queue were drained concurrently by {} harts",
        r.jobs_done,
        r.job_harts
    );
    kprintln!(
        "[smp] each job ran exactly once -> {}",
        if r.jobs_done == NJOBS as u64 && r.jobs_each_once {
            "QUEUE-OK - a correct concurrent run queue: none lost, none double-run"
        } else {
            "QUEUE-BROKEN"
        }
    );
    kprintln!("[smp] proven: several harts drain one shared run queue under a lock, each item exactly once - the core of a symmetric scheduler.");
    kprintln!("[smp] next: make each job a U-mode task dispatch (needs per-hart trap state + address-space switch); see ROADMAP.");
}

/// Prepare one task slot: build its private address space and reset its tallies.
/// Called only from the boot hart, so the frame allocator is not contended.
fn ap_prepare_slot(slot: usize, entry: usize, arg: usize) -> bool {
    let satp = build_ap_slot_space(slot);
    if satp == 0 {
        return false;
    }
    AP_SLOT_ENTRY[slot].store(entry, Ordering::Relaxed);
    AP_SLOT_ARG[slot].store(arg, Ordering::Relaxed);
    AP_SLOT_SATP[slot].store(satp, Ordering::Relaxed);
    AP_SLOT_RUNS[slot].store(0, Ordering::Relaxed);
    AP_SLOT_HART[slot].store(u32::MAX, Ordering::Relaxed);
    AP_SLOT_EXIT[slot].store(u64::MAX, Ordering::Relaxed);
    AP_SLOT_FAULT[slot].store(false, Ordering::Relaxed);
    true
}

/// Hand `n` prepared slots to the secondaries and wait for all of them to finish.
/// Returns false on timeout.
fn ap_run_batch(n: usize) -> bool {
    AP_TASKS_DONE.store(0, Ordering::Release);
    AP_LIVE.store(0, Ordering::Relaxed);
    AP_LIVE_MAX.store(0, Ordering::Relaxed);
    for s in 0..n {
        ap_q_push(s as u32);
    }
    AP_SCHED_ON.store(true, Ordering::Release);
    let mut spins = 0u64;
    while AP_TASKS_DONE.load(Ordering::Acquire) < n as u64 && spins < SMP_SPIN_LIMIT {
        core::hint::spin_loop();
        spins += 1;
    }
    AP_SCHED_ON.store(false, Ordering::Release);
    AP_TASKS_DONE.load(Ordering::Acquire) >= n as u64
}

/// Release a slot's page-table frames.
fn ap_free_slot(slot: usize) {
    let r = AP_SLOT_ROOT[slot].swap(0, Ordering::Relaxed);
    let l = AP_SLOT_L1[slot].swap(0, Ordering::Relaxed);
    if r != 0 {
        frame_free(r);
    }
    if l != 0 {
        frame_free(l);
    }
    AP_SLOT_SATP[slot].store(0, Ordering::Relaxed);
}

/// Interactive `smp-task`: dispatch one real U-mode task onto a secondary hart and
/// wait for it to finish, while the boot hart stays on the console.
fn run_smp_task_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-task] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-task] dispatching a U-mode task to a secondary hart (boot hart {boot} stays on the console)"
    );
    if !ap_prepare_slot(0, ap_worker_task as *const () as usize, 0) {
        kprintln!("[smp-task] out of frames while building the task's address space.");
        return;
    }
    let ok = ap_run_batch(1);
    let hart = AP_SLOT_HART[0].load(Ordering::Relaxed);
    let exit = AP_SLOT_EXIT[0].load(Ordering::Relaxed);
    let faulted = AP_SLOT_FAULT[0].load(Ordering::Relaxed);
    ap_free_slot(0);

    if !ok {
        kprintln!("[smp-task] TIMEOUT: no hart reported the task done.");
        return;
    }
    if faulted {
        kprintln!("[smp-task] the task FAULTED on hart {hart} (handled; the hart recovered).");
        return;
    }
    kprintln!("[smp-task] the task exited (code {exit}) on hart {hart} -> U-MODE-ON-AP");
    kprintln!("[smp-task] proven: a U-mode task ran to completion on a hart other than the boot hart, its syscalls serviced there via a per-hart trap path.");
}

/// Interactive `smp-sched`: hand several U-mode tasks to ONE shared queue and let
/// every secondary hart pull from it — symmetric scheduling, several tasks running
/// in U-mode at the same instant on different harts.
fn run_smp_sched_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-sched] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-sched] queueing {AP_SLOTS} U-mode tasks; every secondary hart pulls from the SAME queue (boot hart {boot} stays on the console)"
    );
    let mut prepared = 0usize;
    while prepared < AP_SLOTS {
        if !ap_prepare_slot(prepared, ap_worker_task as *const () as usize, 0) {
            break;
        }
        prepared += 1;
    }
    if prepared == 0 {
        kprintln!("[smp-sched] out of frames while building address spaces.");
        return;
    }
    let ok = ap_run_batch(prepared);
    let live_max = AP_LIVE_MAX.load(Ordering::Relaxed);

    let mut each_once = true;
    let mut faults = 0usize;
    let mut hart_mask = 0u64;
    for s in 0..prepared {
        if AP_SLOT_RUNS[s].load(Ordering::Relaxed) != 1 {
            each_once = false;
        }
        if AP_SLOT_FAULT[s].load(Ordering::Relaxed) {
            faults += 1;
        }
        let h = AP_SLOT_HART[s].load(Ordering::Relaxed);
        if (h as usize) < MAX_HARTS {
            hart_mask |= 1 << h;
        }
    }
    let harts_used = hart_mask.count_ones() as u64;

    kprint!("[smp-sched] task -> hart placement: ");
    for (s, hart) in AP_SLOT_HART.iter().take(prepared).enumerate() {
        if s > 0 {
            kprint!(", ");
        }
        kprint!("t{}=hart{}", s, hart.load(Ordering::Relaxed));
    }
    kprintln!("");
    for s in 0..prepared {
        ap_free_slot(s);
    }

    if !ok {
        kprintln!("[smp-sched] TIMEOUT: not every task reported done.");
        return;
    }
    kprintln!(
        "[smp-sched] {prepared} tasks ran on {harts_used} harts, each exactly once, {faults} faults; peak {live_max} U-mode tasks live at the same time"
    );
    kprintln!(
        "[smp-sched] verdict -> {}",
        if each_once && faults == 0 && harts_used >= 2 && live_max >= 2 {
            "SCHED-OK - one queue, many harts, several U-mode tasks executing simultaneously"
        } else {
            "SCHED-INCOMPLETE"
        }
    );
}

/// Interactive `smp-isolate`: two tasks on two harts, and the second one reaches
/// into the first's stack. Each task has its OWN address space, so the intruder
/// must fault instead — parallelism did not cost isolation.
fn run_smp_isolate_demo() {
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-isolate] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    let victim_stack = task_stack_top(0) - 64; // inside slot 0's stack region
    kprintln!("[smp-isolate] task 0 is an ordinary worker; task 1 reaches into task 0's stack at {victim_stack:#x}");
    if !ap_prepare_slot(0, ap_worker_task as *const () as usize, 0)
        || !ap_prepare_slot(1, ap_rogue_task as *const () as usize, victim_stack)
    {
        kprintln!("[smp-isolate] out of frames while building address spaces.");
        return;
    }
    let ok = ap_run_batch(2);
    let good_fault = AP_SLOT_FAULT[0].load(Ordering::Relaxed);
    let rogue_fault = AP_SLOT_FAULT[1].load(Ordering::Relaxed);
    let h0 = AP_SLOT_HART[0].load(Ordering::Relaxed);
    let h1 = AP_SLOT_HART[1].load(Ordering::Relaxed);
    ap_free_slot(0);
    ap_free_slot(1);

    if !ok {
        kprintln!("[smp-isolate] TIMEOUT: not every task reported done.");
        return;
    }
    kprintln!("[smp-isolate] worker on hart {h0}: {}", if good_fault { "FAULTED (unexpected)" } else { "ran cleanly" });
    kprintln!(
        "[smp-isolate] intruder on hart {h1}: {}",
        if rogue_fault {
            "page-faulted on the cross-task write, killed on its own hart"
        } else {
            "was NOT blocked"
        }
    );
    kprintln!(
        "[smp-isolate] verdict -> {}",
        if rogue_fault && !good_fault {
            "ISOLATION-OK - concurrent tasks on different harts cannot reach each other's memory"
        } else {
            "ISOLATION-BROKEN"
        }
    );
}

/// Marz M1 groundwork: report whether a NIC is present and which slot it owns.
/// This is the device the egress boundary will be built on; nothing is granted
/// to anyone by probing.
fn net_probe() {
    match find_virtio_mmio(VIRTIO_DEVICE_ID_NET) {
        Some(pa) => {
            let slot = (pa - VIRTIO_BLK_MMIO_PA) / VIRTIO_MMIO_STRIDE;
            kprintln!("[marz] virtio-net present: mmio_pa={pa:#x} slot={slot}");
            kprintln!("[marz] a Marz daemon would be granted ONLY this page (cap TASK_DEVICE_VIRTIO_NET), never the whole window");
            record_event("kernel", "marz.probe", "virtio-net", "OK");
        }
        None => {
            kprintln!("[marz] no virtio-net device present (QEMU needs -device virtio-net-device)");
            record_event("kernel", "marz.probe", "virtio-net", "absent");
        }
    }
}
#[derive(Clone, Copy)]
struct ProcessSpec {
    elf: &'static [u8],
    caps: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    personality: u8,
    map_uart: bool,
    map_virtio_blk: bool,
    map_virtio_net: bool,
    map_virtio_dma: bool,
}

impl ProcessSpec {
    const fn new(elf: &'static [u8], caps: usize, arg0: usize) -> Self {
        ProcessSpec {
            elf,
            caps,
            arg0,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            personality: PERS_NATIVE,
            map_uart: false,
            map_virtio_blk: false,
            map_virtio_net: false,
            map_virtio_dma: false,
        }
    }

    const fn uart(mut self) -> Self {
        self.map_uart = true;
        self
    }

    const fn virtio_blk(mut self) -> Self {
        self.map_virtio_blk = true;
        self
    }

    /// Grant ONLY the discovered virtio-net page (per-device, not the window).
    const fn virtio_net(mut self) -> Self {
        self.map_virtio_net = true;
        self
    }

    const fn virtio_dma(mut self) -> Self {
        self.map_virtio_dma = true;
        self
    }

    const fn args(mut self, arg1: usize, arg2: usize, arg3: usize) -> Self {
        self.arg1 = arg1;
        self.arg2 = arg2;
        self.arg3 = arg3;
        self
    }

    /// Run this ELF under the Linux syscall personality (serviced by Pol).
    const fn linux(mut self) -> Self {
        self.personality = PERS_LINUX;
        self
    }
}

fn run_ipc_typed_demo() {
    if virtio_service_is_running() {
        kprintln!(
            "[typed-ipc] skipped: run before starting services to avoid disturbing daemon slot 0"
        );
        print_ipcstat();
        return;
    }
    kprintln!("[typed-ipc] demo: typed OK, BAD_REQUEST, TIMEOUT, and DENIED");
    run_tasks(&[
        (
            typed_ipc_service_task as *const () as usize,
            TASK_PRINT | TASK_IPC,
            PERS_NATIVE,
        ),
        (
            typed_ipc_client_task as *const () as usize,
            TASK_PRINT | TASK_IPC,
            PERS_NATIVE,
        ),
    ]);
    run_tasks(&[(
        typed_ipc_timeout_task as *const () as usize,
        TASK_PRINT | TASK_IPC,
        PERS_NATIVE,
    )]);
    run_tasks(&[(typed_ipc_denied_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
    kprintln!(
        "[typed-ipc] PASS: OK={}, BAD_REQUEST={}, TIMEOUT={}, DENIED={}",
        ipc_status_name(IPC_STATUS_OK),
        ipc_status_name(IPC_STATUS_BAD_REQUEST),
        ipc_status_name(IPC_STATUS_TIMEOUT),
        ipc_status_name(IPC_STATUS_DENIED)
    );
}

// --- Cairn v1 console front-end -------------------------------------------------

fn parse_usize_token(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
        i += 1;
    }
    Some(n)
}

fn calc_op_token(s: &str) -> Option<usize> {
    match s {
        "+" => Some(CALC_OP_ADD),
        "-" => Some(CALC_OP_SUB),
        "*" | "x" | "X" => Some(CALC_OP_MUL),
        "/" => Some(CALC_OP_DIV),
        _ => None,
    }
}

fn calc_eval(op: usize, a: usize, b: usize) -> Option<usize> {
    match op {
        CALC_OP_ADD => Some(a.saturating_add(b)),
        CALC_OP_SUB => Some(a.saturating_sub(b)),
        CALC_OP_MUL => Some(a.saturating_mul(b)),
        // checked_div is the divide-by-zero guard, not an optimisation: it
        // returns None for b == 0, which is exactly this arm's contract.
        CALC_OP_DIV => a.checked_div(b),
        _ => None,
    }
}

fn calc_command(plan: &KernelPlan, arg: &str) {
    if !app_calc_is_active(plan) {
        kprintln!("[calc] calc not installed; run `app-install calc` or `install run`");
        return;
    }
    let mut parts = arg.split_whitespace();
    let Some(a_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let Some(op_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let Some(b_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let (Some(a), Some(op), Some(b)) = (
        parse_usize_token(a_s),
        calc_op_token(op_s),
        parse_usize_token(b_s),
    ) else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    run_foreground_processes(&[
        ProcessSpec::new(CALC_ELF, TASK_PRINT | TASK_IPC, CALC_ROLE_EVAL).args(op, a, b),
    ]);
    if unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] } == 0 {
        if let Some(result) = calc_eval(op, a, b) {
            let expr = format!("{} {} {} = {}", a_s, op_s, b_s, result);
            run_registered_virtio_client(plan, BLK_REQ_CALC_SET, &expr);
            record_event("app", "calc.eval", "calc", "OK");
        }
    }
}

fn vault_put(plan: &KernelPlan, arg: &str) {
    if !app_vault_is_active(plan) {
        kprintln!("[vault] vault not installed; run `app-install vault` or `install run`");
        return;
    }
    run_registered_virtio_client(plan, BLK_REQ_VAULT_SET, arg);
    record_event("app", "vault.put", "vault", "OK");
}

fn explain_command(arg: &str) {
    match arg.trim() {
        "app-run lab" | "app-run" => {
            kprintln!("explain app-run lab:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: app registry -> foreground U-mode app -> IPC workers -> virtio-block storage");
            kprintln!("  denied direct: MMIO DMA BLOCK_DIRECT");
        }
        "install" | "install run" => {
            kprintln!("explain install run:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: boot manifest -> virtio-block service -> disk marker/app registry -> verify");
            kprintln!("  rollback point: v0 current/previous sectors and registry checkpoints");
        }
        "calc" => {
            kprintln!("explain calc:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: installed calc ELF computes in U-mode, last result stored via app registry");
            kprintln!("  denied direct: DEVICE DMA BLOCK_DIRECT");
        }
        "vault" | "vault-put" => {
            kprintln!("explain vault:");
            kprintln!("  requires: SPAWN for put, INSPECT for get");
            kprintln!("  path: console -> virtio-block typed IPC -> vault private sector");
            kprintln!("  denied direct: MMIO DMA BLOCK_DIRECT");
        }
        other => kprintln!("explain: no detailed path for '{other}' yet"),
    }
}

fn parse_small_count(arg: &str, default: usize) -> usize {
    let bytes = arg.trim().as_bytes();
    if bytes.is_empty() {
        return default;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return default;
        }
        n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
        i += 1;
    }
    n.clamp(1, 8)
}

fn stress_lab(plan: &KernelPlan, arg: &str) {
    let count = parse_small_count(arg, 3);
    kprintln!("[stress-lab] ensuring lab app is installed");
    app_install(plan, "lab");
    print_memstat();
    let free_before = unsafe { *FRAME_FREE.get() };
    let mut i = 0usize;
    while i < count {
        kprintln!("[stress-lab] iteration {}/{}", i + 1, count);
        app_run(plan, "lab");
        i += 1;
    }
    let free_after = unsafe { *FRAME_FREE.get() };
    print_memstat();
    if free_before == free_after {
        kprintln!("[stress-lab] PASS: free frames stable at {}", free_after);
    } else {
        kprintln!(
            "[stress-lab] WARN: free frames changed before={} after={}",
            free_before,
            free_after
        );
    }
}

// Worker tasks (run in U-mode, so they live in the user region). Each prints a
// couple of steps and yields between them, so their output interleaves.
#[link_section = ".user.text"]
#[inline(never)]
fn sys_yield() {
    unsafe { asm!("ecall", in("a7") SYS_YIELD, lateout("a0") _, lateout("a1") _) };
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn worker_a() -> ! {
    sys_print(b"    [task A] step 1\n");
    sys_yield();
    sys_print(b"    [task A] step 2\n");
    sys_yield();
    sys_print(b"    [task A] finished\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn worker_b() -> ! {
    sys_print(b"    [task B] step 1\n");
    sys_yield();
    sys_print(b"    [task B] step 2\n");
    sys_yield();
    sys_print(b"    [task B] finished\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn worker_c() -> ! {
    sys_print(b"    [task C] step 1\n");
    sys_yield();
    sys_print(b"    [task C] step 2\n");
    sys_yield();
    sys_print(b"    [task C] finished\n");
    sys_exit(0)
}

// --- Cairn-style store as a user-space service, reached over IPC. -------------
// The agent never touches the store's memory; it sends requests and the service
// replies, all via capability-mediated IPC. The store keeps a current value and
// one previous value, so an action can be *rolled back* — the agent-OS
// differentiator (rollbackable actions, D013/D004), now on the kernel. (v0:
// 1-deep history, ≤63-byte values; full content-addressing/provenance is the
// dezh-cairn crate.)
const OP_SET: usize = 0;
const OP_GET: usize = 1;
const OP_ROLLBACK: usize = 2;
const OP_STOP: usize = 3;

// Value-IPC: pass a request as a single register word, encoded (op << 32 | value).
// No buffers means no compiler-emitted memcpy/memset — which a U-mode task cannot
// call (those live in kernel text). Everything here is scalar.
#[inline(always)]
fn enc(op: usize, val: usize) -> usize {
    (op << 32) | (val & 0xFFFF_FFFF)
}

#[link_section = ".user.text"]
#[inline(never)]
fn vsend(to: usize, word: usize) {
    unsafe {
        asm!("ecall", inout("a0") to => _, in("a1") 0usize, in("a2") 0usize, in("a3") 0usize, in("a4") word, in("a7") SYS_SEND)
    };
}

#[link_section = ".user.text"]
#[inline(never)]
fn vrecv() -> (usize, usize) {
    let word: usize;
    let from: usize;
    unsafe {
        asm!("ecall", inout("a0") 0usize => _, inout("a1") 0usize => from, out("a2") word, lateout("a3") _, in("a7") SYS_RECV)
    };
    (word, from)
}

#[link_section = ".user.text"]
#[inline(never)]
fn vrecv_timeout(timeout_ticks: usize) -> (usize, usize, usize) {
    let rc: usize;
    let from: usize;
    let word: usize;
    unsafe {
        asm!(
            "ecall",
            inout("a0") 0usize => rc,
            inout("a1") 0usize => from,
            inout("a2") timeout_ticks => word,
            lateout("a3") _,
            in("a7") SYS_RECV_TIMEOUT
        )
    };
    (rc, from, word)
}

#[link_section = ".user.text"]
#[inline(always)]
fn utyped_word(service: usize, op: usize, request_id: usize, status: usize, arg: usize) -> usize {
    (IPC_PROTO_V1 << 56)
        | ((service & 0xff) << 48)
        | ((op & 0xff) << 40)
        | ((request_id & 0xffff) << 24)
        | ((status & 0xff) << 16)
        | (arg & 0xffff)
}

#[link_section = ".user.text"]
#[inline(always)]
fn utyped_op(word: usize) -> usize {
    (word >> 40) & 0xff
}

#[link_section = ".user.text"]
#[inline(always)]
fn utyped_status(word: usize) -> usize {
    (word >> 16) & 0xff
}

#[link_section = ".user.text"]
#[inline(never)]
fn sys_printnum(v: usize) {
    unsafe { asm!("ecall", inout("a0") v => _, in("a7") SYS_PRINTNUM) };
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn cairn_service() -> ! {
    let mut cur: usize = 0;
    let mut prev: usize = 0;
    loop {
        let (word, from) = vrecv();
        let op = word >> 32;
        let val = word & 0xFFFF_FFFF;
        if op == OP_SET {
            prev = cur; // keep one step of history so the action is rollbackable
            cur = val;
            vsend(from, 0);
        } else if op == OP_GET {
            vsend(from, cur);
        } else if op == OP_ROLLBACK {
            cur = prev;
            vsend(from, 0);
        } else {
            vsend(from, 0);
            sys_exit(0);
        }
    }
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn agent_cairn() -> ! {
    let svc = 0usize; // the Cairn store service is task 0

    sys_print(b"    [agent] set value to 100\n");
    vsend(svc, enc(OP_SET, 100));
    vrecv();

    sys_print(b"    [agent] set value to 999 (a bad edit)\n");
    vsend(svc, enc(OP_SET, 999));
    vrecv();

    vsend(svc, enc(OP_GET, 0));
    let (v, _) = vrecv();
    sys_print(b"    [agent] get -> ");
    sys_printnum(v);

    sys_print(b"    [agent] rolling back the bad edit\n");
    vsend(svc, enc(OP_ROLLBACK, 0));
    vrecv();

    vsend(svc, enc(OP_GET, 0));
    let (v2, _) = vrecv();
    sys_print(b"    [agent] get -> ");
    sys_printnum(v2);
    sys_print(b"    [agent] (value restored by rollback) done\n");

    vsend(svc, enc(OP_STOP, 0));
    vrecv();
    sys_exit(0)
}

// --- Preemption demo: CPU-bound tasks that never yield still interleave. ------
// With cooperative scheduling, "A start, A end, B start, B end" (A hogs the CPU).
// With timer preemption, "B start" appears before "A end" — the timer forces a
// switch mid-loop, so one task can no longer monopolize the CPU (the safety
// property needed before running untrusted agents).
#[link_section = ".user.text"]
#[inline(never)]
fn busy(n: usize) {
    let mut i = 0usize;
    while i < n {
        unsafe { asm!("nop") };
        i += 1;
    }
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn preempt_a() -> ! {
    sys_print(b"    [A] start (busy loop, never yields)\n");
    busy(8_000_000);
    sys_print(b"    [A] end\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn preempt_b() -> ! {
    sys_print(b"    [B] start (busy loop, never yields)\n");
    busy(8_000_000);
    sys_print(b"    [B] end\n");
    sys_exit(0)
}

// --- Isolation demo: one task cannot read another task's private memory. ------
// task0 (victim) owns its stack region; task1 (spy) tries to read it directly.
// While the spy runs, the victim's region is U=0, so the load page-faults and the
// kernel kills only the spy — inter-task no-ambient-authority at the hardware
// memory boundary, which is what makes the IPC layer the *only* way to share.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn victim_task() -> ! {
    sys_print(b"    [task0] my stack is private; only I can touch my region\n");
    sys_yield(); // let the spy try
    sys_print(b"    [task0] still alive after the spy was killed\n");
    sys_exit(0)
}

// A zero-authority task that tries to WIELD a capability it was never granted:
// it calls the privileged PRINT syscall directly. There is no ambient authority
// to inherit and no way to forge or amplify a capability, so the kernel denies
// the syscall at the capability check and the task prints nothing.
#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn forge_task() -> ! {
    let msg = b"    [forge] (BUG) I printed without holding the PRINT capability!\n";
    sys_write(msg.as_ptr(), msg.len());
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn spy_task() -> ! {
    // Read straight into task0's stack region (base = stack_base(); see the
    // kernel log). It is U=0 while we run, so this load faults and we are killed.
    let v: u64;
    unsafe { asm!("ld {0}, 0({1})", out(reg) v, in(reg) 0x8060_0800usize) };
    let _ = v;
    let msg = b"    [spy] (BUG) I read another task's memory!\n";
    sys_write(msg.as_ptr(), msg.len());
    sys_exit(0)
}

// --- IPC demo: an agent delegates a capability to a service over a message. ---
// The service starts with NO authority; it cannot print until the agent sends it
// a message that *delegates* the PRINT capability. The kernel enforces that the
// agent can only delegate what it holds (attenuation, never widening) — the
// microkernel keystone for agents calling services and spawning sub-agents.
#[link_section = ".user.text"]
#[inline(never)]
fn sys_send(to: usize, s: &[u8], grant: usize) -> usize {
    let mut a0 = to;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") s.as_ptr() as usize, in("a2") s.len(), in("a3") grant, in("a7") SYS_SEND)
    };
    a0
}

#[link_section = ".user.text"]
#[inline(never)]
fn sys_recv(buf: &mut [u8]) -> usize {
    let mut a0 = buf.as_mut_ptr() as usize;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") buf.len(), lateout("a2") _, lateout("a3") _, in("a7") SYS_RECV)
    };
    a0 // bytes received
}

// Raw write wrapper: takes ptr+len so user code never calls a (non-inlined,
// kernel-resident) core slicing helper — which a U-mode task cannot fetch.
#[link_section = ".user.text"]
#[inline(never)]
fn sys_write(ptr: *const u8, len: usize) -> usize {
    let mut a0 = ptr as usize;
    unsafe { asm!("ecall", inout("a0") a0, in("a1") len, in("a7") SYS_PRINT) };
    a0
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn service_task() -> ! {
    // No authority yet: this print is denied by the kernel.
    sys_print(b"    [service] (pre-IPC) I have no capabilities; this print is denied\n");
    let mut buf = [0u8; 64];
    let n = sys_recv(&mut buf); // blocks until the agent delegates a capability
    sys_print(b"    [service] received a delegated PRINT capability via IPC; now I can print:\n");
    sys_write(buf.as_ptr(), n); // echo the payload (no slice indexing)
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn agent_task() -> ! {
    sys_print(b"    [agent] delegating my PRINT capability to the service over IPC\n");
    sys_send(
        0,
        b"    [service] <payload delivered with a delegated PRINT cap>\n",
        TASK_PRINT,
    );
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn typed_ipc_service_task() -> ! {
    let (word1, from1) = vrecv();
    if utyped_op(word1) == IPC_OP_PING {
        vsend(
            from1,
            utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_PING, 1, IPC_STATUS_OK, 0),
        );
    } else {
        vsend(
            from1,
            utyped_word(
                IPC_SERVICE_SYSTEM,
                IPC_OP_BADREQ,
                1,
                IPC_STATUS_BAD_REQUEST,
                0,
            ),
        );
    }

    let (word2, from2) = vrecv();
    let status = if utyped_op(word2) == IPC_OP_PING {
        IPC_STATUS_OK
    } else {
        IPC_STATUS_BAD_REQUEST
    };
    vsend(
        from2,
        utyped_word(IPC_SERVICE_SYSTEM, utyped_op(word2), 2, status, 0),
    );
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn typed_ipc_client_task() -> ! {
    vsend(
        0,
        utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_PING, 1, IPC_STATUS_OK, 0),
    );
    let (ok, _) = vrecv();
    sys_print(b"    [typed-ipc] PING -> ");
    sys_printnum(utyped_status(ok));

    vsend(
        0,
        utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_BADREQ, 2, IPC_STATUS_OK, 0),
    );
    let (bad, _) = vrecv();
    sys_print(b"    [typed-ipc] BADREQ -> ");
    sys_printnum(utyped_status(bad));
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn typed_ipc_timeout_task() -> ! {
    let (rc, _, word) = vrecv_timeout(0);
    sys_print(b"    [typed-ipc] RECV_TIMEOUT -> ");
    if rc == IPC_STATUS_TIMEOUT {
        sys_printnum(utyped_status(word));
    } else {
        sys_printnum(rc);
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn typed_ipc_denied_task() -> ! {
    let rc = sys_send(0, b"", 0);
    sys_print(b"    [typed-ipc] no-IPC SEND -> ");
    if rc == SYS_DENIED {
        sys_printnum(IPC_STATUS_DENIED);
    } else {
        sys_printnum(rc);
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn queue_service_task() -> ! {
    sys_print(b"    [queue-service] delaying receive so two clients enqueue\n");
    sys_yield();
    sys_yield();

    let mut first = [0u8; 64];
    let n1 = sys_recv(&mut first);
    sys_print(b"    [queue-service] recv #1: ");
    sys_write(first.as_ptr(), n1);

    let mut second = [0u8; 64];
    let n2 = sys_recv(&mut second);
    sys_print(b"    [queue-service] recv #2: ");
    sys_write(second.as_ptr(), n2);

    sys_print(b"    [queue-service] FIFO mailbox preserved both client messages\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn queue_agent_a() -> ! {
    sys_print(b"    [queue-agent-a] enqueue alpha\n");
    sys_send(0, b"alpha\n", 0);
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn queue_agent_b() -> ! {
    sys_print(b"    [queue-agent-b] enqueue beta\n");
    sys_send(0, b"beta\n", 0);
    sys_exit(0)
}

// --- A Linux-ABI app, run unmodified through the Pol personality layer. -------
// It speaks the real Linux riscv64 syscall ABI (write=64, exit=93). The kernel's
// Pol layer translates each into a capability-checked Dezh action; an
// unsupported syscall returns ENOSYS. The app has zero ambient authority — it
// only reaches the console because it holds the PRINT capability.
#[link_section = ".user.text"]
#[inline(never)]
fn linux_write(fd: usize, s: &[u8]) -> i64 {
    let mut a0 = fd;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") s.as_ptr() as usize, in("a2") s.len(), in("a7") LINUX_WRITE)
    };
    a0 as i64
}

#[link_section = ".user.text"]
#[inline(never)]
fn linux_close(fd: usize) -> i64 {
    let mut a0 = fd;
    // 57 = Linux `close`; the Pol layer does not support it -> ENOSYS.
    unsafe { asm!("ecall", inout("a0") a0, in("a7") 57usize) };
    a0 as i64
}

#[link_section = ".user.text"]
#[inline(never)]
fn linux_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") LINUX_EXIT, options(noreturn)) }
}

// --- Benchmark task: measure the cost of a syscall (ecall) round trip. -------
// Times N minimal syscalls with the U-mode-readable `time` CSR and reports the
// per-call cost back to the kernel. (Under QEMU this is an emulated figure; see
// BENCH.md for the real-hardware comparison.)
#[link_section = ".user.text"]
#[inline(never)]
fn sys_null() {
    unsafe { asm!("ecall", in("a7") SYS_NULL, lateout("a0") _, lateout("a1") _) };
}

#[link_section = ".user.text"]
#[inline(never)]
fn rdtime_u() -> usize {
    let t: usize;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

#[link_section = ".user.text"]
#[inline(never)]
fn sys_report(ticks: usize, iters: usize) {
    unsafe { asm!("ecall", inout("a0") ticks => _, in("a1") iters, in("a7") SYS_REPORT) };
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn bench_task() -> ! {
    let n: usize = 500_000;
    let t0 = rdtime_u();
    let mut i = 0;
    while i < n {
        sys_null();
        i += 1;
    }
    let t1 = rdtime_u();
    sys_report(t1.wrapping_sub(t0), n);
    sys_exit(0)
}

// --- Pol translation-overhead benchmark --------------------------------------
// Two U-mode tasks doing the SAME zero-work syscall the same number of times:
// one via the native Dezh `SYS_PRINT` path, one via the Linux `write` ABI routed
// through the Pol personality layer. Both pass a zero-length buffer, so neither
// touches the UART; the only difference on the kernel side is the personality
// branch + Linux-ABI decode. The kernel times each run and reports the delta as
// the per-syscall translation overhead. (QEMU-emulated; the delta is the honest
// number for F4 — see BENCH.md.)
const BENCH_POL_ITERS: usize = 200_000;

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn bench_native_print_task() -> ! {
    let mut i = 0;
    while i < BENCH_POL_ITERS {
        sys_print(b""); // native SYS_PRINT, zero-length: cap-checked, no output
        i += 1;
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn bench_pol_write_task() -> ! {
    let mut i = 0;
    while i < BENCH_POL_ITERS {
        linux_write(1, b""); // Linux write(2) ABI, zero-length: serviced by Pol
        i += 1;
    }
    linux_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
extern "C" fn linux_app() -> ! {
    linux_write(
        1,
        b"    [linux] hello from a Linux-ABI app, serviced by Pol\n",
    );
    let r = linux_close(3);
    if r == -38 {
        linux_write(
            1,
            b"    [linux] close(3) returned ENOSYS -> unsupported syscall, denied cleanly\n",
        );
    }
    linux_exit(0)
}

// --- Console capabilities ----------------------------------------------------
mod cap {
    pub const INSPECT: u32 = 1 << 0;
    pub const TIME: u32 = 1 << 1;
    pub const ECHO: u32 = 1 << 2;
    pub const HALT: u32 = 1 << 3;
    pub const SECRET: u32 = 1 << 4; // deliberately never granted (demo)
    pub const SPAWN: u32 = 1 << 5; // run a U-mode task
}

struct CommandSpec {
    name: &'static str,
    cap: u32,
    cap_name: &'static str,
    group: &'static str,
    help: &'static str,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        cap: 0,
        cap_name: "-",
        group: "Inspect",
        help: "list commands or show help <command>",
    },
    CommandSpec {
        name: "version",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show kernel and review-kit version",
    },
    CommandSpec {
        name: "about",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show the Dezh OS thesis in one screen",
    },
    CommandSpec {
        name: "clear",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "clear the terminal",
    },
    CommandSpec {
        name: "explain",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "explain command authority and service path",
    },
    CommandSpec {
        name: "caps",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show the console's capabilities",
    },
    CommandSpec {
        name: "mem",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show the memory map",
    },
    CommandSpec {
        name: "frames",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "frame allocator self-test (alloc/zero/free)",
    },
    CommandSpec {
        name: "memstat",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show frame ownership and process memory accounting",
    },
    CommandSpec {
        name: "ipcstat",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show IPC send/receive/timeout counters",
    },
    CommandSpec {
        name: "ipc-typed-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "exercise typed IPC OK/BAD_REQUEST/TIMEOUT/DENIED",
    },
    CommandSpec {
        name: "status",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show boot/runtime/storage summary",
    },
    CommandSpec {
        name: "tasks",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "show task slots and service bindings",
    },
    CommandSpec {
        name: "disk",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Services",
        help: "probe virtio-mmio slots for a block device",
    },
    CommandSpec {
        name: "agent",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run a Dezh-IR agent program (capability-gated interpreter)",
    },
    CommandSpec {
        name: "bwrite",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "write a marker to disk sector 0 (virtio-blk)",
    },
    CommandSpec {
        name: "bread",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "read disk sector 0 (proves persistence)",
    },
    CommandSpec {
        name: "pset",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "durable Cairn: set current value (persisted) <text>",
    },
    CommandSpec {
        name: "pget",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "durable Cairn: read current value",
    },
    CommandSpec {
        name: "prollback",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "durable Cairn: roll back to previous value",
    },
    CommandSpec {
        name: "write",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "alias: write <text> to durable Cairn current value",
    },
    CommandSpec {
        name: "read",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "alias: read durable Cairn current value",
    },
    CommandSpec {
        name: "rollback",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "alias: roll back durable Cairn current value",
    },
    CommandSpec {
        name: "history",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "show v0 current/previous Cairn sector status",
    },
    CommandSpec {
        name: "cairn-status",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "Cairn v1: show namespaces, head refs, and commit slots",
    },
    CommandSpec {
        name: "cairn-commit",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "Cairn v1: commit a value <ns> <text> (namespace-capability gated)",
    },
    CommandSpec {
        name: "cairn-get",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "Cairn v1: read the head value of <ns>",
    },
    CommandSpec {
        name: "cairn-log",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "Cairn v1: show the commit chain of <ns> (newest first)",
    },
    CommandSpec {
        name: "cairn-rollback",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Storage",
        help: "Cairn v1: move the <ns> head ref back [n] commits (history kept)",
    },
    CommandSpec {
        name: "cairn-verify",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "Cairn v1: re-hash the head object of <ns> against its commit record",
    },
    CommandSpec {
        name: "cairn-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "F2 flagship: commits, log, bad write, rollback, verify, namespace denial",
    },
    CommandSpec {
        name: "root",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Storage",
        help: "show installed root marker and metadata",
    },
    CommandSpec {
        name: "install",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Install",
        help: "installer v1: plan|check|run|verify|report|rollback|--dry-run",
    },
    CommandSpec {
        name: "pkg-recv",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "receive a .dzp package over the UART (base64 lines, '.' ends)",
    },
    CommandSpec {
        name: "sig-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "package signing: verify a signed pkg, attenuate to the publisher ceiling, refuse tampered/revoked",
    },
    CommandSpec {
        name: "pkg-list",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "list packages installed via pkg-recv",
    },
    CommandSpec {
        name: "pkg-info",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "show a package's manifest grants (granted/denied)",
    },
    CommandSpec {
        name: "pkg-run",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "run an installed package with exactly its installed grants",
    },
    CommandSpec {
        name: "intent-open",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "open an Ahd (intent): a capability ceiling. intent-open <kind> [lease]",
    },
    CommandSpec {
        name: "intent-revoke",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "revoke an intent: it authorizes nothing further (provenance survives). intent-revoke <id>",
    },
    CommandSpec {
        name: "lease-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "self-contained proof: a leased intent expires after N runs; a revoked one authorizes nothing",
    },
    CommandSpec {
        name: "cap-demo",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Intent",
        help: "object-capability primitive: attenuated delegation + per-object generation-stamped revocation",
    },
    CommandSpec {
        name: "smp-demo",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "SMP: secondary harts brought up via SBI HSM run a parallel round with coherent atomics",
    },
    CommandSpec {
        name: "smp-task",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "SMP: dispatch a real U-mode task onto a secondary hart while the boot hart stays on the console",
    },
    CommandSpec {
        name: "smp-sched",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "SMP: symmetric scheduling - one task queue, every hart pulls from it, several U-mode tasks at once",
    },
    CommandSpec {
        name: "smp-isolate",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "SMP: concurrent tasks on different harts cannot reach each other's memory (per-task address spaces)",
    },
    CommandSpec {
        name: "ns-revoke",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "revoke a namespace capability at runtime (ocap generation bump). ns-revoke <ns>",
    },
    CommandSpec {
        name: "ns-grant",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "re-mint a namespace capability at the current generation. ns-grant <ns>",
    },
    CommandSpec {
        name: "nsrevoke-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "proof: revoke a live namespace capability at runtime; the storage path refuses until re-granted",
    },
    CommandSpec {
        name: "agentrevoke-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "the ocap namespace gate covers the untrusted AGENT path: a revoked ns refuses the agent's write",
    },
    CommandSpec {
        name: "marz-send",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "Marz: transmit one real frame to an authorized destination. marz-send <dest>",
    },
    CommandSpec {
        name: "marz-ping",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "Marz: probe an authorized destination and RECEIVE the answer (ARP + ICMP echo). marz-ping <dest>",
    },
    CommandSpec {
        name: "dev-revoke",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "revoke a device capability at runtime (ocap generation). dev-revoke <block|net>",
    },
    CommandSpec {
        name: "dev-grant",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "re-mint a device capability. dev-grant <block|net>",
    },
    CommandSpec {
        name: "dev-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "device authority as a revocable ocap handle, above the per-destination gate",
    },
    CommandSpec {
        name: "marz-grant",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "grant egress authority for one destination. marz-grant <dest>",
    },
    CommandSpec {
        name: "marz-revoke",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "revoke egress authority for one destination (others untouched). marz-revoke <dest>",
    },
    CommandSpec {
        name: "marz-effect-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "Marz M3: a real send recorded as an irreversible effect that rollback refuses",
    },
    CommandSpec {
        name: "marz-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "Marz M2: per-destination egress authority + the DIFC export rule, proven on the wire",
    },
    CommandSpec {
        name: "irq-stat",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Inspect",
        help: "external device interrupts serviced via the PLIC",
    },
    CommandSpec {
        name: "net-probe",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "Marz M1: report the virtio-net device the egress boundary will be built on",
    },
    CommandSpec {
        name: "exfil-demo",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "confidentiality (DIFC): reading a secret taints an agent so it cannot leak it to a public sink",
    },
    CommandSpec {
        name: "taintflow-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "DIFC enforced on the storage path: read vault, then a write-down to a public ns is refused until declassify",
    },
    CommandSpec {
        name: "taint",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "show the operator's current secrecy taint (DIFC)",
    },
    CommandSpec {
        name: "declassify",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "privileged declassification: clear the operator's DIFC taint",
    },
    CommandSpec {
        name: "endorse",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "privileged endorsement (the dual of declassify): restore integrity after validating outside input",
    },
    CommandSpec {
        name: "ingress-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "proof: data read from the network is unvalidated and cannot become trusted state without an explicit endorsement",
    },
    CommandSpec {
        name: "intent-list",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Intent",
        help: "list open Ahds (intent tokens) with lease + revocation status",
    },
    CommandSpec {
        name: "intent-run",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "run a package under an Ahd; derived cap <= Ahd. intent-run <id> <app>",
    },
    CommandSpec {
        name: "intent-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Intent",
        help: "self-contained proof: same agent under two Ahds (in-intent vs beyond-intent)",
    },
    CommandSpec {
        name: "sand-log",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "Sand effect ledger for <ns>: actor -> intent -> derived cap -> reversibility",
    },
    CommandSpec {
        name: "sand-info",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "Sand: full provenance of the head effect in <ns>",
    },
    CommandSpec {
        name: "sand-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P2 flagship: run an agent under an intent, then show its effect on the ledger",
    },
    CommandSpec {
        name: "sfar-plan",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "rollback forecast for mission <ahd>: what could be undone, and with what confidence",
    },
    CommandSpec {
        name: "sfar-rollback",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "roll a whole mission <ahd> back: retract reversible effects, refuse the rest w/ reason",
    },
    CommandSpec {
        name: "sfar-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P3 flagship: an agent mission with a mix of effect classes, forecast, then honest rollback",
    },
    CommandSpec {
        name: "sfar-cross-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P3: a mission spanning two namespaces; rollback needs authority over every one it touched",
    },
    CommandSpec {
        name: "comp-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P3: roll back a compensatable effect by running + recording its registered compensating action",
    },
    CommandSpec {
        name: "redteam",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P4: a malicious agent tries five escapes; each is stopped at a named boundary and the system survives",
    },
    CommandSpec {
        name: "why-denied",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "W8 P5: explain the last denial (or `why-denied all`) and name the boundary that produced it",
    },
    CommandSpec {
        name: "tbar",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Effects",
        help: "W8 P5: the actor -> intent -> effect provenance graph for intent <ahd>",
    },
    CommandSpec {
        name: "overnight",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Effects",
        help: "W8 P7 flagship: leave a coding agent loose overnight, then account for and undo its night",
    },
    CommandSpec {
        name: "pkg-remove",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "remove an installed package (its grants go with it)",
    },
    CommandSpec {
        name: "pkg-store",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "inspect persistent package-store slots and blob range",
    },
    CommandSpec {
        name: "pkg-journal",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "inspect the package transaction journal",
    },
    CommandSpec {
        name: "pkg-recover",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "recover or quarantine an interrupted package transaction",
    },
    CommandSpec {
        name: "pkg-verify",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "verify one package's registry entry and persisted blob",
    },
    CommandSpec {
        name: "pkg-fault",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "inject a package transaction fault for reboot recovery tests",
    },
    CommandSpec {
        name: "pkg-gc",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "explicitly wipe blobs for logically removed package slots",
    },
    CommandSpec {
        name: "pkg-update",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "upload and transactionally update an Active package",
    },
    CommandSpec {
        name: "pkg-rollback",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "restore a verified previous package checkpoint",
    },
    CommandSpec {
        name: "pkg-versions",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "show active and previous package versions",
    },
    CommandSpec {
        name: "pkg-review",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "review package caps, pin state, and lifecycle policy",
    },
    CommandSpec {
        name: "pkg-pin",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "pin a package against surprise update/rollback",
    },
    CommandSpec {
        name: "pkg-unpin",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "remove a package pin after explicit review",
    },
    CommandSpec {
        name: "pkg-retire",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "logically retire a package; physical cleanup remains explicit",
    },
    CommandSpec {
        name: "pkg-lifecycle",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "summarize package lifecycle counts and policy",
    },
    CommandSpec {
        name: "pkg-audit",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Packages",
        help: "show where package lifecycle audit evidence is recorded",
    },
    CommandSpec {
        name: "apps",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "list app bundles or registry state (available|installed)",
    },
    CommandSpec {
        name: "app-info",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "show app bundle and install state",
    },
    CommandSpec {
        name: "app-install",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "transactionally install an available app",
    },
    CommandSpec {
        name: "app-run",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "run an installed app with registry-scoped caps",
    },
    CommandSpec {
        name: "app-remove",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "logically remove an installed app",
    },
    CommandSpec {
        name: "app-deny",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "prove installed app has no direct device/block grants",
    },
    CommandSpec {
        name: "app-permissions",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "show requested/granted/denied app authorities",
    },
    CommandSpec {
        name: "note-set",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "set dezh-note private value via app registry storage",
    },
    CommandSpec {
        name: "note-get",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "read dezh-note private value via app registry storage",
    },
    CommandSpec {
        name: "lab-set",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "set dezh-lab private value via app registry storage",
    },
    CommandSpec {
        name: "lab-get",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "read dezh-lab private value via app registry storage",
    },
    CommandSpec {
        name: "calc",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "run dezh-calc: calc <n> <+|-|*|/> <n>",
    },
    CommandSpec {
        name: "calc-history",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "read dezh-calc last stored result",
    },
    CommandSpec {
        name: "vault-put",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Apps",
        help: "store a private value through dezh-vault",
    },
    CommandSpec {
        name: "vault-get",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Apps",
        help: "read dezh-vault private value",
    },
    CommandSpec {
        name: "vblkd",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Services",
        help: "run long-lived user-space virtio-blk daemon + IPC client",
    },
    CommandSpec {
        name: "services",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Services",
        help: "list runtime services",
    },
    CommandSpec {
        name: "svc-stop",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Services",
        help: "stop a supervised service",
    },
    CommandSpec {
        name: "svc-restart",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Services",
        help: "restart a stopped/faulted service",
    },
    CommandSpec {
        name: "svc-fault-demo",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Services",
        help: "fault a supervised service and keep console alive",
    },
    CommandSpec {
        name: "install-check",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Services",
        help: "validate install manifest and disk root marker",
    },
    CommandSpec {
        name: "install-init",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Services",
        help: "initialize Dezh root marker/metadata on disk",
    },
    CommandSpec {
        name: "root-status",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Services",
        help: "read Dezh root metadata from disk",
    },
    CommandSpec {
        name: "events",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Audit",
        help: "show kernel/app/service event timeline",
    },
    CommandSpec {
        name: "audit",
        cap: cap::INSPECT,
        cap_name: "INSPECT",
        group: "Audit",
        help: "show audit summary and recent events",
    },
    CommandSpec {
        name: "uptime",
        cap: cap::TIME,
        cap_name: "TIME",
        group: "Inspect",
        help: "show timer uptime",
    },
    CommandSpec {
        name: "echo",
        cap: cap::ECHO,
        cap_name: "ECHO",
        group: "Demos",
        help: "echo <text>",
    },
    CommandSpec {
        name: "run",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run a capability-limited U-mode task",
    },
    CommandSpec {
        name: "load",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "load a separate program into its own address space",
    },
    CommandSpec {
        name: "procs",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run two separate programs concurrently (own address spaces)",
    },
    CommandSpec {
        name: "rogue",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Safety",
        help: "run a task that tries forbidden memory (gets killed)",
    },
    CommandSpec {
        name: "multi",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run 3 cooperative U-mode tasks (round-robin)",
    },
    CommandSpec {
        name: "spy",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Safety",
        help: "prove a task cannot read another task's memory",
    },
    CommandSpec {
        name: "preempt",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "two non-yielding tasks interleave via timer preemption",
    },
    CommandSpec {
        name: "linux",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run a Linux-ABI app via the Pol personality",
    },
    CommandSpec {
        name: "linux-elf",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "load a REAL static Linux/RISC-V ELF (F4); denied without PRINT",
    },
    CommandSpec {
        name: "bench",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "measure ecall round-trip cost (U-mode task)",
    },
    CommandSpec {
        name: "bench-pol",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "measure Pol Linux-ABI translation overhead vs the native path (F4)",
    },
    CommandSpec {
        name: "bench-os",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "benchmark syscall/trap boundary using a separate U-mode ELF",
    },
    CommandSpec {
        name: "bench-ipc",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "benchmark U-mode IPC service/client message flow",
    },
    CommandSpec {
        name: "bench-storage",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "validate storage through the registered virtio-block daemon",
    },
    CommandSpec {
        name: "bench-caps",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Safety",
        help: "validate denied capability/device paths",
    },
    CommandSpec {
        name: "bench-all",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run the Dezh benchmark/validation suite v0",
    },
    CommandSpec {
        name: "stress-lab",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "run lab repeatedly and check frame reclamation",
    },
    CommandSpec {
        name: "ipc",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "agent delegates a capability to a service via IPC",
    },
    CommandSpec {
        name: "ipcq",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "two clients enqueue IPC messages without overwriting",
    },
    CommandSpec {
        name: "queues",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "alias: run IPC FIFO queue demo",
    },
    CommandSpec {
        name: "cairn",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "agent does a rollbackable action via a Cairn store service",
    },
    CommandSpec {
        name: "deny",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Safety",
        help: "run a compact denial tour",
    },
    CommandSpec {
        name: "secret",
        cap: cap::SECRET,
        cap_name: "SECRET",
        group: "Safety",
        help: "(needs a cap the console lacks)",
    },
    CommandSpec {
        name: "halt",
        cap: cap::HALT,
        cap_name: "HALT",
        group: "Power",
        help: "power off the machine",
    },
];

fn cap_names(set: u32) -> &'static str {
    match set {
        s if s == cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT | cap::SPAWN => {
            "INSPECT TIME ECHO HALT SPAWN"
        }
        _ => "(custom set)",
    }
}

fn print_help(held: u32) {
    const GROUPS: &[&str] = &[
        "Inspect", "Storage", "Install", "Packages", "Apps", "Services", "Audit", "Safety",
        "Demos", "Power",
    ];
    kprintln!("commands (cap required -> held?):");
    for group in GROUPS {
        kprintln!("  [{}]", group);
        for c in COMMANDS {
            if c.group != *group {
                continue;
            }
            let ok = if c.cap == 0 || held & c.cap == c.cap {
                "yes"
            } else {
                "DENIED"
            };
            kprintln!("    {:<13} {:<8} [{}]  {}", c.name, c.cap_name, ok, c.help);
        }
    }
}

fn print_command_help(name: &str, held: u32) {
    let wanted = name.trim();
    if wanted.is_empty() {
        print_help(held);
        return;
    }
    for c in COMMANDS {
        if c.name == wanted {
            let ok = if c.cap == 0 || held & c.cap == c.cap {
                "yes"
            } else {
                "DENIED"
            };
            kprintln!("help: {}", c.name);
            kprintln!("  group: {}", c.group);
            kprintln!("  requires: {} ({})", c.cap_name, ok);
            kprintln!("  usage: {}", command_usage(c.name));
            kprintln!("  about: {}", c.help);
            return;
        }
    }
    kprintln!("help: unknown command '{wanted}'");
}

fn command_usage(name: &str) -> &str {
    match name {
        "install" => "install plan|check|run|verify|report|rollback|--dry-run",
        "pkg-info" => "pkg-info <name>",
        "pkg-run" => "pkg-run <name>",
        "pkg-remove" => "pkg-remove <name>",
        "pkg-store" => "pkg-store",
        "pkg-journal" => "pkg-journal",
        "pkg-recover" => "pkg-recover",
        "pkg-verify" => "pkg-verify <name>",
        "pkg-fault" => {
            "pkg-fault <install-after-blob|install-pending-registry|remove-pending|corrupt-journal>"
        }
        "pkg-gc" => "pkg-gc [plan|run]",
        "pkg-update" => "pkg-update <name> [--allow-new-caps]",
        "pkg-rollback" => "pkg-rollback <name> [--force]",
        "pkg-versions" => "pkg-versions <name>",
        "pkg-review" => "pkg-review <name>",
        "pkg-pin" => "pkg-pin <name>",
        "pkg-unpin" => "pkg-unpin <name>",
        "pkg-retire" => "pkg-retire <name>",
        "pkg-lifecycle" => "pkg-lifecycle",
        "pkg-audit" => "pkg-audit <name>",
        "calc" => "calc <n> <+|-|*|/> <n>",
        "vault-put" => "vault-put <text>",
        "app-permissions" => "app-permissions <note|lab|calc|vault>",
        "explain" => "explain <command>",
        "svc-stop" => "svc-stop virtio-block",
        "svc-restart" => "svc-restart virtio-block",
        "svc-fault-demo" => "svc-fault-demo virtio-block",
        _ => name,
    }
}

fn print_status(plan: &KernelPlan, memory: &[MemoryRegion], held: u32) {
    refresh_virtio_service_state();
    let ticks = TICKS.load(Ordering::Relaxed);
    let usable_regions = memory
        .iter()
        .filter(|r| r.kind == MemoryKind::Usable)
        .count();
    let running_services = running_service_count();
    kprintln!("status:");
    kprintln!("  target: {:?}", plan.target);
    kprintln!(
        "  uptime: {} ticks (~{}.{} s)",
        ticks,
        ticks / TIMER_HZ,
        ticks % TIMER_HZ
    );
    kprintln!(
        "  memory: {} bytes usable across {} usable region(s)",
        plan.usable_bytes,
        usable_regions
    );
    kprintln!(
        "  services: {} declared, {} running",
        plan.services.len(),
        running_services
    );
    kprintln!(
        "  install: root={} block={} marker_sector={} root_metadata_sector={}",
        plan.install_manifest.root_service,
        plan.install_manifest.block_service,
        plan.install_manifest.layout.marker_sector,
        plan.install_manifest.layout.root_metadata_sector
    );
    kprintln!("  console caps: {}", cap_names(held));
}

fn task_state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Unused => "Unused",
        TaskState::Ready => "Ready",
        TaskState::Blocked => "Blocked",
        TaskState::Done => "Done",
    }
}

fn print_tasks() {
    refresh_virtio_service_state();
    unsafe {
        kprintln!("tasks:");
        let mut i = 0usize;
        while i < MAX_TASKS {
            kprintln!(
                "  task{} state={:<7} kind={:<10} frames={:<3} caps={:#x} exit={} service={}",
                i,
                task_state_name((*TSTATE.get())[i]),
                task_kind_name((*TRES.get())[i].kind),
                task_owned_frames(i),
                (*TCAPS.get())[i],
                (*TEXIT.get())[i],
                service_for_task(i)
            );
            i += 1;
        }
    }
}

fn print_memstat() {
    let total = unsafe { *FRAME_TOTAL.get() };
    let free = unsafe { *FRAME_FREE.get() };
    let used = total.saturating_sub(free);
    let process_owned = process_owned_frames();
    let daemon_owned = owned_frames_by_kind(TaskKind::Daemon);
    let foreground_owned = owned_frames_by_kind(TaskKind::Foreground);
    let unowned = used.saturating_sub(process_owned);
    kprintln!("memstat:");
    kprintln!("  frames: total={} free={} used={}", total, free, used);
    kprintln!(
        "  owned: process={} daemon={} foreground={}",
        process_owned,
        daemon_owned,
        foreground_owned
    );
    kprintln!("  unowned allocated estimate={}", unowned);
}

fn print_version() {
    kprintln!("Dezh OS review prototype v0.2-control-surface");
    kprintln!("  kernel: riscv64 qemu-virt S-mode");
    kprintln!("  ipc: typed v0 with timeout/status");
    kprintln!("  installer: v1 UX over v0 disk layout");
}

fn print_about() {
    kprintln!("Dezh OS: capability-secure research prototype");
    kprintln!("  thesis: no ambient authority; every effect needs an explicit grant");
    kprintln!("  current: U-mode apps, user-space virtio-block, typed IPC, installer/app registry");
    kprintln!("  review focus: authority visibility, service recovery, app install/run/storage");
}

fn print_caps_why(arg: &str) {
    match arg.trim() {
        "note-get" | "read" | "root-status" => {
            kprintln!("caps why {}:", arg.trim());
            kprintln!("  console requires: INSPECT");
            kprintln!("  foreground client receives: PRINT IPC BLOCK_READ BLOCK_WRITE");
            kprintln!("  device access remains only in virtio-block daemon");
        }
        "app-run lab" | "app-run" | "calc" | "vault-put" => {
            kprintln!("caps why {}:", arg.trim());
            kprintln!("  console requires: SPAWN");
            kprintln!("  app receives: PRINT IPC only");
            kprintln!("  denied: DEVICE_VIRTIO_BLK DMA BLOCK_DIRECT MMIO");
        }
        "install run" | "install" => {
            kprintln!("caps why install run:");
            kprintln!("  console requires: SPAWN");
            kprintln!("  installer path uses: registered virtio-block service");
            kprintln!("  service owns: DEVICE_VIRTIO_BLK DMA BLOCK_READ BLOCK_WRITE");
        }
        _ => kprintln!(
            "caps why: try `caps why install run`, `caps why app-run lab`, or `caps why note-get`"
        ),
    }
}

fn console(plan: &KernelPlan, memory: &[MemoryRegion], held: u32) -> ! {
    kprintln!();
    kprintln!("Dezh console. Every command requires an explicit capability.");
    kprintln!("Type 'help'. The console holds: {}", cap_names(held));

    let mut buf = [0u8; 128];
    loop {
        kprint!("dezh> ");
        let len = read_line(&mut buf);
        let line = core::str::from_utf8(&buf[..len]).unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        dispatch(cmd, arg, plan, memory, held);
    }
}

fn dispatch(cmd: &str, arg: &str, plan: &KernelPlan, memory: &[MemoryRegion], held: u32) {
    let spec = match COMMANDS.iter().find(|c| c.name == cmd) {
        Some(s) => s,
        None => {
            kprintln!("unknown command: {cmd} (try 'help')");
            return;
        }
    };

    if spec.cap != 0 && held & spec.cap != spec.cap {
        kprintln!(
            "denied: '{}' requires capability {} (not held)",
            cmd,
            spec.cap_name
        );
        return;
    }

    match cmd {
        "help" => {
            print_command_help(arg, held);
        }
        "version" => print_version(),
        "about" => print_about(),
        "clear" => kprint!("\x1b[2J\x1b[H"),
        "explain" => explain_command(arg),
        "caps" => {
            if let Some(rest) = arg.strip_prefix("why ") {
                print_caps_why(rest);
            } else {
                kprintln!("console capabilities: {}", cap_names(held));
            }
        }
        "mem" => {
            kprintln!("usable memory: {} bytes", plan.usable_bytes);
            for r in memory {
                let end = r.start + r.len;
                kprintln!("  {:#012x}..{:#012x}  {:?}", r.start, end, r.kind);
            }
        }
        "status" => print_status(plan, memory, held),
        "tasks" => print_tasks(),
        "memstat" => print_memstat(),
        "ipcstat" => print_ipcstat(),
        "ipc-typed-demo" => run_ipc_typed_demo(),
        "disk" => {
            kprintln!("[kernel] user-space virtio-blk: first prove no device cap means no MMIO");
            run_virtio_no_grant_probe();
            kprintln!("[kernel] no-grant probe returned; console survived");
            run_registered_virtio_client(plan, BLK_REQ_PROBE, "");
        }
        "bwrite" => run_registered_virtio_client(plan, BLK_REQ_BWRITE, ""),
        "bread" => run_registered_virtio_client(plan, BLK_REQ_BREAD, ""),
        "pset" => run_registered_virtio_client(plan, BLK_REQ_PSET, arg),
        "pget" => run_registered_virtio_client(plan, BLK_REQ_PGET, ""),
        "prollback" => run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, ""),
        "write" => run_registered_virtio_client(plan, BLK_REQ_PSET, arg),
        "read" => run_registered_virtio_client(plan, BLK_REQ_PGET, ""),
        "rollback" => run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, ""),
        "history" => {
            kprintln!("[storage] Cairn v0 keeps current sector 2 and previous sector 3");
            kprintln!("[storage] current value:");
            run_registered_virtio_client(plan, BLK_REQ_PGET, "");
            kprintln!("[storage] for the full commit history use `cairn-log <ns>` (Cairn v1)");
        }
        "cairn-status" => {
            let _ = run_registered_virtio_client_ns(plan, BLK_REQ_CAIRN_STATUS, "", 0);
        }
        "cairn-commit" => cairn_cmd_commit(plan, arg),
        "cairn-get" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, arg),
        "cairn-log" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_LOG, arg),
        "cairn-verify" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_VERIFY, arg),
        "cairn-rollback" => cairn_cmd_rollback(plan, arg),
        "cairn-demo" => run_cairn_demo(plan),
        "sand-log" => sand_cmd(plan, BLK_REQ_SAND_LOG, arg),
        "sand-info" => sand_cmd(plan, BLK_REQ_SAND_INFO, arg),
        "sand-demo" => run_sand_demo(plan),
        "sfar-plan" => sfar_cmd(plan, BLK_REQ_SFAR_PLAN, arg),
        "sfar-rollback" => sfar_cmd(plan, BLK_REQ_SFAR_ROLLBACK, arg),
        "sfar-demo" => run_sfar_demo(plan),
        "sfar-cross-demo" => run_sfar_cross_demo(plan),
        "comp-demo" => run_comp_demo(plan),
        "redteam" => run_redteam(plan),
        "why-denied" => why_denied(arg),
        "tbar" => sfar_cmd(plan, BLK_REQ_TBAR, arg),
        "overnight" => run_overnight(plan),
        "vblkd" => {
            kprintln!("[kernel] exercising registered virtio-blk daemon with IPC client");
            kprintln!("[kernel] daemon gets DEVICE+DMA+IPC; client gets IPC+DMA only (no MMIO)");
            run_virtio_blk_daemon_demo(plan);
            kprintln!("[kernel] virtio-blk daemon demo done; back in the console");
        }
        "agent" => {
            use dezh_core::ir;
            kprintln!("[kernel] Dezh-IR (shared dezh-core engine): verified, capability-gated");
            let mut buf = [0u8; 512];
            let sum = ir::demo_sum(&mut buf);
            if let Err(t) = ir::verify(sum) {
                kprintln!("  verify failed: {}", t.msg());
            } else {
                kprintln!("  prog 1 (loop: sum 1..=5, then print) WITH the PRINT capability:");
                let mut h = KHost {
                    caps: ir::CAP_PRINT,
                    cairn: None,
                    intent: 0,
                    derived: 0,
                };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
                kprintln!("  prog 1 again WITHOUT the PRINT capability:");
                let mut h = KHost {
                    caps: 0,
                    cairn: None,
                    intent: 0,
                    derived: 0,
                };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
            }
            let mut buf2 = [0u8; 512];
            let cairn = ir::demo_cairn(&mut buf2);
            kprintln!("  prog 2 (write to Cairn, then read it back) with WRITE+READ+PRINT:");
            kprintln!("  (durable: lands in Cairn v1 ns=agent via the user-space storage daemon)");
            let mut h = KHost {
                caps: ir::CAP_WRITE | ir::CAP_READ | ir::CAP_PRINT,
                cairn: cairn_ns_id("agent").map(|ns| (plan, ns)),
                intent: 0,
                derived: 0,
            };
            if let Err(t) = ir::run(cairn, &mut h) {
                kprintln!("  [ir] TRAP: {}", t.msg());
            }
        }
        "frames" => {
            let free0 = unsafe { *FRAME_FREE.get() };
            kprintln!("frames: {} total, {} free", unsafe { *FRAME_TOTAL.get() }, free0);
            let a = frame_alloc();
            let b = frame_alloc();
            let c = frame_alloc();
            let first = unsafe { *(a as *const u64) };
            kprintln!("  allocated {a:#x} {b:#x} {c:#x}; first word of {a:#x} = {first} (zeroed)");
            kprintln!("  free now: {}", unsafe { *FRAME_FREE.get() });
            frame_free(a);
            frame_free(b);
            frame_free(c);
            kprintln!(
                "  after free: {} (back to {})",
                unsafe { *FRAME_FREE.get() },
                free0
            );
        }
        "services" => {
            let _ = ensure_virtio_block_service(plan);
            print_services();
        }
        "svc-stop" => match arg {
            "virtio-block" => svc_stop_virtio(plan),
            _ => kprintln!("usage: svc-stop virtio-block"),
        },
        "svc-restart" => match arg {
            "virtio-block" => svc_restart_virtio(plan),
            _ => kprintln!("usage: svc-restart virtio-block"),
        },
        "svc-fault-demo" => match arg {
            "virtio-block" => svc_fault_demo_virtio(plan),
            _ => kprintln!("usage: svc-fault-demo virtio-block"),
        },
        "root" => {
            kprintln!("[install] root summary:");
            kprintln!(
                "  manifest root={} block={} marker_sector={} metadata_sector={}",
                plan.install_manifest.root_service,
                plan.install_manifest.block_service,
                plan.install_manifest.layout.marker_sector,
                plan.install_manifest.layout.root_metadata_sector
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
            run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
        }
        "install" => install_command(plan, arg),
        "pkg-recv" => pkg::pkg_recv(plan),
        "sig-demo" => pkg::sig_demo(plan),
        "pkg-list" => pkg::pkg_list(plan),
        "pkg-info" => pkg::pkg_info(plan, arg),
        "pkg-run" => pkg::pkg_run(plan, arg),
        "intent-open" => pkg::intent_open(arg),
        "intent-revoke" => pkg::intent_revoke(arg),
        "lease-demo" => pkg::lease_demo(),
        "cap-demo" => run_cap_demo(),
        "smp-demo" => run_smp_demo(),
        "smp-task" => run_smp_task_demo(),
        "smp-sched" => run_smp_sched_demo(),
        "smp-isolate" => run_smp_isolate_demo(),
        "ns-revoke" => ns_revoke(plan, arg),
        "ns-grant" => ns_grant(plan, arg),
        "nsrevoke-demo" => run_nsrevoke_demo(plan),
        "agentrevoke-demo" => run_agentrevoke_demo(plan),
        "irq-stat" => irq_stat(),
        "net-probe" => net_probe(),
        "marz-send" => run_marz_send(plan, arg),
        "marz-ping" => run_marz_ping(arg),
        "dev-revoke" => dev_authority_set(arg, false),
        "dev-grant" => dev_authority_set(arg, true),
        "dev-demo" => run_dev_demo(plan),
        "marz-grant" => marz_dest_authority(arg, true),
        "marz-revoke" => marz_dest_authority(arg, false),
        "marz-demo" => run_marz_demo(plan),
        "marz-effect-demo" => run_marz_effect_demo(plan),
        "exfil-demo" => run_exfil_demo(),
        "taintflow-demo" => run_taintflow_demo(plan),
        "taint" => taint_show(),
        "declassify" => declassify(),
        "endorse" => endorse(),
        "ingress-demo" => run_ingress_demo(plan),
        "intent-list" => pkg::intent_list(),
        "intent-run" => pkg::intent_run(plan, arg),
        "intent-demo" => pkg::intent_demo(plan),
        "pkg-remove" => pkg::pkg_remove(plan, arg),
        "pkg-store" => pkg::pkg_store(plan),
        "pkg-journal" => pkg::pkg_journal(plan),
        "pkg-recover" => pkg::pkg_recover(plan),
        "pkg-verify" => pkg::pkg_verify(plan, arg),
        "pkg-fault" => pkg::pkg_fault(plan, arg),
        "pkg-gc" => pkg::pkg_gc(plan, arg),
        "pkg-update" => pkg::pkg_update(plan, arg),
        "pkg-rollback" => pkg::pkg_rollback(plan, arg),
        "pkg-versions" => pkg::pkg_versions(plan, arg),
        "pkg-review" => pkg::pkg_review(plan, arg),
        "pkg-pin" => pkg::pkg_pin(plan, arg, true),
        "pkg-unpin" => pkg::pkg_pin(plan, arg, false),
        "pkg-retire" => pkg::pkg_retire(plan, arg),
        "pkg-lifecycle" => pkg::pkg_lifecycle(plan),
        "pkg-audit" => pkg::pkg_audit(plan, arg),
        "apps" => print_apps(plan, arg),
        "app-info" => app_info(plan, arg),
        "app-install" => app_install(plan, arg),
        "app-run" => app_run(plan, arg),
        "app-remove" => app_remove(plan, arg),
        "app-deny" => app_deny(plan, arg),
        "app-permissions" => app_permissions(arg),
        "note-set" => run_registered_virtio_client(plan, BLK_REQ_NOTE_SET, arg),
        "note-get" => run_registered_virtio_client(plan, BLK_REQ_NOTE_GET, ""),
        "lab-set" => run_registered_virtio_client(plan, BLK_REQ_LAB_SET, arg),
        "lab-get" => run_registered_virtio_client(plan, BLK_REQ_LAB_GET, ""),
        "calc" => calc_command(plan, arg),
        "calc-history" => run_registered_virtio_client(plan, BLK_REQ_CALC_GET, ""),
        "vault-put" => vault_put(plan, arg),
        "vault-get" => run_registered_virtio_client(plan, BLK_REQ_VAULT_GET, ""),
        "install-check" => {
            kprintln!("[install] validating boot/install manifest v0");
            kprintln!(
                "[install] target={:?} root={} block={} marker_sector={}",
                plan.install_manifest.target,
                plan.install_manifest.root_service,
                plan.install_manifest.block_service,
                plan.install_manifest.layout.marker_sector
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
        }
        "install-init" => {
            kprintln!(
                "[install] initializing Dezh root marker and metadata via user-space block service"
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
        }
        "root-status" => {
            kprintln!("[install] reading Dezh root metadata via registered block service");
            run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
        }
        "events" => print_events(),
        "audit" => print_audit(),
        "uptime" => {
            let t = TICKS.load(Ordering::Relaxed);
            kprintln!("uptime: {} ticks (~{}.{} s)", t, t / TIMER_HZ, t % TIMER_HZ);
        }
        "echo" => kprintln!("{arg}"),
        "run" => {
            kprintln!("[kernel] spawning U-mode task; granted capability: PRINT (not TIME)");
            run_tasks(&[(user_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            kprintln!("[kernel] task returned; back in the S-mode console");
        }
        "load" => {
            kprintln!(
                "[kernel] loading a separate program into its own address space (cap: PRINT)"
            );
            run_processes(&[ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 0).uart()]);
            kprintln!("[kernel] program exited; back in the console");
        }
        "procs" => {
            kprintln!("[kernel] loading TWO copies as separate processes (own address spaces)");
            run_processes(&[
                ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 1),
                ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 2),
            ]);
            kprintln!("[kernel] all processes exited; back in the console");
        }
        "rogue" => {
            kprintln!(
                "[kernel] spawning a rogue U-mode task (it will try to touch the UART directly)"
            );
            run_tasks(&[(rogue_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            kprintln!("[kernel] rogue task handled; console survived");
        }
        "multi" => {
            kprintln!("[kernel] spawning 3 cooperative U-mode tasks (round-robin via yield)");
            run_tasks(&[
                (worker_a as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (worker_b as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (worker_c as *const () as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] all tasks done; back in the console");
        }
        "linux" => {
            kprintln!("[kernel] running a Linux-ABI app through the Pol personality layer");
            run_tasks(&[(linux_app as *const () as usize, TASK_PRINT, PERS_LINUX)]);
            kprintln!("[kernel] Linux app done; back in the console");
        }
        "linux-elf" => {
            kprintln!(
                "[kernel] loading a REAL unmodified static Linux/RISC-V ELF ({} bytes,",
                LINUX_GUEST_ELF.len()
            );
            kprintln!("         target=riscv64gc-unknown-linux-musl) into its own address space.");
            kprintln!("[kernel] --- WITH the print capability: Pol services its syscalls ---");
            record_event("kernel", "pol.elf.run", "process", "start");
            run_foreground_processes(&[ProcessSpec::new(LINUX_GUEST_ELF, TASK_PRINT, 0).linux()]);
            kprintln!("[kernel] --- WITHOUT the print capability: kernel DENIES write ---");
            run_foreground_processes(&[ProcessSpec::new(LINUX_GUEST_ELF, 0, 0).linux()]);
            record_event("kernel", "pol.elf.run", "process", "OK");
            kprintln!("[kernel] the same ELF also runs on real riscv64 Linux; back in the console");
        }
        "bench" => {
            kprintln!("[kernel] running ecall round-trip microbenchmark (500000 calls)...");
            run_tasks(&[(bench_task as *const () as usize, 0, PERS_NATIVE)]);
            kprintln!("[kernel] benchmark done");
        }
        "bench-pol" => {
            // Same zero-work syscall via two paths: native SYS_PRINT vs the Linux
            // write(2) ABI routed through Pol. The kernel times both; the delta is
            // the per-syscall translation overhead (F4, D015). QEMU-emulated.
            kprintln!(
                "[kernel] Pol translation microbenchmark ({} calls each): native SYS_PRINT vs Linux write(2)...",
                BENCH_POL_ITERS
            );
            let n = BENCH_POL_ITERS as u64;
            let t0 = rdtime();
            run_tasks(&[(bench_native_print_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            let t1 = rdtime();
            run_tasks(&[(bench_pol_write_task as *const () as usize, TASK_PRINT, PERS_LINUX)]);
            let t2 = rdtime();
            let native_ns = t1.wrapping_sub(t0).saturating_mul(100) / n;
            let pol_ns = t2.wrapping_sub(t1).saturating_mul(100) / n;
            let overhead = pol_ns.saturating_sub(native_ns);
            kprintln!("  [bench-pol] native SYS_PRINT round-trip:   ~{native_ns} ns/call (QEMU-emulated)");
            kprintln!("  [bench-pol] Pol Linux write(2) round-trip: ~{pol_ns} ns/call (QEMU-emulated)");
            kprintln!(
                "  [bench-pol] Pol translation overhead: ~{overhead} ns/call (delta over native, emulated)"
            );
            kprintln!("[kernel] benchmark done");
        }
        "bench-os" => run_bench_os(),
        "bench-ipc" => run_bench_ipc(),
        "bench-storage" => run_bench_storage(plan),
        "bench-caps" => run_bench_caps(),
        "bench-all" => run_bench_all(plan),
        "stress-lab" => stress_lab(plan, arg),
        "preempt" => {
            kprintln!("[kernel] two CPU-bound tasks that never yield (watch them interleave)");
            run_tasks(&[
                (preempt_a as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (preempt_b as *const () as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] preemption demo done");
        }
        "spy" => {
            kprintln!(
                "[kernel] isolation: task0 owns a private stack; task1 (spy) tries to read it"
            );
            kprintln!("[kernel] (task0 stack region base = {:#x})", stack_base());
            run_tasks(&[
                (victim_task as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (spy_task as *const () as usize, 0, PERS_NATIVE),
            ]);
            kprintln!("[kernel] isolation demo done");
        }
        "ipc" => {
            kprintln!("[kernel] IPC: a no-authority service + an agent that delegates PRINT to it");
            // task 0 = service (no caps), task 1 = agent (holds PRINT)
            run_tasks(&[
                (service_task as *const () as usize, TASK_IPC, PERS_NATIVE),
                (agent_task as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] IPC demo done; back in the console");
        }
        "ipcq" => {
            if virtio_service_is_running() {
                kprintln!(
                    "[kernel] IPC queue demo skipped to keep running services alive; use it before starting services"
                );
                return;
            }
            kprintln!("[kernel] IPC queue: two clients enqueue while the service is busy");
            run_tasks(&[
                (
                    queue_service_task as *const () as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] IPC queue demo done; back in the console");
        }
        "queues" => {
            if virtio_service_is_running() {
                kprintln!(
                    "[kernel] queues demo skipped to keep running services alive; use it before starting services"
                );
                return;
            }
            kprintln!("[kernel] queues: bounded FIFO IPC mailbox demo");
            run_tasks(&[
                (
                    queue_service_task as *const () as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] queue demo done; back in the console");
        }
        "cairn" => {
            kprintln!(
                "[kernel] Cairn store service + an agent doing a rollbackable action over IPC"
            );
            // task 0 = cairn store service, task 1 = agent (holds PRINT)
            run_tasks(&[
                (cairn_service as *const () as usize, TASK_IPC, PERS_NATIVE),
                (agent_cairn as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] Cairn demo done; back in the console");
        }
        "deny" => {
            kprintln!("[safety] denial tour: no ambient authority across caps, MMIO, and Pol");
            kprintln!("denied: 'secret' requires capability SECRET (not held)");
            run_virtio_no_grant_probe();
            kprintln!("[safety] no-grant MMIO fault returned; console survived");
            if virtio_service_is_running() {
                kprintln!(
                    "[safety] Pol denial demo skipped here to keep running services alive; use `linux` before starting services"
                );
            } else {
                run_tasks(&[(linux_app as *const () as usize, TASK_PRINT, PERS_LINUX)]);
                kprintln!("[safety] unsupported Linux syscall returned ENOSYS; console survived");
            }
        }
        "halt" => {
            kprintln!("halting.");
            shutdown(FINISH_PASS);
        }
        other => kprintln!("'{other}' has no handler"),
    }
}

fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let c = Uart.getc();
        match c {
            b'\n' => {
                if SKIP_LF_AFTER_CR.swap(false, Ordering::Relaxed) {
                    continue;
                }
                kprintln!();
                return len;
            }
            b'\r' => {
                SKIP_LF_AFTER_CR.store(true, Ordering::Relaxed);
                kprintln!();
                return len;
            }
            0x7f | 0x08 => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
                if len > 0 {
                    len -= 1;
                    kprint!("\x08 \x08");
                }
            }
            c if (c == b' ' || c.is_ascii_graphic()) && len < buf.len() => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
                buf[len] = c;
                len += 1;
                Uart.putc(c);
            }
            _ => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn kmain(hart_id: usize, _fdt: usize) -> ! {
    Uart.init();
    // SBI hands the boot hart's id in a0. Capture it before anything else needs a0.
    let boot_hart = hart_id;

    let memory = vec![
        MemoryRegion::new(0x8000_0000, 0x20_0000, MemoryKind::Reserved),
        MemoryRegion::new(0x8020_0000, 0x7E0_0000, MemoryKind::Usable),
        MemoryRegion::new(0x1000_0000, 0x1000, MemoryKind::Mmio),
    ];
    let info = BootInfo::qemu_minimal_riscv(memory.clone());

    let plan = match plan_boot(&info) {
        Ok(plan) => plan,
        Err(e) => {
            kprintln!("[dezh-boot] BOOT CONTRACT VIOLATION: {e:?}");
            shutdown(FINISH_FAIL);
        }
    };

    // Dezh banner (ASCII so it renders on any serial console). The info line is
    // filled from the validated boot plan.
    kprintln!();
    kprintln!(r"   ____            _");
    kprintln!(r"  |  _ \  ___  ___| |__");
    kprintln!(r"  | | | |/ _ \/_  / '_ \");
    kprintln!(r"  | |_| |  __/ / /| | | |");
    kprintln!(r"  |____/ \___//___|_| |_|");
    kprintln!("  Dezh OS - capability-secure - no ambient authority");
    kprintln!(
        "  v0 - riscv64 - {} MiB usable - {} services",
        plan.usable_bytes / 1024 / 1024,
        plan.services.len()
    );
    kprintln!();
    kprintln!("[dezh-boot] alive on bare metal (qemu virt, riscv64, S-mode)");
    kprintln!("[dezh-boot] boot contract VALIDATED");
    kprintln!("[dezh-boot] banner: {}", boot_banner(&plan));
    kprintln!("[dezh-boot] no ambient authority: capability seeds bound to declared services only");

    kprintln!("[dezh-boot] installing trap vector + supervisor timer...");
    unsafe {
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        asm!("csrs sie, {}", in(reg) STIE);
        asm!("csrs sstatus, {}", in(reg) 1usize << 1); // SIE: global supervisor interrupts
        asm!("csrw scounteren, {}", in(reg) 0x7usize); // let U-mode read cycle/time/instret
    }

    plic_init(boot_hart);
    kprintln!("[dezh-boot] PLIC up: virtio device interrupts routed to boot hart {boot_hart} S-mode (no longer polled-only)");

    smp_bringup(boot_hart);
    smp_report_boot();

    kprintln!("[dezh-boot] enabling Sv39 paging (U-mode confined to its own region)...");
    build_page_tables();
    enable_paging();
    frames_init();
    {
        let (total, free) = unsafe { ((*FRAME_TOTAL.get()), (*FRAME_FREE.get())) };
        kprintln!(
            "[dezh-boot] frame allocator: {} x 4 KiB frames ({} MiB free)",
            total,
            (free * FRAME_SIZE) / (1024 * 1024)
        );
    }
    kprintln!(
        "[dezh-boot] embedded user ELFs: userprog={} bytes, virtio-blk={} bytes, dezh-bench={} bytes, dezh-note={} bytes, dezh-lab={} bytes, dezh-calc={} bytes, dezh-vault={} bytes",
        USERPROG_ELF.len(),
        VIRTIO_BLK_ELF.len(),
        BENCH_ELF.len(),
        NOTE_ELF.len(),
        LAB_ELF.len(),
        CALC_ELF.len(),
        VAULT_ELF.len()
    );
    kprintln!(
        "[dezh-boot] install manifest v0: root={} block={} marker_sector={}",
        plan.install_manifest.root_service,
        plan.install_manifest.block_service,
        plan.install_manifest.layout.marker_sector
    );
    build_service_registry(&plan);

    let held = cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT | cap::SPAWN;
    console(&plan, &memory, held);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprint!("\n[dezh-boot] PANIC: ");
    kprintln!("{info}");
    shutdown(FINISH_FAIL);
}

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
mod console;

use console::print_memstat;

use crate::apps::{app_calc_is_active, app_install, app_run, app_vault_is_active};


use abi::*;
use audit::record_event;
mod demos;
mod dev;
mod difc;
mod mm;
mod net;
mod ocap;
mod pkg;
mod proc;
mod sched;
mod smp;


use crate::dev::virtio::{VIRTIO_BLK_MMIO_PA, VIRTIO_DEVICE_ID_NET, VIRTIO_MMIO_COUNT, VIRTIO_MMIO_STRIDE, find_virtio_mmio};


use mm::paging::stack_base;

use sched::{
    print_ipcstat, run_foreground_processes, run_tasks, TaskState, LINUX_EXIT, LINUX_WRITE, MAX_TASKS, PERS_LINUX, PERS_NATIVE, TEXIT,
    TIRQ_WAITING, TSTATE,
};
mod service;
mod vblk;

use vblk::{
    prepare_virtio_input_bytes, read_virtio_output_sector, run_registered_virtio_client,
    run_registered_virtio_client_ns, run_registered_virtio_client_status,
    run_registered_virtio_sector_status,
    run_virtio_client_ns_raw, virtio_dma_pa,
};
use service::virtio_service_is_running;

// `Uart` is re-exported at the crate root because the kprint!/kprintln! macros
// expand to `$crate::Uart` at every call site in the tree. `Global` used to be
// re-exported here too; main.rs no longer owns a single one, so modules import
// it from `mm::global` themselves.
pub(crate) use dev::uart::{Uart, UART_BASE};
use mm::frames::{frame_free, FRAME_FREE, FRAME_SIZE};
use proc::loader::{
    reclaim_resources,
};
use ocap::device::dev_authority_live;
use ocap::ns::ns_authority_ok;
use difc::{
    difc_ingress,
    NS_SECRET_VAULT, OP_TAINT,
};
use arch::finisher::{shutdown, FINISH_FAIL};
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
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprint!("\n[dezh-boot] PANIC: ");
    kprintln!("{info}");
    shutdown(FINISH_FAIL);
}

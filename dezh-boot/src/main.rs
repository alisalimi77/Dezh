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

use dev::plic::{plic_handle, SCAUSE_EXTERNAL};
use utasks::*;



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
mod sync;
mod utasks;


use crate::dev::virtio::{VIRTIO_BLK_MMIO_PA, VIRTIO_DEVICE_ID_NET, VIRTIO_MMIO_STRIDE, find_virtio_mmio};


use mm::paging::stack_base;

use sched::{
    print_ipcstat, run_tasks, PERS_NATIVE,
};
mod service;
mod vblk;

use vblk::{
    prepare_virtio_input_bytes, read_virtio_output_sector,
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
use mm::frames::{frame_free, FRAME_SIZE};
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
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec;
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

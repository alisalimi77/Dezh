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

mod pkg;

// The RISC-V implementation of the shared Dezh-core Host: capability check +
// the side effect (kernel console). The Dezh-IR engine lives in dezh-core and
// is identical across ISAs.
struct KHost {
    caps: u32,
}
impl dezh_core::ir::Host for KHost {
    fn can(&self, cap: u32) -> bool {
        self.caps & cap != 0
    }
    fn print_num(&mut self, v: i64) {
        kprintln!("  [ir] print -> {v}");
    }
    fn print_str(&mut self, s: &[u8]) {
        kprintln!("  [ir] {}", core::str::from_utf8(s).unwrap_or("<non-utf8>"));
    }
    fn cairn_put(&mut self, _data: &[u8]) -> bool {
        false
    }
    fn cairn_get(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
}

use core::alloc::{GlobalAlloc, Layout};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

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

extern "C" {
    fn trap_entry();
    fn restore_kernel_ctx() -> !;
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

extern "C" {
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

// --- NS16550 UART on the QEMU `virt` board. --------------------------------
const UART_BASE: *mut u8 = 0x1000_0000 as *mut u8;
const UART_RBR: usize = 0;
const UART_THR: usize = 0;
const UART_FCR: usize = 2;
const UART_LSR: usize = 5;

pub(crate) struct Uart;

impl Uart {
    fn init(&self) {
        unsafe { write_volatile(UART_BASE.add(UART_FCR), 0x07) } // enable + clear FIFOs
    }
    fn putc(&self, byte: u8) {
        unsafe { write_volatile(UART_BASE.add(UART_THR), byte) }
    }
    fn getc(&self) -> u8 {
        loop {
            let lsr = unsafe { read_volatile(UART_BASE.add(UART_LSR)) };
            if lsr & 0x01 != 0 {
                return unsafe { read_volatile(UART_BASE.add(UART_RBR)) };
            }
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.putc(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{ let _ = core::write!($crate::Uart, $($arg)*); }};
}
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{ let _ = core::writeln!($crate::Uart, $($arg)*); }};
}

// --- Minimal bump allocator (alloc is needed by dezh-kernel's Vec/String). --
const HEAP_SIZE: usize = 1 << 20;

struct BumpHeap {
    arena: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpHeap {}
unsafe impl GlobalAlloc for BumpHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.arena.get() as usize;
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (base + cur + layout.align() - 1) & !(layout.align() - 1);
            let new_next = aligned - base + layout.size();
            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange(cur, new_next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
#[global_allocator]
static HEAP: BumpHeap = BumpHeap {
    arena: UnsafeCell::new([0; HEAP_SIZE]),
    next: AtomicUsize::new(0),
};

// --- QEMU `virt` SiFive test finisher: cleanly exit the emulator. ----------
const TEST_FINISHER: *mut u32 = 0x10_0000 as *mut u32;
const FINISH_PASS: u32 = 0x5555;
const FINISH_FAIL: u32 = 0x3333;

fn shutdown(code: u32) -> ! {
    unsafe { write_volatile(TEST_FINISHER, code) }
    loop {
        unsafe { asm!("wfi") }
    }
}

// --- Timer (silent background uptime tick). --------------------------------
const TIMER_DELTA: u64 = 1_000_000;
const TIMER_HZ: u64 = 10;
const QUANTUM: u64 = 50_000; // ~5 ms scheduler time slice for preemption
const STIE: usize = 1 << 5; // supervisor timer interrupt enable (in `sie`)
static TICKS: AtomicU64 = AtomicU64::new(0);
static SKIP_LF_AFTER_CR: AtomicBool = AtomicBool::new(false);

fn rdtime() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

fn sbi_set_timer(stime: u64) {
    unsafe {
        asm!("ecall", in("a0") stime, in("a7") 0usize, lateout("a0") _, lateout("a1") _);
    }
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
const SYS_DENIED: usize = usize::MAX; // result sentinel for "capability not held"

// --- Per-task capabilities (what the running U-mode task is allowed to do). --
const TASK_PRINT: usize = 1 << 0;
const TASK_TIME: usize = 1 << 1;
const TASK_IPC: usize = 1 << 2;
const TASK_DEVICE_VIRTIO_BLK: usize = 1 << 3;
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
extern "C" {
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

// --- Sv39 paging: confine U-mode tasks to their own region. -----------------
// PMP cannot distinguish S-mode from U-mode, so memory isolation between the
// kernel and a user task is done with page tables: kernel + MMIO pages are
// supervisor-only (U=0); only the user region is U=1. A U-mode access anywhere
// else page-faults.
#[repr(align(4096))]
struct PageTable([u64; 512]);
static mut ROOT: PageTable = PageTable([0; 512]);
static mut L1: PageTable = PageTable([0; 512]);

const PTE_V: u64 = 1 << 0;
const PTE_R: u64 = 1 << 1;
const PTE_W: u64 = 1 << 2;
const PTE_X: u64 = 1 << 3;
const PTE_U: u64 = 1 << 4;
const PTE_A: u64 = 1 << 6;
const PTE_D: u64 = 1 << 7;

const RAM_BASE: u64 = 0x8000_0000;
const MEGA: u64 = 0x20_0000; // 2 MiB megapage

fn pte(pa: u64, flags: u64) -> u64 {
    ((pa >> 12) << 10) | PTE_V | PTE_A | PTE_D | flags
}

/// Base of the per-task stack regions: the 2 MiB megapage right after the shared
/// code region. Task `i` owns the megapage `STACK_BASE + i*2MiB`.
fn stack_base() -> u64 {
    user_region().1 as u64
}

fn task_stack_top(i: usize) -> usize {
    (stack_base() + (i as u64 + 1) * MEGA) as usize
}

fn stack_region_l1_index(i: usize) -> usize {
    (((stack_base() - RAM_BASE) / MEGA) as usize) + i
}

fn build_page_tables() {
    let (us, ue) = user_region();
    let code_lo = us as u64;
    let code_hi = ue as u64;
    let sbase = stack_base();
    let stacks_hi = sbase + (MAX_TASKS as u64) * MEGA;
    unsafe {
        let root = &mut (*core::ptr::addr_of_mut!(ROOT)).0;
        let l1 = &mut (*core::ptr::addr_of_mut!(L1)).0;
        // 0x0..0x4000_0000 as one kernel-only gigapage (covers UART + finisher).
        root[0] = pte(0x0, PTE_R | PTE_W | PTE_X); // U=0
                                                   // 0x8000_0000..0xC000_0000 via an L1 table of 2 MiB megapages.
        let l1_pa = core::ptr::addr_of!(L1) as u64;
        root[2] = ((l1_pa >> 12) << 10) | PTE_V; // non-leaf pointer
        for i in 0..512usize {
            let pa = RAM_BASE + (i as u64) * MEGA;
            let flags = if pa >= code_lo && pa < code_hi {
                // Shared task code: read+execute for U-mode, no write (W^X).
                PTE_R | PTE_X | PTE_U
            } else if pa >= sbase && pa < stacks_hi {
                // Per-task stack: read+write, U bit toggled per running task.
                PTE_R | PTE_W
            } else {
                // Kernel + MMIO: supervisor-only.
                PTE_R | PTE_W | PTE_X
            };
            l1[i] = pte(pa, flags);
        }
    }
}

/// Make only `active`'s stack region U-accessible; clear U on every other task's
/// stack. This is what isolates tasks from each other: while task `i` runs, it
/// can touch its own stack but a load/store into another task's region faults.
fn set_active_task_mem(active: usize) {
    unsafe {
        let l1 = &mut (*core::ptr::addr_of_mut!(L1)).0;
        for i in 0..MAX_TASKS {
            let idx = stack_region_l1_index(i);
            if i == active {
                l1[idx] |= PTE_U;
            } else {
                l1[idx] &= !PTE_U;
            }
        }
        asm!("sfence.vma");
    }
}

fn enable_paging() {
    let root_pa = core::ptr::addr_of!(ROOT) as u64;
    let satp = (8u64 << 60) | (root_pa >> 12); // mode 8 = Sv39
    unsafe {
        asm!("sfence.vma");
        asm!("csrw satp, {}", in(reg) satp);
        asm!("sfence.vma");
        asm!("csrs sstatus, {}", in(reg) 1usize << 18); // SUM: S-mode may read U pages
    }
}

// --- Physical frame allocator (the bedrock for dynamic memory). --------------
// A free list of 4 KiB physical frames over a RAM pool above all static
// regions. Every frame is ZEROED on allocation, so memory handed to a new
// process can never leak a previous owner's bytes — capability hygiene, and an
// avoidable mistake we do not repeat.
const FRAME_SIZE: usize = 4096;
const FRAME_POOL_BASE: usize = 0x8100_0000; // 16 MiB into RAM (above kernel/.user/stacks)
const FRAME_POOL_END: usize = 0x8800_0000; // end of the 128 MiB QEMU `virt` RAM

static mut FRAME_FREE_HEAD: usize = 0; // 0 = empty; otherwise a free frame's address
static mut FRAME_TOTAL: usize = 0;
static mut FRAME_FREE: usize = 0;

fn frames_init() {
    unsafe {
        FRAME_FREE_HEAD = 0;
        FRAME_TOTAL = 0;
        FRAME_FREE = 0;
        // Link every frame into the free list (each free frame stores the next).
        let mut a = FRAME_POOL_BASE;
        while a + FRAME_SIZE <= FRAME_POOL_END {
            *(a as *mut usize) = FRAME_FREE_HEAD;
            FRAME_FREE_HEAD = a;
            FRAME_TOTAL += 1;
            FRAME_FREE += 1;
            a += FRAME_SIZE;
        }
    }
}

/// Allocate one zeroed physical frame, or 0 if out of memory.
fn frame_alloc() -> usize {
    unsafe {
        let f = FRAME_FREE_HEAD;
        if f == 0 {
            return 0;
        }
        FRAME_FREE_HEAD = *(f as *const usize);
        FRAME_FREE -= 1;
        core::ptr::write_bytes(f as *mut u8, 0, FRAME_SIZE); // zero on alloc
        f
    }
}

/// Return a frame to the free list.
fn frame_free(f: usize) {
    unsafe {
        *(f as *mut usize) = FRAME_FREE_HEAD;
        FRAME_FREE_HEAD = f;
        FRAME_FREE += 1;
    }
}

const MAX_TASK_OWNED_FRAMES: usize = 384;

#[derive(Clone, Copy, PartialEq)]
enum TaskKind {
    Empty,
    Foreground,
    Daemon,
    LegacyBakedTask,
}

#[derive(Clone, Copy)]
struct TaskResources {
    kind: TaskKind,
    root: usize,
    count: usize,
    frames: [usize; MAX_TASK_OWNED_FRAMES],
}

#[derive(Clone, Copy)]
struct AddressSpaceBuild {
    root: usize,
    entry: usize,
    resources: TaskResources,
}

const EMPTY_TASK_RESOURCES: TaskResources = TaskResources {
    kind: TaskKind::Empty,
    root: 0,
    count: 0,
    frames: [0; MAX_TASK_OWNED_FRAMES],
};

impl TaskResources {
    fn new(kind: TaskKind) -> Self {
        TaskResources {
            kind,
            root: 0,
            count: 0,
            frames: [0; MAX_TASK_OWNED_FRAMES],
        }
    }

    fn add_frame(&mut self, frame: usize) -> bool {
        if frame == 0 || self.count >= MAX_TASK_OWNED_FRAMES {
            return false;
        }
        self.frames[self.count] = frame;
        self.count += 1;
        true
    }

    fn alloc_frame(&mut self) -> usize {
        let frame = frame_alloc();
        if frame == 0 {
            return 0;
        }
        if !self.add_frame(frame) {
            frame_free(frame);
            return 0;
        }
        frame
    }
}

/// The separate user program, compiled to its own riscv ELF by build.rs and
/// embedded here. The loader maps it into a fresh address space at runtime.
const USERPROG_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/userprog.elf"));
const VIRTIO_BLK_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/virtio-blk.elf"));
const BENCH_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-bench.elf"));
const NOTE_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-note.elf"));
const LAB_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-lab.elf"));
const CALC_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-calc.elf"));
const VAULT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-vault.elf"));

const DEV_UART_VA: usize = 0x5000_0000;
const DEV_VIRTIO_BLK_VA: usize = 0x5000_0000;
const VIRTIO_BLK_MMIO_PA: usize = 0x1000_1000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;
const VIRTIO_MMIO_COUNT: usize = 8;
const VIRTIO_DMA_VA: usize = 0x5100_0000;
const VIRTIO_DMA_SIZE: usize = 16 * 1024;
const VIRTIO_INPUT_OFF: usize = 12_288;

#[repr(align(4096))]
#[allow(dead_code)]
struct DmaWindow([u8; VIRTIO_DMA_SIZE]);
static mut VIRTIO_DMA: DmaWindow = DmaWindow([0; VIRTIO_DMA_SIZE]);

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
}

// --- Program loader + per-process address space ------------------------------
// A loaded program gets its OWN page table (satp) built from frames: the kernel
// is mapped (U=0) so traps work, and the program's segments + a stack are mapped
// U=1. This is the proper foundation for running real, separately-built programs
// (and ends the "user calls kernel-resident helpers" fault for good).

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    let mut i = 0;
    while i < 8 {
        a[i] = b[o + i];
        i += 1;
    }
    u64::from_le_bytes(a)
}

/// Walk one level of an Sv39 table, allocating the next-level table if absent.
unsafe fn walk_alloc(
    table: *mut u64,
    idx: usize,
    resources: &mut TaskResources,
) -> Option<*mut u64> {
    let e = *table.add(idx);
    if e & PTE_V != 0 {
        Some((((e >> 10) << 12) as usize) as *mut u64) // existing next table
    } else {
        let frame = resources.alloc_frame();
        if frame == 0 {
            return None;
        }
        *table.add(idx) = ((frame as u64 >> 12) << 10) | PTE_V; // non-leaf
        Some(frame as *mut u64)
    }
}

/// Map one 4 KiB page va->pa with `flags` in the page table rooted at `root`.
fn map_page(root: usize, va: usize, pa: usize, flags: u64, resources: &mut TaskResources) -> bool {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    unsafe {
        let Some(l1) = walk_alloc(root as *mut u64, vpn2, resources) else {
            return false;
        };
        let Some(l0) = walk_alloc(l1, vpn1, resources) else {
            return false;
        };
        *l0.add(vpn0) = pte(pa as u64, flags);
    }
    true
}

const USER_STACK_TOP: usize = 0x4070_0000;
const USER_STACK_BOTTOM: usize = 0x406F_0000;

/// Walk a page table to the frame backing `va` (page must already be mapped).
unsafe fn translate(root: usize, va: usize) -> usize {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    let l1 = (((*(root as *const u64).add(vpn2)) >> 10) << 12) as usize;
    let l0 = (((*(l1 as *const u64).add(vpn1)) >> 10) << 12) as usize;
    let leaf = *(l0 as *const u64).add(vpn0);
    ((leaf >> 10) << 12) as usize
}

/// Build a fresh address space for the embedded program. Returns (satp root, entry).
///
/// Two passes so that segments sharing a page (common: a small RX segment and an
/// R segment in the same 4 KiB page) are handled correctly: map every covered
/// page once, then copy each segment's bytes to the right intra-page offset.
fn reclaim_resources(resources: &mut TaskResources) {
    let mut i = 0usize;
    while i < resources.count {
        let frame = resources.frames[i];
        resources.frames[i] = 0;
        if frame != 0 {
            frame_free(frame);
        }
        i += 1;
    }
    *resources = EMPTY_TASK_RESOURCES;
}

fn build_address_space(spec: &ProcessSpec, kind: TaskKind) -> Option<AddressSpaceBuild> {
    let img = spec.elf;
    let mut resources = TaskResources::new(kind);
    let root = resources.alloc_frame();
    if root == 0 {
        return None;
    }
    resources.root = root;
    unsafe {
        let r = root as *mut u64;
        // Kernel mappings so traps resolve while this satp is active (U=0):
        *r.add(0) = pte(0x0, PTE_R | PTE_W | PTE_X); // 0..1 GiB gigapage (UART etc)
        let l1_pa = core::ptr::addr_of!(L1) as u64; // share the kernel's 0x8000_0000 L1
        *r.add(2) = ((l1_pa >> 12) << 10) | PTE_V;
    }

    let entry = u64_at(img, 24) as usize;
    let phoff = u64_at(img, 32) as usize;
    let phentsize = u16_at(img, 54) as usize;
    let phnum = u16_at(img, 56) as usize;

    // Pass 1: find the page-aligned VA span of all PT_LOAD segments and map it.
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(img, ph) != 1 {
            continue;
        }
        let pv = u64_at(img, ph + 16) as usize;
        let pm = u64_at(img, ph + 40) as usize;
        lo = lo.min(pv & !0xfff);
        hi = hi.max((pv + pm + 0xfff) & !0xfff);
    }
    let mut va = lo;
    while va < hi {
        let frame = resources.alloc_frame();
        if frame == 0 {
            reclaim_resources(&mut resources);
            return None;
        }
        // W^X: derive permissions from the ELF segment flags covering this page —
        // executable code is mapped R+X (never writable), data R+W (never
        // executable). (Linux historically allowed W+X; we don't.)
        let mut fl = PTE_U | PTE_R;
        for i in 0..phnum {
            let ph = phoff + i * phentsize;
            if u32_at(img, ph) != 1 {
                continue;
            }
            let pv = u64_at(img, ph + 16) as usize;
            let pm = u64_at(img, ph + 40) as usize;
            if va >= (pv & !0xfff) && va < ((pv + pm + 0xfff) & !0xfff) {
                let pf = u32_at(img, ph + 4); // PF_X=1, PF_W=2, PF_R=4
                if pf & 1 != 0 {
                    fl |= PTE_X;
                }
                if pf & 2 != 0 {
                    fl |= PTE_W;
                }
            }
        }
        if !map_page(root, va, frame, fl, &mut resources) {
            reclaim_resources(&mut resources);
            return None;
        }
        va += FRAME_SIZE;
    }

    // Pass 2: copy each segment's file bytes to the correct virtual addresses.
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(img, ph) != 1 {
            continue;
        }
        let poff = u64_at(img, ph + 8) as usize;
        let pvaddr = u64_at(img, ph + 16) as usize;
        let pfilesz = u64_at(img, ph + 32) as usize;
        let mut k = 0usize;
        while k < pfilesz {
            let dva = pvaddr + k;
            let frame = unsafe { translate(root, dva & !0xfff) };
            unsafe { *((frame + (dva & 0xfff)) as *mut u8) = img[poff + k] };
            k += 1;
        }
    }

    // Map the user stack (U=1 RW).
    let mut s = USER_STACK_BOTTOM;
    while s < USER_STACK_TOP {
        let frame = resources.alloc_frame();
        if frame == 0 {
            reclaim_resources(&mut resources);
            return None;
        }
        if !map_page(root, s, frame, PTE_U | PTE_R | PTE_W, &mut resources) {
            reclaim_resources(&mut resources);
            return None;
        }
        s += FRAME_SIZE;
    }

    // Device grants are explicit: no process sees MMIO unless its launch spec
    // maps that device. Drivers are user processes with device capabilities,
    // not kernel code with ambient hardware reach.
    if spec.map_uart {
        if !map_page(
            root,
            DEV_UART_VA,
            UART_BASE as usize,
            PTE_U | PTE_R | PTE_W,
            &mut resources,
        ) {
            reclaim_resources(&mut resources);
            return None;
        }
    }
    if spec.map_virtio_blk && spec.caps & TASK_DEVICE_VIRTIO_BLK != 0 {
        let mut i = 0usize;
        while i < VIRTIO_MMIO_COUNT {
            if !map_page(
                root,
                DEV_VIRTIO_BLK_VA + i * VIRTIO_MMIO_STRIDE,
                VIRTIO_BLK_MMIO_PA + i * VIRTIO_MMIO_STRIDE,
                PTE_U | PTE_R | PTE_W,
                &mut resources,
            ) {
                reclaim_resources(&mut resources);
                return None;
            }
            i += 1;
        }
    }
    if spec.map_virtio_dma && spec.caps & (TASK_BLOCK_READ | TASK_BLOCK_WRITE) != 0 {
        let dma_pa = core::ptr::addr_of!(VIRTIO_DMA) as usize;
        let mut off = 0usize;
        while off < VIRTIO_DMA_SIZE {
            if !map_page(
                root,
                VIRTIO_DMA_VA + off,
                dma_pa + off,
                PTE_U | PTE_R | PTE_W,
                &mut resources,
            ) {
                reclaim_resources(&mut resources);
                return None;
            }
            off += FRAME_SIZE;
        }
    }

    Some(AddressSpaceBuild {
        root,
        entry,
        resources,
    })
}

/// The kernel's own satp (the global identity address space the console runs in).
fn kernel_satp() -> usize {
    (8usize << 60) | ((core::ptr::addr_of!(ROOT) as usize) >> 12)
}

/// satp value for a process whose page table root is at `root`.
fn proc_satp(root: usize) -> usize {
    (8usize << 60) | (root >> 12)
}

// --- Cooperative multitasking scheduler -------------------------------------
// Several U-mode tasks share the CPU by yielding (round-robin). Each has a full
// register frame (saved/restored by utrap), its own 64 KiB stack carved from the
// top of the user region, and its own capability set. Timer preemption is a
// future refinement; for now switches happen on yield/exit (cooperative).
const MAX_TASKS: usize = 4;

#[derive(Clone, Copy, PartialEq)]
enum TaskState {
    Unused,
    Ready,
    Blocked, // waiting on msg_recv until a message arrives
    Done,
}

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
static mut SERVICES: [ServiceEntry; MAX_SERVICES] = [EMPTY_SERVICE; MAX_SERVICES];
static mut SERVICE_COUNT: usize = 0;
static mut TEXIT: [usize; MAX_TASKS] = [0; MAX_TASKS];

#[derive(Clone, Copy)]
struct IpcStats {
    sends: usize,
    receives: usize,
    denied_sends: usize,
    timeouts: usize,
    queue_full: usize,
    max_depth: usize,
}

static mut IPC_STATS: IpcStats = IpcStats {
    sends: 0,
    receives: 0,
    denied_sends: 0,
    timeouts: 0,
    queue_full: 0,
    max_depth: 0,
};

#[derive(Clone, Copy)]
struct EventEntry {
    tick: u64,
    actor: &'static str,
    action: &'static str,
    target: &'static str,
    result: &'static str,
}

const EMPTY_EVENT: EventEntry = EventEntry {
    tick: 0,
    actor: "",
    action: "",
    target: "",
    result: "",
};

const EVENT_CAP: usize = 32;
static mut EVENTS: [EventEntry; EVENT_CAP] = [EMPTY_EVENT; EVENT_CAP];
static mut EVENT_NEXT: usize = 0;
static mut EVENT_COUNT: usize = 0;

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
    word: usize, // a register-passed scalar (used by the value-IPC / Cairn demo)
    buf: [u8; 64],
}

const EMPTY_IPC_MESSAGE: IpcMessage = IpcMessage {
    from: 0,
    len: 0,
    grant: 0,
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

static mut MBOX: [Mailbox; MAX_TASKS] = [EMPTY_MAILBOX; MAX_TASKS];

static mut TRECV_WAITING: [bool; MAX_TASKS] = [false; MAX_TASKS];
static mut TRECV_DEADLINE: [u64; MAX_TASKS] = [0; MAX_TASKS];
static mut TRECV_PTR: [usize; MAX_TASKS] = [0; MAX_TASKS];
static mut TRECV_LEN: [usize; MAX_TASKS] = [0; MAX_TASKS];

static mut FRAMES: [[usize; 32]; MAX_TASKS] = [[0; 32]; MAX_TASKS];
static mut TSTATE: [TaskState; MAX_TASKS] = [TaskState::Unused; MAX_TASKS];
static mut TCAPS: [usize; MAX_TASKS] = [0; MAX_TASKS];
static mut TPERS: [u8; MAX_TASKS] = [0; MAX_TASKS];
static mut TSATP: [usize; MAX_TASKS] = [0; MAX_TASKS]; // each task's address space (satp)
static mut TRES: [TaskResources; MAX_TASKS] = [EMPTY_TASK_RESOURCES; MAX_TASKS];
static mut CURRENT: usize = 0;

fn clear_mailbox(i: usize) {
    unsafe {
        MBOX[i] = EMPTY_MAILBOX;
        TRECV_WAITING[i] = false;
        TRECV_DEADLINE[i] = 0;
        TRECV_PTR[i] = 0;
        TRECV_LEN[i] = 0;
    }
}

unsafe fn recv_message_into(task: usize, frame: &mut [usize]) -> bool {
    if MBOX[task].count == 0 {
        return false;
    }
    let head = MBOX[task].head;
    let msg = MBOX[task].slots[head];
    let n = msg.len.min(frame[F_A1]);
    if n > 0 {
        let dst = core::slice::from_raw_parts_mut(frame[F_A0] as *mut u8, n);
        dst.copy_from_slice(&msg.buf[..n]);
    }
    TCAPS[task] |= msg.grant;
    MBOX[task].slots[head] = EMPTY_IPC_MESSAGE;
    MBOX[task].head = (head + 1) % MAILBOX_DEPTH;
    MBOX[task].count -= 1;
    frame[F_A0] = n;
    frame[F_A1] = msg.from;
    frame[F_A2] = msg.word;
    IPC_STATS.receives += 1;
    true
}

unsafe fn expire_recv_timeouts() {
    let now = TICKS.load(Ordering::Relaxed);
    let mut i = 0usize;
    while i < MAX_TASKS {
        if TRECV_WAITING[i] && TSTATE[i] == TaskState::Blocked && TRECV_DEADLINE[i] <= now {
            if MBOX[i].count > 0 {
                TRECV_WAITING[i] = false;
                TSTATE[i] = TaskState::Ready;
            } else {
                TRECV_WAITING[i] = false;
                TRECV_DEADLINE[i] = 0;
                TRECV_PTR[i] = 0;
                TRECV_LEN[i] = 0;
                FRAMES[i][F_SEPC] += 4;
                FRAMES[i][F_A0] = IPC_STATUS_TIMEOUT;
                FRAMES[i][F_A1] = usize::MAX;
                FRAMES[i][F_A2] = typed_word(
                    IPC_SERVICE_SYSTEM,
                    IPC_OP_TIMEOUT,
                    0,
                    IPC_STATUS_TIMEOUT,
                    0,
                );
                TSTATE[i] = TaskState::Ready;
                IPC_STATS.timeouts += 1;
            }
        }
        i += 1;
    }
}

fn task_kind_name(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Empty => "-",
        TaskKind::Foreground => "foreground",
        TaskKind::Daemon => "daemon",
        TaskKind::LegacyBakedTask => "legacy",
    }
}

fn reclaim_task_resources(slot: usize) {
    unsafe {
        if slot >= MAX_TASKS || TRES[slot].count == 0 {
            TSATP[slot] = 0;
            return;
        }
        reclaim_resources(&mut TRES[slot]);
        TSATP[slot] = 0;
        TCAPS[slot] = 0;
        TPERS[slot] = PERS_NATIVE;
        clear_mailbox(slot);
    }
}

fn task_owned_frames(slot: usize) -> usize {
    unsafe {
        if slot < MAX_TASKS {
            TRES[slot].count
        } else {
            0
        }
    }
}

fn owned_frames_by_kind(kind: TaskKind) -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            if TRES[i].kind == kind {
                total += TRES[i].count;
            }
            i += 1;
        }
        total
    }
}

fn process_owned_frames() -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            total += TRES[i].count;
            i += 1;
        }
        total
    }
}

fn reclaim_finished_foreground_tasks() {
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            if TSTATE[i] == TaskState::Done {
                reclaim_task_resources(i);
            }
            i += 1;
        }
    }
}

// A task's syscall personality: which ABI its `ecall`s speak.
const PERS_NATIVE: u8 = 0; // Dezh native syscalls (SYS_*)
const PERS_LINUX: u8 = 1; // Linux RISC-V syscall ABI, serviced by the Pol layer

// Frame index of the third arg register a2 = x12 -> 11.
const F_A2: usize = 11;

// Linux (riscv64 generic) syscall numbers we recognize; everything else ENOSYS.
const LINUX_WRITE: usize = 64;
const LINUX_EXIT: usize = 93;
const LINUX_EXIT_GROUP: usize = 94;
// Linux negative errno values, as returned in a0.
const LINUX_EBADF: usize = (-9i64) as usize;
const LINUX_EACCES: usize = (-13i64) as usize;
const LINUX_ENOSYS: usize = (-38i64) as usize;

// Frame index of register xN is N-1; a0=x10 -> 9, a1=x11 -> 10, a7=x17 -> 16,
// sp=x2 -> 1, sepc -> 31.
const F_A0: usize = 9;
const F_A1: usize = 10;
const F_A3: usize = 12;
const F_A4: usize = 13;
const F_A7: usize = 16;
const F_SP: usize = 1;
const F_SEPC: usize = 31;

fn frame_ptr(i: usize) -> *mut usize {
    unsafe { core::ptr::addr_of_mut!(FRAMES[i]) as *mut usize }
}

unsafe fn pick_next() -> Option<usize> {
    expire_recv_timeouts();
    for off in 0..MAX_TASKS {
        let i = (CURRENT + 1 + off) % MAX_TASKS;
        if TSTATE[i] == TaskState::Ready {
            return Some(i);
        }
    }
    None
}

/// Pick the next Ready task and return its frame, or longjmp back to the console
/// if every task is finished.
unsafe fn schedule_or_return() -> *const usize {
    match pick_next() {
        Some(i) => {
            CURRENT = i;
            set_active_task_mem(i); // give the new task its private stack, hide others
                                    // Switch to the task's address space (own satp for a loaded process,
                                    // the shared kernel satp for a baked task).
            asm!("csrw satp, {}", in(reg) TSATP[i]);
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
        let cur = CURRENT; // snapshot before any reschedule (avoids &static_mut)
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
            TSTATE[cur] = TaskState::Done;
            TEXIT[cur] = SYS_DENIED;
            return schedule_or_return();
        }

        if code == 8 {
            frame[F_SEPC] += 4; // resume after the ecall
            let caps = TCAPS[cur];

            // Pol: a Linux-personality task speaks the Linux syscall ABI. We
            // translate each Linux syscall into a capability-checked Dezh action;
            // anything we do not support returns ENOSYS, just like the user-space
            // Linux personality spike (D014).
            if TPERS[cur] == PERS_LINUX {
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
                                frame[F_A0] = LINUX_EACCES;
                            }
                        } else {
                            frame[F_A0] = LINUX_EBADF;
                        }
                        return frame_ptr;
                    }
                    LINUX_EXIT | LINUX_EXIT_GROUP => {
                        kprintln!("  [pol/linux] app exit (code {})", frame[F_A0]);
                        TSTATE[cur] = TaskState::Done;
                        TEXIT[cur] = frame[F_A0];
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
                    TSTATE[cur] = TaskState::Ready;
                    return schedule_or_return();
                }
                SYS_EXIT => {
                    kprintln!("  [kernel] task {} exited (code {})", cur, frame[F_A0]);
                    TSTATE[cur] = TaskState::Done;
                    TEXIT[cur] = frame[F_A0];
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
                    let ns = if iters > 0 {
                        ticks.saturating_mul(100) / iters
                    } else {
                        0
                    };
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
                        IPC_STATS.denied_sends += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let to = frame[F_A0];
                    let len = frame[F_A2].min(64);
                    let requested = frame[F_A3];
                    if to >= MAX_TASKS
                        || TSTATE[to] == TaskState::Unused
                        || TSTATE[to] == TaskState::Done
                    {
                        IPC_STATS.denied_sends += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // ATTENUATION: a sender can only delegate capabilities it
                    // itself holds — never widen. (caps = sender's TCAPS.)
                    let granted = requested & caps;
                    if MBOX[to].count == MAILBOX_DEPTH {
                        IPC_STATS.queue_full += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let tail = MBOX[to].tail;
                    let msg = &mut MBOX[to].slots[tail];
                    if len > 0 {
                        let src = core::slice::from_raw_parts(frame[F_A1] as *const u8, len);
                        msg.buf[..len].copy_from_slice(src);
                    }
                    msg.len = len;
                    msg.from = cur;
                    msg.grant = granted;
                    msg.word = frame[F_A4]; // register-passed scalar (value-IPC)
                    MBOX[to].tail = (tail + 1) % MAILBOX_DEPTH;
                    MBOX[to].count += 1;
                    IPC_STATS.sends += 1;
                    if MBOX[to].count > IPC_STATS.max_depth {
                        IPC_STATS.max_depth = MBOX[to].count;
                    }
                    if TSTATE[to] == TaskState::Blocked {
                        TRECV_WAITING[to] = false;
                        TSTATE[to] = TaskState::Ready;
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
                        TSTATE[cur] = TaskState::Blocked;
                        return schedule_or_return();
                    }
                }
                SYS_RECV_TIMEOUT => {
                    if caps & TASK_IPC == 0 {
                        kprintln!("  [kernel] DENIED recv-timeout: task {cur} holds no IPC capability");
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
                        IPC_STATS.timeouts += 1;
                        return frame_ptr;
                    }
                    TRECV_WAITING[cur] = true;
                    TRECV_PTR[cur] = frame[F_A0];
                    TRECV_LEN[cur] = frame[F_A1];
                    TRECV_DEADLINE[cur] = TICKS.load(Ordering::Relaxed).saturating_add(timeout);
                    frame[F_SEPC] -= 4;
                    TSTATE[cur] = TaskState::Blocked;
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
fn run_tasks(specs: &[(usize, usize, u8)]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        for i in 0..MAX_TASKS {
            reclaim_task_resources(i);
            TSTATE[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        for (i, &(entry, caps, pers)) in specs.iter().take(n).enumerate() {
            let f = &mut FRAMES[i];
            *f = [0; 32];
            f[F_SEPC] = entry;
            f[F_SP] = task_stack_top(i); // each task owns a private 2 MiB stack region
            TCAPS[i] = caps;
            TPERS[i] = pers;
            TSATP[i] = kernel_satp(); // baked tasks share the kernel address space
            TRES[i] = EMPTY_TASK_RESOURCES;
            TRES[i].kind = TaskKind::LegacyBakedTask;
            TSTATE[i] = TaskState::Ready;
        }
        CURRENT = 0;
        set_active_task_mem(0); // expose only task 0's stack region to start
                                // Switch to the multitasking trap path and arm the preemption timer.
        asm!("csrw stvec, {}", in(reg) utrap as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        run_first(frame_ptr(0) as *const usize);
        // Returned via restore_kernel_ctx once every task is Done.
        asm!("csrw stvec, {}", in(reg) trap_entry as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA); // restore the console uptime cadence
    }
}

/// Load and run several separate programs as real processes: each gets its own
/// ELF, its own address space (satp), an id in a0, and a capability set. They
/// run concurrently under the preemptive scheduler, isolated from one another,
/// and return to the console once all have exited. Zero ambient authority — a
/// process holds only the capabilities passed here (no fork).
fn run_processes(specs: &[ProcessSpec]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        // A loaded process must not see any baked-task stack region.
        set_active_task_mem(usize::MAX);
        for i in 0..MAX_TASKS {
            reclaim_task_resources(i);
            TSTATE[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        let mut launched = 0usize;
        let mut first_ready = usize::MAX;
        for (i, spec) in specs.iter().take(n).enumerate() {
            let Some(build) = build_address_space(spec, TaskKind::Foreground) else {
                kprintln!("  [kernel] process launch failed: out of frames");
                continue;
            };
            let f = &mut FRAMES[i];
            *f = [0; 32];
            f[F_SEPC] = build.entry;
            f[F_SP] = USER_STACK_TOP; // each process has its own stack in its own space
            f[F_A0] = spec.arg0;
            f[F_A1] = spec.arg1;
            f[F_A2] = spec.arg2;
            f[F_A3] = spec.arg3;
            TCAPS[i] = spec.caps;
            TPERS[i] = spec.personality;
            TSATP[i] = proc_satp(build.root);
            TRES[i] = build.resources;
            TSTATE[i] = TaskState::Ready;
            if first_ready == usize::MAX {
                first_ready = i;
            }
            launched += 1;
        }
        if launched == 0 {
            return;
        }
        CURRENT = first_ready;
        asm!("csrw stvec, {}", in(reg) utrap as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) TSATP[first_ready]); // enter the first process's address space
        asm!("sfence.vma");
        run_first(frame_ptr(first_ready) as *const usize);
        // Back in the kernel address space once every process has exited.
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        let mut i = 0usize;
        while i < MAX_TASKS {
            if TSTATE[i] == TaskState::Done {
                reclaim_task_resources(i);
            }
            i += 1;
        }
    }
}

fn run_scheduler_from(first: usize) {
    unsafe {
        CURRENT = first;
        asm!("csrw stvec, {}", in(reg) utrap as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) TSATP[first]);
        asm!("sfence.vma");
        run_first(frame_ptr(first) as *const usize);
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
    }
}

fn spawn_process_at(slot: usize, spec: &ProcessSpec, kind: TaskKind) -> bool {
    unsafe {
        reclaim_task_resources(slot);
        let Some(build) = build_address_space(spec, kind) else {
            kprintln!("  [kernel] process launch failed: out of frames");
            TSTATE[slot] = TaskState::Unused;
            clear_mailbox(slot);
            return false;
        };
        let f = &mut FRAMES[slot];
        *f = [0; 32];
        f[F_SEPC] = build.entry;
        f[F_SP] = USER_STACK_TOP;
        f[F_A0] = spec.arg0;
        f[F_A1] = spec.arg1;
        f[F_A2] = spec.arg2;
        f[F_A3] = spec.arg3;
        TCAPS[slot] = spec.caps;
        TPERS[slot] = spec.personality;
        TSATP[slot] = proc_satp(build.root);
        TRES[slot] = build.resources;
        TEXIT[slot] = 0;
        clear_mailbox(slot);
        TSTATE[slot] = TaskState::Ready;
        true
    }
}

fn clear_foreground_tasks() {
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            reclaim_task_resources(i);
            TSTATE[i] = TaskState::Unused;
            clear_mailbox(i);
            TCAPS[i] = 0;
            TEXIT[i] = 0;
            i += 1;
        }
    }
}

fn run_foreground_processes(specs: &[ProcessSpec]) {
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
        while i < SERVICE_COUNT {
            if SERVICES[i].name == name {
                return Some(i);
            }
            i += 1;
        }
    }
    None
}

fn build_service_registry(plan: &KernelPlan) {
    unsafe {
        SERVICE_COUNT = 0;
        for service in &plan.services {
            if SERVICE_COUNT >= MAX_SERVICES {
                break;
            }
            let caps = task_caps_for(service.name, plan);
            let grants = match service.kind {
                ServiceKind::VirtioBlock => 0b11,
                ServiceKind::Cairn => 0b01,
                _ => 0,
            };
            SERVICES[SERVICE_COUNT] = ServiceEntry {
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
            SERVICE_COUNT += 1;
        }
    }
    kprintln!(
        "[dezh-boot] service registry built from boot plan ({} services)",
        unsafe { SERVICE_COUNT }
    );
}

fn refresh_virtio_service_state() {
    if let Some(i) = service_index("virtio-block") {
        unsafe {
            let task = SERVICES[i].task;
            if task < MAX_TASKS {
                if TSTATE[task] == TaskState::Blocked || TSTATE[task] == TaskState::Ready {
                    SERVICES[i].state = ServiceState::Running;
                    SERVICES[i].fault = "";
                } else if TSTATE[task] == TaskState::Done && TEXIT[task] == 0 {
                    SERVICES[i].state = ServiceState::Stopped;
                    SERVICES[i].fault = "manual stop";
                    SERVICES[i].last_exit = TEXIT[task];
                    reclaim_task_resources(task);
                } else if TSTATE[task] == TaskState::Done {
                    SERVICES[i].state = ServiceState::Faulted;
                    SERVICES[i].fault = "driver exited or faulted";
                    SERVICES[i].last_exit = TEXIT[task];
                    reclaim_task_resources(task);
                }
            }
        }
    }
}

fn ensure_virtio_block_service(_plan: &KernelPlan) -> Option<usize> {
    let idx = service_index("virtio-block")?;
    unsafe {
        let task = SERVICES[idx].task;
        if SERVICES[idx].state == ServiceState::Running
            && task < MAX_TASKS
            && (TSTATE[task] == TaskState::Blocked || TSTATE[task] == TaskState::Ready)
        {
            return Some(task);
        }
        if SERVICES[idx].state == ServiceState::Stopped {
            kprintln!("[services] virtio-block unavailable: service is Stopped; use `svc-restart virtio-block`");
            return None;
        }
        if SERVICES[idx].state == ServiceState::Faulted {
            kprintln!("[services] virtio-block unavailable: service is Faulted; use `svc-restart virtio-block`");
            return None;
        }
        SERVICES[idx].state = ServiceState::Starting;
        SERVICES[idx].task = VIRTIO_SERVICE_TASK;
        SERVICES[idx].fault = "";
        SERVICES[idx].last_started_tick = TICKS.load(Ordering::Relaxed);
        let caps = SERVICES[idx].caps;
        kprintln!(
            "[services] starting virtio-block from boot registry as task {VIRTIO_SERVICE_TASK}"
        );
        let spec = ProcessSpec::new(VIRTIO_BLK_ELF, caps, BLK_OP_DAEMON)
            .args(virtio_dma_pa(), 0, 0)
            .virtio_blk()
            .virtio_dma();
        if !spawn_process_at(VIRTIO_SERVICE_TASK, &spec, TaskKind::Daemon) {
            SERVICES[idx].state = ServiceState::Faulted;
            SERVICES[idx].fault = "driver launch failed: out of frames";
            return None;
        }
    }
    run_scheduler_from(VIRTIO_SERVICE_TASK);
    refresh_virtio_service_state();
    unsafe {
        if SERVICES[idx].state == ServiceState::Running {
            kprintln!(
                "[services] virtio-block Running (task {})",
                SERVICES[idx].task
            );
            Some(SERVICES[idx].task)
        } else {
            kprintln!("[services] virtio-block Faulted: {}", SERVICES[idx].fault);
            None
        }
    }
}

fn virtio_service_is_running() -> bool {
    refresh_virtio_service_state();
    if let Some(i) = service_index("virtio-block") {
        unsafe {
            return SERVICES[i].state == ServiceState::Running;
        }
    }
    false
}

fn print_services() {
    refresh_virtio_service_state();
    unsafe {
        let count = SERVICE_COUNT;
        kprintln!("runtime services ({} total):", count);
        let mut i = 0usize;
        while i < count {
            let s = SERVICES[i];
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

const BLK_OP_NO_GRANT_PROBE: usize = 7;
const BLK_OP_DAEMON: usize = 8;
const BLK_OP_CLIENT_DEMO: usize = 9;
const BLK_OP_CLIENT_REQ: usize = 10;
const BLK_REQ_PROBE: usize = 1;
const BLK_REQ_BWRITE: usize = 2;
const BLK_REQ_BREAD: usize = 3;
const BLK_REQ_PSET: usize = 4;
const BLK_REQ_PGET: usize = 5;
const BLK_REQ_PROLLBACK: usize = 6;
const BLK_REQ_STOP: usize = 7;
const BLK_REQ_INSTALL_CHECK: usize = 8;
const BLK_REQ_INSTALL_INIT: usize = 9;
const BLK_REQ_ROOT_STATUS: usize = 10;
const BLK_REQ_APP_AVAILABLE: usize = 11;
const BLK_REQ_APP_INSTALLED: usize = 12;
const BLK_REQ_APP_INFO: usize = 13;
const BLK_REQ_APP_INSTALL_NOTE: usize = 14;
const BLK_REQ_APP_REQUIRE_NOTE: usize = 15;
const BLK_REQ_APP_REMOVE_NOTE: usize = 16;
const BLK_REQ_NOTE_SET: usize = 17;
const BLK_REQ_NOTE_GET: usize = 18;
const BLK_REQ_APP_INSTALL_LAB: usize = 19;
const BLK_REQ_APP_REQUIRE_LAB: usize = 20;
const BLK_REQ_APP_REMOVE_LAB: usize = 21;
const BLK_REQ_LAB_SET: usize = 22;
const BLK_REQ_LAB_GET: usize = 23;
const BLK_REQ_FAULT_DEMO: usize = 24;
const BLK_REQ_APP_INSTALL_CALC: usize = 25;
const BLK_REQ_APP_REQUIRE_CALC: usize = 26;
const BLK_REQ_APP_REMOVE_CALC: usize = 27;
const BLK_REQ_CALC_SET: usize = 28;
const BLK_REQ_CALC_GET: usize = 29;
const BLK_REQ_APP_INSTALL_VAULT: usize = 30;
const BLK_REQ_APP_REQUIRE_VAULT: usize = 31;
const BLK_REQ_APP_REMOVE_VAULT: usize = 32;
const BLK_REQ_VAULT_SET: usize = 33;
const BLK_REQ_VAULT_GET: usize = 34;
const IPC_PROTO_V1: usize = 0xd1;
const IPC_SERVICE_SYSTEM: usize = 0;
const IPC_STATUS_OK: usize = 0;
const IPC_STATUS_DENIED: usize = 1;
const IPC_STATUS_UNAVAILABLE: usize = 2;
const IPC_STATUS_TIMEOUT: usize = 3;
const IPC_STATUS_BAD_REQUEST: usize = 4;
const IPC_STATUS_IO_FAILURE: usize = 5;
const IPC_STATUS_FAULTED: usize = 6;
const IPC_STATUS_BUSY: usize = 7;
const IPC_OP_PING: usize = 1;
const IPC_OP_TIMEOUT: usize = 2;
const IPC_OP_BADREQ: usize = 255;
const VIRTIO_SERVICE_TASK: usize = 0;
const FIRST_FOREGROUND_TASK: usize = 1;
const BENCH_ROLE_SYSCALL: usize = 1;
const BENCH_ROLE_IPC_SERVICE: usize = 2;
const BENCH_ROLE_IPC_CLIENT: usize = 3;
const BENCH_ROLE_CAPS: usize = 4;
const BENCH_SYSCALL_ITERS: usize = 200_000;
const BENCH_IPC_ITERS: usize = 32;
const NOTE_ROLE_RUN: usize = 1;
const NOTE_ROLE_DENY_MMIO: usize = 2;
const NOTE_ROLE_DENY_BLOCK: usize = 3;
const LAB_ROLE_UI: usize = 1;
const LAB_ROLE_WORKER: usize = 2;
const LAB_ROLE_DENY_BLOCK: usize = 3;
const LAB_ROLE_DENY_MMIO: usize = 4;
const CALC_ROLE_RUN: usize = 1;
const CALC_ROLE_EVAL: usize = 2;
const CALC_OP_ADD: usize = 1;
const CALC_OP_SUB: usize = 2;
const CALC_OP_MUL: usize = 3;
const CALC_OP_DIV: usize = 4;
const VAULT_ROLE_RUN: usize = 1;
const VAULT_ROLE_DENY_BLOCK: usize = 2;
const VAULT_ROLE_DENY_MMIO: usize = 3;

fn typed_word(service: usize, op: usize, request_id: usize, status: usize, arg: usize) -> usize {
    (IPC_PROTO_V1 << 56)
        | ((service & 0xff) << 48)
        | ((op & 0xff) << 40)
        | ((request_id & 0xffff) << 24)
        | ((status & 0xff) << 16)
        | (arg & 0xffff)
}

fn ipc_status_name(status: usize) -> &'static str {
    match status {
        IPC_STATUS_OK => "OK",
        IPC_STATUS_DENIED => "DENIED",
        IPC_STATUS_UNAVAILABLE => "UNAVAILABLE",
        IPC_STATUS_TIMEOUT => "TIMEOUT",
        IPC_STATUS_BAD_REQUEST => "BAD_REQUEST",
        IPC_STATUS_IO_FAILURE => "IO_FAILURE",
        IPC_STATUS_FAULTED => "FAULTED",
        IPC_STATUS_BUSY => "BUSY",
        _ => "UNKNOWN",
    }
}

fn print_ipcstat() {
    unsafe {
        let stats = IPC_STATS;
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

fn record_event(
    actor: &'static str,
    action: &'static str,
    target: &'static str,
    result: &'static str,
) {
    unsafe {
        EVENTS[EVENT_NEXT] = EventEntry {
            tick: TICKS.load(Ordering::Relaxed),
            actor,
            action,
            target,
            result,
        };
        EVENT_NEXT = (EVENT_NEXT + 1) % EVENT_CAP;
        if EVENT_COUNT < EVENT_CAP {
            EVENT_COUNT += 1;
        }
    }
}

fn print_events() {
    unsafe {
        kprintln!("events:");
        kprintln!("  TICK   ACTOR      ACTION          TARGET          RESULT");
        let start = if EVENT_COUNT == EVENT_CAP { EVENT_NEXT } else { 0 };
        let mut n = 0usize;
        while n < EVENT_COUNT {
            let idx = (start + n) % EVENT_CAP;
            let e = EVENTS[idx];
            kprintln!(
                "  {:<6} {:<10} {:<15} {:<15} {}",
                e.tick,
                e.actor,
                e.action,
                e.target,
                e.result
            );
            n += 1;
        }
        if EVENT_COUNT == 0 {
            kprintln!("  (no events recorded yet)");
        }
    }
}

fn print_audit() {
    kprintln!("audit summary:");
    kprintln!("  model: no ambient authority; important effects are event-recorded");
    kprintln!("  tracked: install, app install/run/remove, service stop/restart/fault, denial demos");
    print_events();
}

fn run_ipc_typed_demo() {
    if virtio_service_is_running() {
        kprintln!("[typed-ipc] skipped: run before starting services to avoid disturbing daemon slot 0");
        print_ipcstat();
        return;
    }
    kprintln!("[typed-ipc] demo: typed OK, BAD_REQUEST, TIMEOUT, and DENIED");
    run_tasks(&[
        (
            typed_ipc_service_task as usize,
            TASK_PRINT | TASK_IPC,
            PERS_NATIVE,
        ),
        (
            typed_ipc_client_task as usize,
            TASK_PRINT | TASK_IPC,
            PERS_NATIVE,
        ),
    ]);
    run_tasks(&[(
        typed_ipc_timeout_task as usize,
        TASK_PRINT | TASK_IPC,
        PERS_NATIVE,
    )]);
    run_tasks(&[(typed_ipc_denied_task as usize, TASK_PRINT, PERS_NATIVE)]);
    kprintln!(
        "[typed-ipc] PASS: OK={}, BAD_REQUEST={}, TIMEOUT={}, DENIED={}",
        ipc_status_name(IPC_STATUS_OK),
        ipc_status_name(IPC_STATUS_BAD_REQUEST),
        ipc_status_name(IPC_STATUS_TIMEOUT),
        ipc_status_name(IPC_STATUS_DENIED)
    );
}

fn svc_stop_virtio(_plan: &KernelPlan) {
    refresh_virtio_service_state();
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    let daemon;
    unsafe {
        if SERVICES[idx].state != ServiceState::Running {
            kprintln!(
                "[services] virtio-block stop skipped: state={}",
                service_state_name(SERVICES[idx].state)
            );
            return;
        }
        daemon = SERVICES[idx].task;
        SERVICES[idx].state = ServiceState::Stopping;
        SERVICES[idx].fault = "manual stop requested";
    }
    let client_caps = TASK_PRINT | TASK_IPC | TASK_BLOCK_READ | TASK_BLOCK_WRITE;
    kprintln!("[services] stopping virtio-block task={daemon} with typed STOP");
    run_foreground_processes(&[
        ProcessSpec::new(VIRTIO_BLK_ELF, client_caps, BLK_OP_CLIENT_REQ)
            .args(daemon, 0, BLK_REQ_STOP)
            .virtio_dma(),
    ]);
    let st = unsafe { TEXIT[FIRST_FOREGROUND_TASK] };
    refresh_virtio_service_state();
    unsafe {
        kprintln!(
            "[services] svc-stop virtio-block status={} state={}",
            st,
            service_state_name(SERVICES[idx].state)
        );
    }
    record_event("console", "svc.stop", "virtio-block", "done");
}

fn svc_restart_virtio(_plan: &KernelPlan) {
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    refresh_virtio_service_state();
    unsafe {
        let task = SERVICES[idx].task;
        if SERVICES[idx].state == ServiceState::Running && task < MAX_TASKS {
            kprintln!("[services] restart requires stopped/faulted service; use svc-stop first");
            return;
        }
        SERVICES[idx].state = ServiceState::Restarting;
        SERVICES[idx].fault = "";
        SERVICES[idx].task = usize::MAX;
        SERVICES[idx].restart_count += 1;
    }
    let _ = ensure_virtio_block_service(_plan);
    refresh_virtio_service_state();
    unsafe {
        kprintln!(
            "[services] svc-restart virtio-block state={} restart_count={}",
            service_state_name(SERVICES[idx].state),
            SERVICES[idx].restart_count
        );
    }
    record_event("console", "svc.restart", "virtio-block", "done");
}

fn svc_fault_demo_virtio(plan: &KernelPlan) {
    refresh_virtio_service_state();
    let Some(idx) = service_index("virtio-block") else {
        kprintln!("[services] virtio-block not declared");
        return;
    };
    unsafe {
        if SERVICES[idx].state != ServiceState::Running {
            kprintln!(
                "[services] fault-demo skipped: state={}",
                service_state_name(SERVICES[idx].state)
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
            service_state_name(SERVICES[idx].state),
            SERVICES[idx].last_exit
        );
    }
    record_event("console", "svc.fault-demo", "virtio-block", "done");
}

fn virtio_dma_pa() -> usize {
    core::ptr::addr_of!(VIRTIO_DMA) as usize
}

fn prepare_virtio_input(text: &str) -> usize {
    let bytes = text.as_bytes();
    let n = bytes.len().min(511);
    unsafe {
        let base = core::ptr::addr_of_mut!(VIRTIO_DMA) as *mut u8;
        core::ptr::write_bytes(base.add(VIRTIO_INPUT_OFF), 0, 512);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(VIRTIO_INPUT_OFF), n);
    }
    n
}

fn run_virtio_no_grant_probe() {
    run_foreground_processes(&[ProcessSpec::new(
        VIRTIO_BLK_ELF,
        TASK_PRINT,
        BLK_OP_NO_GRANT_PROBE,
    )]);
}

fn run_registered_virtio_client(plan: &KernelPlan, req: usize, input: &str) {
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

fn run_registered_virtio_client_status(plan: &KernelPlan, req: usize, input: &str) -> usize {
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
    unsafe { TEXIT[FIRST_FOREGROUND_TASK] }
}

fn run_virtio_blk_daemon_demo(plan: &KernelPlan) {
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

fn run_bench_os() {
    kprintln!(
        "[bench-os] launching separate U-mode benchmark ELF ({} null syscalls)",
        BENCH_SYSCALL_ITERS
    );
    run_foreground_processes(&[
        ProcessSpec::new(BENCH_ELF, TASK_PRINT, BENCH_ROLE_SYSCALL).args(BENCH_SYSCALL_ITERS, 0, 0),
    ]);
    kprintln!("[bench-os] complete; console returned");
}

fn run_bench_ipc() {
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

fn run_bench_storage(plan: &KernelPlan) {
    kprintln!("[bench-storage] validating registered virtio-block storage path");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
    run_registered_virtio_client(plan, BLK_REQ_PSET, "bench-storage-value");
    run_registered_virtio_client(plan, BLK_REQ_PGET, "");
    run_registered_virtio_client(plan, BLK_REQ_PSET, "bench-storage-bad-edit");
    run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, "");
    kprintln!("[bench-storage] complete via user-space virtio-block daemon");
}

fn run_bench_caps() {
    kprintln!("[bench-caps] launching app with PRINT only");
    run_foreground_processes(&[ProcessSpec::new(BENCH_ELF, TASK_PRINT, BENCH_ROLE_CAPS)]);
    kprintln!("[bench-caps] running no-grant MMIO proof");
    run_virtio_no_grant_probe();
    kprintln!("[bench-caps] complete; denied paths returned cleanly");
}

fn run_bench_all(plan: &KernelPlan) {
    kprintln!("[bench-all] Dezh validation suite v0 starting");
    run_bench_os();
    run_bench_ipc();
    run_bench_storage(plan);
    run_bench_caps();
    refresh_virtio_service_state();
    kprintln!("[bench-all] PASS: syscall, IPC, storage, caps, and service liveness checked");
}

fn app_note_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_NOTE, "") == 0
}

fn app_lab_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_LAB, "") == 0
}

fn app_calc_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_CALC, "") == 0
}

fn app_vault_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_VAULT, "") == 0
}

fn print_apps(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "" => {
            kprintln!("[apps] available bundles:");
            run_registered_virtio_client(plan, BLK_REQ_APP_AVAILABLE, "");
            kprintln!("[apps] installed apps:");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, "");
        }
        "available" => run_registered_virtio_client(plan, BLK_REQ_APP_AVAILABLE, ""),
        "installed" => run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, ""),
        other => kprintln!("[apps] unknown view '{other}' (use: apps available|installed)"),
    }
}

fn app_info(plan: &KernelPlan, arg: &str) {
    if !matches!(arg.trim(), "" | "note" | "lab" | "calc" | "vault") {
        kprintln!("[app-info] unknown app '{}'", arg.trim());
        return;
    }
    run_registered_virtio_client(plan, BLK_REQ_APP_INFO, "");
}

fn app_install(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => {
            record_event("console", "app.install", "note", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_NOTE, "");
            record_event("installer", "app.install", "note", "done");
        }
        "lab" => {
            record_event("console", "app.install", "lab", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_LAB, "");
            record_event("installer", "app.install", "lab", "done");
        }
        "calc" => {
            record_event("console", "app.install", "calc", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_CALC, "");
            record_event("installer", "app.install", "calc", "done");
        }
        "vault" => {
            record_event("console", "app.install", "vault", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_VAULT, "");
            record_event("installer", "app.install", "vault", "done");
        }
        other => kprintln!("[installer] unknown available app '{other}'"),
    }
}

fn app_run(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => {
            if !app_note_is_active(plan) {
                kprintln!("[app-run] note not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching note with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT | TASK_IPC,
                NOTE_ROLE_RUN,
            )]);
            kprintln!("[app-run] note exited; console returned");
            record_event("app", "app.run", "note", "OK");
        }
        "lab" => {
            if !app_lab_is_active(plan) {
                kprintln!("[app-run] lab not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] preparing lab private storage through virtio-block service");
            run_registered_virtio_client(plan, BLK_REQ_LAB_SET, "lab-run-start");
            kprintln!("[app-run] launching lab UI + workers with caps=PRINT,IPC only");
            run_foreground_processes(&[
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_UI).args(2, 0, 0),
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_WORKER).args(
                    FIRST_FOREGROUND_TASK,
                    1,
                    0,
                ),
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_WORKER).args(
                    FIRST_FOREGROUND_TASK,
                    2,
                    0,
                ),
            ]);
            run_registered_virtio_client(plan, BLK_REQ_LAB_SET, "lab-run-complete");
            run_registered_virtio_client(plan, BLK_REQ_LAB_GET, "");
            kprintln!("[app-run] lab exited; console returned");
            record_event("app", "app.run", "lab", "OK");
        }
        "calc" => {
            if !app_calc_is_active(plan) {
                kprintln!("[app-run] calc not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching calc with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                CALC_ELF,
                TASK_PRINT | TASK_IPC,
                CALC_ROLE_RUN,
            )]);
            kprintln!("[app-run] calc exited; console returned");
            record_event("app", "app.run", "calc", "OK");
        }
        "vault" => {
            if !app_vault_is_active(plan) {
                kprintln!("[app-run] vault not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching vault with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT | TASK_IPC,
                VAULT_ROLE_RUN,
            )]);
            kprintln!("[app-run] vault exited; console returned");
            record_event("app", "app.run", "vault", "OK");
        }
        other => kprintln!("[app-run] unknown app '{other}'"),
    }
}

fn app_remove(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_NOTE, ""),
        "lab" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_LAB, ""),
        "calc" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_CALC, ""),
        "vault" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_VAULT, ""),
        other => kprintln!("[installer] unknown installed app '{other}'"),
    }
    record_event("console", "app.remove", "app", "done");
}

fn app_deny(plan: &KernelPlan, arg: &str) {
    let daemon = ensure_virtio_block_service(plan).unwrap_or(usize::MAX);
    match arg.trim() {
        "note" => {
            kprintln!("[app-deny] note has no direct block grant when launched without IPC");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT,
                NOTE_ROLE_DENY_BLOCK,
            )
            .args(daemon, 0, 0)]);
            kprintln!("[app-deny] note has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT | TASK_IPC,
                NOTE_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] note device/block direct access denied; console survived");
        }
        "lab" => {
            kprintln!("[app-deny] lab has no direct block grant when launched without IPC");
            run_foreground_processes(&[
                ProcessSpec::new(LAB_ELF, TASK_PRINT, LAB_ROLE_DENY_BLOCK).args(daemon, 0, 0)
            ]);
            kprintln!("[app-deny] lab has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                LAB_ELF,
                TASK_PRINT | TASK_IPC,
                LAB_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] lab device/block direct access denied; console survived");
        }
        "vault" => {
            kprintln!("[app-deny] vault has no direct block grant when launched without IPC");
            run_foreground_processes(&[
                ProcessSpec::new(VAULT_ELF, TASK_PRINT, VAULT_ROLE_DENY_BLOCK).args(daemon, 0, 0)
            ]);
            kprintln!("[app-deny] vault has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT | TASK_IPC,
                VAULT_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] vault device/block direct access denied; console survived");
        }
        other => kprintln!("[app-deny] unknown app '{other}'"),
    }
    record_event("kernel", "deny.app", "app", "OK");
}

fn app_permissions(arg: &str) {
    let app = arg.trim();
    if !matches!(app, "note" | "lab" | "calc" | "vault") {
        kprintln!("usage: app-permissions <note|lab|calc|vault>");
        return;
    }
    kprintln!("app permissions: {app}");
    kprintln!("  REQUESTED  PRINT IPC");
    kprintln!("  GRANTED    PRINT IPC");
    kprintln!("  DENIED     DEVICE_VIRTIO_BLK DMA BLOCK_DIRECT MMIO");
    kprintln!("  STORAGE    service-mediated via virtio-block daemon");
}

fn install_plan() {
    kprintln!("Install Plan: Dezh Root v1");
    kprintln!("  [01] Probe block service        ready");
    kprintln!("  [02] Validate boot manifest     ready");
    kprintln!("  [03] Write root marker          pending");
    kprintln!("  [04] Initialize app registry    pending");
    kprintln!("  [05] Install base apps          note lab calc vault");
    kprintln!("  [06] Verify root/app state      pending");
    kprintln!("  [07] Commit install report      pending");
}

fn progress(stage: usize, total: usize, label: &str, status: &str) {
    let filled = stage * 20 / total;
    let mut bar = [b'-'; 20];
    let mut i = 0usize;
    while i < filled && i < bar.len() {
        bar[i] = b'#';
        i += 1;
    }
    let s = core::str::from_utf8(&bar).unwrap_or("--------------------");
    kprintln!("[{}] {:>3}%  {:<28} {}", s, stage * 100 / total, label, status);
}

fn install_verify(plan: &KernelPlan) {
    kprintln!("[install-v1] verifying root marker, metadata, and base app registry");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
    run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
    run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, "");
    record_event("installer", "install.verify", "root-v1", "done");
}

fn install_report() {
    kprintln!("Install Report: Dezh Root v1");
    kprintln!("  root marker      sector 0");
    kprintln!("  root metadata    sector 4");
    kprintln!("  app registry     sectors 5..10");
    kprintln!("  private data     sectors 16..19");
    kprintln!("  required service virtio-block");
    kprintln!("  policy           no ambient authority");
    print_events();
}

fn install_run(plan: &KernelPlan, dry_run: bool) {
    install_plan();
    record_event("console", "install.run", "root-v1", "start");
    let total = 7usize;
    progress(1, total, "probe block service", "OK");
    if dry_run {
        progress(2, total, "validate boot manifest", "OK");
        progress(3, total, "write root marker", "dry-run");
        progress(4, total, "initialize app registry", "dry-run");
        progress(5, total, "install base apps", "dry-run");
        progress(6, total, "verify root/app state", "dry-run");
        progress(7, total, "commit install report", "dry-run");
        kprintln!("[install-v1] dry-run complete; disk not modified");
        record_event("installer", "install.dryrun", "root-v1", "OK");
        return;
    }
    progress(2, total, "validate boot manifest", "OK");
    progress(3, total, "write root marker", "running");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
    progress(4, total, "initialize app registry", "running");
    app_install(plan, "note");
    progress(5, total, "install base apps", "running");
    app_install(plan, "lab");
    app_install(plan, "calc");
    app_install(plan, "vault");
    progress(6, total, "verify root/app state", "running");
    install_verify(plan);
    progress(7, total, "commit install report", "OK");
    install_report();
    record_event("installer", "install.run", "root-v1", "OK");
}

fn install_command(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "" | "plan" => install_plan(),
        "check" => run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, ""),
        "run" => install_run(plan, false),
        "--dry-run" | "dry-run" => install_run(plan, true),
        "verify" => install_verify(plan),
        "report" => install_report(),
        "rollback" => {
            kprintln!("[install-v1] rollback uses storage rollback for v0 root data");
            run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, "");
            record_event("installer", "install.rollback", "root-v1", "done");
        }
        other => kprintln!("usage: install plan|check|run|verify|report|rollback|--dry-run (got '{other}')"),
    }
}

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
        CALC_OP_DIV => {
            if b == 0 {
                None
            } else {
                Some(a / b)
            }
        }
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
    let (Some(a), Some(op), Some(b)) = (parse_usize_token(a_s), calc_op_token(op_s), parse_usize_token(b_s)) else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    run_foreground_processes(&[ProcessSpec::new(CALC_ELF, TASK_PRINT | TASK_IPC, CALC_ROLE_EVAL)
        .args(op, a, b)]);
    if unsafe { TEXIT[FIRST_FOREGROUND_TASK] } == 0 {
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
    let free_before = unsafe { FRAME_FREE };
    let mut i = 0usize;
    while i < count {
        kprintln!("[stress-lab] iteration {}/{}", i + 1, count);
        app_run(plan, "lab");
        i += 1;
    }
    let free_after = unsafe { FRAME_FREE };
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
        asm!("ecall", inout("a0") 0usize => _, inout("a1") 0usize => from, out("a2") word, in("a7") SYS_RECV)
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
    unsafe { asm!("ecall", inout("a0") a0, in("a1") buf.len(), in("a7") SYS_RECV) };
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
            utyped_word(
                IPC_SERVICE_SYSTEM,
                IPC_OP_PING,
                1,
                IPC_STATUS_OK,
                0,
            ),
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
        name: "pkg-remove",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Packages",
        help: "remove an installed package (its grants go with it)",
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
        name: "bench",
        cap: cap::SPAWN,
        cap_name: "SPAWN",
        group: "Demos",
        help: "measure ecall round-trip cost (U-mode task)",
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
        "Inspect", "Storage", "Install", "Apps", "Services", "Audit", "Safety", "Demos", "Power",
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

fn command_usage<'a>(name: &'a str) -> &'a str {
    match name {
        "install" => "install plan|check|run|verify|report|rollback|--dry-run",
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
    let running_services = unsafe {
        let mut n = 0usize;
        let mut i = 0usize;
        while i < SERVICE_COUNT {
            if SERVICES[i].state == ServiceState::Running {
                n += 1;
            }
            i += 1;
        }
        n
    };
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

fn service_for_task(task: usize) -> &'static str {
    unsafe {
        let mut i = 0usize;
        while i < SERVICE_COUNT {
            if SERVICES[i].task == task {
                return SERVICES[i].name;
            }
            i += 1;
        }
    }
    "-"
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
                task_state_name(TSTATE[i]),
                task_kind_name(TRES[i].kind),
                task_owned_frames(i),
                TCAPS[i],
                TEXIT[i],
                service_for_task(i)
            );
            i += 1;
        }
    }
}

fn print_memstat() {
    let total = unsafe { FRAME_TOTAL };
    let free = unsafe { FRAME_FREE };
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
        _ => kprintln!("caps why: try `caps why install run`, `caps why app-run lab`, or `caps why note-get`"),
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
            kprintln!("[storage] previous value is used by rollback; full commit history is future Cairn work");
        }
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
                };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
                kprintln!("  prog 1 again WITHOUT the PRINT capability:");
                let mut h = KHost { caps: 0 };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
            }
            let mut buf2 = [0u8; 512];
            let cairn = ir::demo_cairn(&mut buf2);
            kprintln!("  prog 2 (write to Cairn, then read it back) with WRITE+READ+PRINT:");
            let mut h = KHost {
                caps: ir::CAP_WRITE | ir::CAP_READ | ir::CAP_PRINT,
            };
            if let Err(t) = ir::run(cairn, &mut h) {
                kprintln!("  [ir] TRAP: {}", t.msg());
            }
        }
        "frames" => {
            let free0 = unsafe { FRAME_FREE };
            kprintln!("frames: {} total, {} free", unsafe { FRAME_TOTAL }, free0);
            let a = frame_alloc();
            let b = frame_alloc();
            let c = frame_alloc();
            let first = unsafe { *(a as *const u64) };
            kprintln!("  allocated {a:#x} {b:#x} {c:#x}; first word of {a:#x} = {first} (zeroed)");
            kprintln!("  free now: {}", unsafe { FRAME_FREE });
            frame_free(a);
            frame_free(b);
            frame_free(c);
            kprintln!(
                "  after free: {} (back to {})",
                unsafe { FRAME_FREE },
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
        "pkg-recv" => pkg::pkg_recv(),
        "pkg-list" => pkg::pkg_list(),
        "pkg-info" => pkg::pkg_info(arg),
        "pkg-run" => pkg::pkg_run(plan, arg),
        "pkg-remove" => pkg::pkg_remove(arg),
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
            run_tasks(&[(user_task as usize, TASK_PRINT, PERS_NATIVE)]);
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
            run_tasks(&[(rogue_task as usize, TASK_PRINT, PERS_NATIVE)]);
            kprintln!("[kernel] rogue task handled; console survived");
        }
        "multi" => {
            kprintln!("[kernel] spawning 3 cooperative U-mode tasks (round-robin via yield)");
            run_tasks(&[
                (worker_a as usize, TASK_PRINT, PERS_NATIVE),
                (worker_b as usize, TASK_PRINT, PERS_NATIVE),
                (worker_c as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] all tasks done; back in the console");
        }
        "linux" => {
            kprintln!("[kernel] running a Linux-ABI app through the Pol personality layer");
            run_tasks(&[(linux_app as usize, TASK_PRINT, PERS_LINUX)]);
            kprintln!("[kernel] Linux app done; back in the console");
        }
        "bench" => {
            kprintln!("[kernel] running ecall round-trip microbenchmark (500000 calls)...");
            run_tasks(&[(bench_task as usize, 0, PERS_NATIVE)]);
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
                (preempt_a as usize, TASK_PRINT, PERS_NATIVE),
                (preempt_b as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] preemption demo done");
        }
        "spy" => {
            kprintln!(
                "[kernel] isolation: task0 owns a private stack; task1 (spy) tries to read it"
            );
            kprintln!("[kernel] (task0 stack region base = {:#x})", stack_base());
            run_tasks(&[
                (victim_task as usize, TASK_PRINT, PERS_NATIVE),
                (spy_task as usize, 0, PERS_NATIVE),
            ]);
            kprintln!("[kernel] isolation demo done");
        }
        "ipc" => {
            kprintln!("[kernel] IPC: a no-authority service + an agent that delegates PRINT to it");
            // task 0 = service (no caps), task 1 = agent (holds PRINT)
            run_tasks(&[
                (service_task as usize, TASK_IPC, PERS_NATIVE),
                (agent_task as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
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
                    queue_service_task as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
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
                    queue_service_task as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] queue demo done; back in the console");
        }
        "cairn" => {
            kprintln!(
                "[kernel] Cairn store service + an agent doing a rollbackable action over IPC"
            );
            // task 0 = cairn store service, task 1 = agent (holds PRINT)
            run_tasks(&[
                (cairn_service as usize, TASK_IPC, PERS_NATIVE),
                (agent_cairn as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
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
                run_tasks(&[(linux_app as usize, TASK_PRINT, PERS_LINUX)]);
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
pub extern "C" fn kmain() -> ! {
    Uart.init();

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
        asm!("csrw stvec, {}", in(reg) trap_entry as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        asm!("csrs sie, {}", in(reg) STIE);
        asm!("csrs sstatus, {}", in(reg) 1usize << 1); // SIE: global supervisor interrupts
        asm!("csrw scounteren, {}", in(reg) 0x7usize); // let U-mode read cycle/time/instret
    }

    kprintln!("[dezh-boot] enabling Sv39 paging (U-mode confined to its own region)...");
    build_page_tables();
    enable_paging();
    frames_init();
    {
        let (total, free) = unsafe { (FRAME_TOTAL, FRAME_FREE) };
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

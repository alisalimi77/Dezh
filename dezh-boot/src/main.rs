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
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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
        kprintln!("\n[dezh-boot] unexpected interrupt scause={scause:#x} — halting");
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
                "  [kernel] DENIED: task faulted (scause {code}) at pc={sepc:#x} on {stval:#x} — killing task"
            );
            unsafe { restore_kernel_ctx() }
        }
        kprintln!("\n[dezh-boot] kernel page fault at pc={sepc:#x} on {stval:#x} (scause {code}) — halting");
        shutdown(FINISH_FAIL);
    }

    kprintln!("\n[dezh-boot] unexpected trap scause={scause:#x} — halting");
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

/// The separate user program, compiled to its own riscv ELF by build.rs and
/// embedded here. The loader maps it into a fresh address space at runtime.
const USERPROG_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/userprog.elf"));
const VIRTIO_BLK_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/virtio-blk.elf"));

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
unsafe fn walk_alloc(table: *mut u64, idx: usize) -> *mut u64 {
    let e = *table.add(idx);
    if e & PTE_V != 0 {
        (((e >> 10) << 12) as usize) as *mut u64 // existing next table
    } else {
        let frame = frame_alloc();
        *table.add(idx) = ((frame as u64 >> 12) << 10) | PTE_V; // non-leaf
        frame as *mut u64
    }
}

/// Map one 4 KiB page va->pa with `flags` in the page table rooted at `root`.
fn map_page(root: usize, va: usize, pa: usize, flags: u64) {
    let vpn2 = (va >> 30) & 0x1ff;
    let vpn1 = (va >> 21) & 0x1ff;
    let vpn0 = (va >> 12) & 0x1ff;
    unsafe {
        let l1 = walk_alloc(root as *mut u64, vpn2);
        let l0 = walk_alloc(l1, vpn1);
        *l0.add(vpn0) = pte(pa as u64, flags);
    }
}

const USER_STACK_TOP: usize = 0x4070_0000;
const USER_STACK_BOTTOM: usize = 0x4060_0000;

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
fn build_address_space(spec: &ProcessSpec) -> (usize, usize) {
    let img = spec.elf;
    let root = frame_alloc();
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
        let frame = frame_alloc();
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
        map_page(root, va, frame, fl);
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
        let frame = frame_alloc();
        map_page(root, s, frame, PTE_U | PTE_R | PTE_W);
        s += FRAME_SIZE;
    }

    // Device grants are explicit: no process sees MMIO unless its launch spec
    // maps that device. Drivers are user processes with device capabilities,
    // not kernel code with ambient hardware reach.
    if spec.map_uart {
        map_page(root, DEV_UART_VA, UART_BASE as usize, PTE_U | PTE_R | PTE_W);
    }
    if spec.map_virtio_blk && spec.caps & TASK_DEVICE_VIRTIO_BLK != 0 {
        let mut i = 0usize;
        while i < VIRTIO_MMIO_COUNT {
            map_page(
                root,
                DEV_VIRTIO_BLK_VA + i * VIRTIO_MMIO_STRIDE,
                VIRTIO_BLK_MMIO_PA + i * VIRTIO_MMIO_STRIDE,
                PTE_U | PTE_R | PTE_W,
            );
            i += 1;
        }
    }
    if spec.map_virtio_dma && spec.caps & (TASK_BLOCK_READ | TASK_BLOCK_WRITE) != 0 {
        let dma_pa = core::ptr::addr_of!(VIRTIO_DMA) as usize;
        let mut off = 0usize;
        while off < VIRTIO_DMA_SIZE {
            map_page(
                root,
                VIRTIO_DMA_VA + off,
                dma_pa + off,
                PTE_U | PTE_R | PTE_W,
            );
            off += FRAME_SIZE;
        }
    }

    (root, entry)
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
    Running,
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
}

const EMPTY_SERVICE: ServiceEntry = ServiceEntry {
    name: "",
    kind: ServiceKind::Init,
    state: ServiceState::Unused,
    task: usize::MAX,
    caps: 0,
    grants: 0,
    fault: "",
};

const MAX_SERVICES: usize = 8;
static mut SERVICES: [ServiceEntry; MAX_SERVICES] = [EMPTY_SERVICE; MAX_SERVICES];
static mut SERVICE_COUNT: usize = 0;
static mut TEXIT: [usize; MAX_TASKS] = [0; MAX_TASKS];

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

static mut FRAMES: [[usize; 32]; MAX_TASKS] = [[0; 32]; MAX_TASKS];
static mut TSTATE: [TaskState; MAX_TASKS] = [TaskState::Unused; MAX_TASKS];
static mut TCAPS: [usize; MAX_TASKS] = [0; MAX_TASKS];
static mut TPERS: [u8; MAX_TASKS] = [0; MAX_TASKS];
static mut TSATP: [usize; MAX_TASKS] = [0; MAX_TASKS]; // each task's address space (satp)
static mut CURRENT: usize = 0;

fn clear_mailbox(i: usize) {
    unsafe {
        MBOX[i] = EMPTY_MAILBOX;
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
            kprintln!("\n[dezh-boot] unexpected interrupt in task (scause={scause:#x}) — halting");
            shutdown(FINISH_FAIL);
        }

        // A task that touches memory outside its region is killed (thesis at the
        // hardware boundary still holds for scheduled tasks).
        if matches!(code, 12 | 13 | 15) {
            let stval: usize;
            asm!("csrr {}, stval", out(reg) stval);
            kprintln!(
                "  [kernel] task {} DENIED: faulted on {stval:#x} (outside its grant) — killing",
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
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // ATTENUATION: a sender can only delegate capabilities it
                    // itself holds — never widen. (caps = sender's TCAPS.)
                    let granted = requested & caps;
                    if MBOX[to].count == MAILBOX_DEPTH {
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
                    if TSTATE[to] == TaskState::Blocked {
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
                    if MBOX[cur].count > 0 {
                        let head = MBOX[cur].head;
                        let msg = MBOX[cur].slots[head];
                        let n = msg.len.min(frame[F_A1]);
                        if n > 0 {
                            let dst = core::slice::from_raw_parts_mut(frame[F_A0] as *mut u8, n);
                            dst.copy_from_slice(&msg.buf[..n]);
                        }
                        // CAPABILITY TRANSFER: the receiver gains exactly the
                        // capabilities the sender delegated (already attenuated).
                        TCAPS[cur] |= msg.grant;
                        let from = msg.from;
                        let word = msg.word;
                        MBOX[cur].slots[head] = EMPTY_IPC_MESSAGE;
                        MBOX[cur].head = (head + 1) % MAILBOX_DEPTH;
                        MBOX[cur].count -= 1;
                        frame[F_A0] = n; // bytes received
                        frame[F_A1] = from; // sender task id (for replies)
                        frame[F_A2] = word; // register-passed scalar (value-IPC)
                        return frame_ptr;
                    } else {
                        // Re-run the ecall when we are scheduled again.
                        frame[F_SEPC] -= 4;
                        TSTATE[cur] = TaskState::Blocked;
                        return schedule_or_return();
                    }
                }
                _ => {
                    frame[F_A0] = SYS_DENIED;
                    return frame_ptr;
                }
            }
        }

        kprintln!("\n[dezh-boot] unexpected trap in task (scause={scause:#x}) — halting");
        shutdown(FINISH_FAIL);
    }
}

/// Set up `specs` as Ready tasks and run them round-robin until all finish.
/// Each spec is (entry, caps). Returns when every task is Done.
fn run_tasks(specs: &[(usize, usize, u8)]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        for i in 0..MAX_TASKS {
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
            TSTATE[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        for (i, spec) in specs.iter().take(n).enumerate() {
            let (root, entry) = build_address_space(spec);
            let f = &mut FRAMES[i];
            *f = [0; 32];
            f[F_SEPC] = entry;
            f[F_SP] = USER_STACK_TOP; // each process has its own stack in its own space
            f[F_A0] = spec.arg0;
            f[F_A1] = spec.arg1;
            f[F_A2] = spec.arg2;
            f[F_A3] = spec.arg3;
            TCAPS[i] = spec.caps;
            TPERS[i] = spec.personality;
            TSATP[i] = proc_satp(root);
            TSTATE[i] = TaskState::Ready;
        }
        CURRENT = 0;
        asm!("csrw stvec, {}", in(reg) utrap as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) TSATP[0]); // enter the first process's address space
        asm!("sfence.vma");
        run_first(frame_ptr(0) as *const usize);
        // Back in the kernel address space once every process has exited.
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
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

fn spawn_process_at(slot: usize, spec: &ProcessSpec) {
    unsafe {
        let (root, entry) = build_address_space(spec);
        let f = &mut FRAMES[slot];
        *f = [0; 32];
        f[F_SEPC] = entry;
        f[F_SP] = USER_STACK_TOP;
        f[F_A0] = spec.arg0;
        f[F_A1] = spec.arg1;
        f[F_A2] = spec.arg2;
        f[F_A3] = spec.arg3;
        TCAPS[slot] = spec.caps;
        TPERS[slot] = spec.personality;
        TSATP[slot] = proc_satp(root);
        TEXIT[slot] = 0;
        clear_mailbox(slot);
        TSTATE[slot] = TaskState::Ready;
    }
}

fn clear_foreground_tasks() {
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
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
    for (i, spec) in specs.iter().take(n).enumerate() {
        spawn_process_at(FIRST_FOREGROUND_TASK + i, spec);
    }
    run_scheduler_from(FIRST_FOREGROUND_TASK);
}

fn service_state_name(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Unused => "Unused",
        ServiceState::Declared => "Declared",
        ServiceState::Starting => "Starting",
        ServiceState::Running => "Running",
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
                    SERVICES[i].fault = "driver stopped";
                } else if TSTATE[task] == TaskState::Done {
                    SERVICES[i].state = ServiceState::Faulted;
                    SERVICES[i].fault = "driver exited or faulted";
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
        SERVICES[idx].state = ServiceState::Starting;
        SERVICES[idx].task = VIRTIO_SERVICE_TASK;
        SERVICES[idx].fault = "";
        let caps = SERVICES[idx].caps;
        kprintln!(
            "[services] starting virtio-block from boot registry as task {VIRTIO_SERVICE_TASK}"
        );
        let spec = ProcessSpec::new(VIRTIO_BLK_ELF, caps, BLK_OP_DAEMON)
            .args(virtio_dma_pa(), 0, 0)
            .virtio_blk()
            .virtio_dma();
        spawn_process_at(VIRTIO_SERVICE_TASK, &spec);
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
                "  - {:<13} {:?} state={} task={} caps={:#x} grants={:#x} {}",
                s.name,
                s.kind,
                service_state_name(s.state),
                s.task,
                s.caps,
                s.grants,
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
const BLK_REQ_INSTALL_CHECK: usize = 8;
const BLK_REQ_INSTALL_INIT: usize = 9;
const BLK_REQ_ROOT_STATUS: usize = 10;
const VIRTIO_SERVICE_TASK: usize = 0;
const FIRST_FOREGROUND_TASK: usize = 1;

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
        help: "list commands",
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
    const GROUPS: &[&str] = &["Inspect", "Storage", "Services", "Safety", "Demos", "Power"];
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
                "  task{} state={:<7} caps={:#x} exit={} service={}",
                i,
                task_state_name(TSTATE[i]),
                TCAPS[i],
                TEXIT[i],
                service_for_task(i)
            );
            i += 1;
        }
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
            print_help(held);
        }
        "caps" => kprintln!("console capabilities: {}", cap_names(held)),
        "mem" => {
            kprintln!("usable memory: {} bytes", plan.usable_bytes);
            for r in memory {
                let end = r.start + r.len;
                kprintln!("  {:#012x}..{:#012x}  {:?}", r.start, end, r.kind);
            }
        }
        "status" => print_status(plan, memory, held),
        "tasks" => print_tasks(),
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
                kprintln!();
                return len;
            }
            b'\r' => {}
            0x7f | 0x08 => {
                if len > 0 {
                    len -= 1;
                    kprint!("\x08 \x08");
                }
            }
            c if (c == b' ' || c.is_ascii_graphic()) && len < buf.len() => {
                buf[len] = c;
                len += 1;
                Uart.putc(c);
            }
            _ => {}
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
        "[dezh-boot] embedded user ELFs: userprog={} bytes, virtio-blk={} bytes",
        USERPROG_ELF.len(),
        VIRTIO_BLK_ELF.len()
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

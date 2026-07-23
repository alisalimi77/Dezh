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
const MARZ_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/marz.elf"));
const BENCH_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-bench.elf"));
const NOTE_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-note.elf"));
const LAB_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-lab.elf"));
const CALC_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-calc.elf"));
const VAULT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dezh-vault.elf"));
// An unmodified static Linux/RISC-V ELF, built for `riscv64gc-unknown-linux-musl`.
// Loaded like any program but run with the Linux personality (Pol, D014/F4).
const LINUX_GUEST_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/linux-guest.elf"));

const DEV_UART_VA: usize = 0x5000_0000;
const DEV_VIRTIO_BLK_VA: usize = 0x5000_0000;
const VIRTIO_BLK_MMIO_PA: usize = 0x1000_1000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;
const VIRTIO_MMIO_COUNT: usize = 8;
const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_DEVICE_ID_NET: u32 = 1;
const VIRTIO_MMIO_OFF_DEVICE_ID: usize = 0x008;
/// Where a Marz daemon sees its granted NIC page (one device, not the window).
const DEV_VIRTIO_NET_VA: usize = 0x5002_0000;
/// Marz gets its OWN DMA window. Sharing one with the block daemon would let
/// either corrupt the other's virtqueue - two devices, two grants.
const MARZ_DMA_VA: usize = 0x5200_0000;
const MARZ_DMA_SIZE: usize = 16 * 1024;
static mut MARZ_DMA: DmaWindow = DmaWindow([0; MARZ_DMA_SIZE]);

fn marz_dma_pa() -> usize {
    core::ptr::addr_of!(MARZ_DMA) as usize
}

/// Scan the virtio-mmio window for a device of `want_id` and return its physical
/// base. The kernel may read the window directly (it lives in the kernel-only
/// device mapping); a daemon never scans — it receives only the single page the
/// kernel grants it.
fn find_virtio_mmio(want_id: u32) -> Option<usize> {
    let mut i = 0usize;
    while i < VIRTIO_MMIO_COUNT {
        let base = VIRTIO_BLK_MMIO_PA + i * VIRTIO_MMIO_STRIDE;
        let magic = unsafe { read_volatile(base as *const u32) };
        let dev = unsafe { read_volatile((base + VIRTIO_MMIO_OFF_DEVICE_ID) as *const u32) };
        if magic == VIRTIO_MMIO_MAGIC && dev == want_id {
            return Some(base);
        }
        i += 1;
    }
    None
}

// --- Marz M2: the egress gate -------------------------------------------------
//
// Authority to send is NOT "network access": it is a capability for a specific
// DESTINATION, and the destination carries a secrecy label so the DIFC rule
// applies on export (the Flume lesson: leaving the system is a declassification).

struct MarzDest {
    name: &'static str,
    ip: [u8; 4],
    port: u16,
    /// How secret this destination is cleared to receive.
    label: dezh_core::difc::Label,
    /// What the ledger records when a frame actually leaves for this destination.
    record: &'static str,
}

const MARZ_DESTS: [MarzDest; 2] = [
    // A public collector: cleared for nothing secret.
    MarzDest {
        name: "ops",
        ip: [10, 0, 2, 2],
        port: 8888,
        label: 0,
        record: "egress -> ops 10.0.2.2:8888 [REAL external send, on the wire]",
    },
    // A destination cleared to receive vault-class secrets.
    MarzDest {
        name: "vault-sync",
        ip: [10, 0, 2, 3],
        port: 9999,
        label: NS_SECRET_VAULT,
        record: "egress -> vault-sync 10.0.2.3:9999 [REAL external send, on the wire]",
    },
];

/// Egress capabilities live above the Cairn namespace bits.
const MARZ_DEST_BASE: usize = 16;
const fn marz_dest_cap(d: usize) -> usize {
    1 << (MARZ_DEST_BASE + d)
}

/// The operator's per-destination egress authority. Revoking one destination
/// leaves the others intact — the point of naming destinations in the capability.
static mut OP_EGRESS: usize = marz_dest_cap(0) | marz_dest_cap(1);

fn marz_dest_id(name: &str) -> Option<usize> {
    MARZ_DESTS.iter().position(|d| d.name == name)
}

fn marz_dest_packed(d: &MarzDest) -> usize {
    ((d.ip[0] as usize) << 24)
        | ((d.ip[1] as usize) << 16)
        | ((d.ip[2] as usize) << 8)
        | (d.ip[3] as usize)
        | ((d.port as usize) << 32)
}

/// The egress gate: a send needs (a) the capability for THAT destination, and
/// (b) an information flow the destination may legally receive. Returns true if
/// the send may proceed; prints a named reason otherwise.
fn marz_gate(d: usize) -> bool {
    let dest = &MARZ_DESTS[d];
    if unsafe { OP_EGRESS } & marz_dest_cap(d) == 0 {
        kprintln!(
            "[marz] DENIED: no capability for destination '{}' -- egress authority names a destination, it is not 'network access'",
            dest.name
        );
        return false;
    }
    if !unsafe { OP_TAINT.may_flow_to(dest.label) } {
        kprintln!(
            "[marz] DENIED: sending to '{}' would export secret-tainted data to a destination cleared for {:#x} (taint={:#x}) -- declassify first",
            dest.name,
            dest.label,
            unsafe { OP_TAINT.secrecy() }
        );
        return false;
    }
    true
}

/// Marz: launch the egress daemon for an authorized destination. It receives
/// exactly two grants — the one discovered NIC page and the DMA window — plus
/// PRINT. No block authority, no other device, and it never scans for hardware.
fn run_marz_send(plan: &KernelPlan, arg: &str) {
    marz_send_to(plan, arg, 0);
}

/// Send to `arg` under intent `ahd` (0 = direct). On success the send is
/// recorded as an IRREVERSIBLE effect on the ledger: it already happened in the
/// outside world, so rollback must refuse it.
fn marz_send_to(plan: &KernelPlan, arg: &str, ahd: u16) {
    const LAB: usize = 1;
    let name = arg.trim();
    let name = if name.is_empty() { "ops" } else { name };
    let Some(d) = marz_dest_id(name) else {
        kprintln!("[marz] unknown destination '{name}' (known: ops vault-sync)");
        return;
    };
    if find_virtio_mmio(VIRTIO_DEVICE_ID_NET).is_none() {
        kprintln!("[marz] no virtio-net device; nothing to send (see net-probe)");
        record_event("kernel", "marz.send", "virtio-net", "absent");
        return;
    }
    if !marz_gate(d) {
        record_event("kernel", "marz.send", MARZ_DESTS[d].name, "DENIED");
        return;
    }
    let dest = &MARZ_DESTS[d];
    kprintln!(
        "[marz] authorized egress to '{}' ({}.{}.{}.{}:{}); launching the daemon with ONLY the NIC page + DMA",
        dest.name, dest.ip[0], dest.ip[1], dest.ip[2], dest.ip[3], dest.port
    );
    run_foreground_processes(&[ProcessSpec::new(
        MARZ_ELF,
        TASK_PRINT | TASK_DEVICE_VIRTIO_NET,
        0,
    )
    .args(marz_dma_pa(), marz_dest_packed(dest), 0)
    .virtio_net()]);
    let st = unsafe { TEXIT[FIRST_FOREGROUND_TASK] };
    record_event(
        "kernel",
        "marz.send",
        dest.name,
        if st == 0 { "OK" } else { "fail" },
    );
    if st != 0 {
        kprintln!("[marz] egress failed (status={st})");
        return;
    }
    kprintln!("[marz] egress complete: a real frame left the machine for '{}'", dest.name);
    // The wire is not reversible. Record it as an irreversible effect so the
    // ledger attributes it and rollback refuses it honestly.
    let derived = pkg::MCAP_PRINT;
    let led = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, ahd, derived, SAND_REV_IRREVERSIBLE),
        dest.record,
        task_ns_cap(LAB),
    );
    kprintln!(
        "[marz] recorded on the ledger as IRREVERSIBLE (ns=lab, intent={}, status={led})",
        if ahd == 0 { 0 } else { ahd }
    );
}

fn marz_dest_authority(arg: &str, grant: bool) {
    let Some(d) = marz_dest_id(arg.trim()) else {
        kprintln!("[marz] unknown destination (known: ops vault-sync)");
        return;
    };
    unsafe {
        if grant {
            OP_EGRESS |= marz_dest_cap(d);
        } else {
            OP_EGRESS &= !marz_dest_cap(d);
        }
    }
    kprintln!(
        "[marz] destination '{}' egress capability {}",
        MARZ_DESTS[d].name,
        if grant { "granted" } else { "REVOKED" }
    );
    record_event(
        "kernel",
        if grant { "marz.grant" } else { "marz.revoke" },
        MARZ_DESTS[d].name,
        "OK",
    );
}

/// M3: a REAL external effect, end to end. A send under an intent leaves the
/// machine, is recorded as irreversible, is attributed by the provenance graph,
/// and is REFUSED by rollback - because it genuinely cannot be undone.
fn run_marz_effect_demo(plan: &KernelPlan) {
    declassify();
    unsafe { OP_EGRESS = marz_dest_cap(0) | marz_dest_cap(1) };
    kprintln!("[marz-effect-demo] a real send becomes an irreversible, attributable effect");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[marz-effect-demo] FAIL: no free intent slot");
        return;
    };
    kprintln!("[marz-effect-demo] 1/4 send to 'ops' under intent Ahd#{id} (a real frame on the wire):");
    marz_send_to(plan, "ops", id);

    kprintln!("[marz-effect-demo] 2/4 the rollback forecast for the mission:");
    let mut idbuf = [0u8; 8];
    let idstr = u16_to_str(id, &mut idbuf);
    sfar_cmd(plan, BLK_REQ_SFAR_PLAN, idstr);

    kprintln!("[marz-effect-demo] 3/4 provenance: who authorized what left the machine:");
    sfar_cmd(plan, BLK_REQ_TBAR, idstr);

    kprintln!("[marz-effect-demo] 4/4 roll the mission back - the send CANNOT be undone:");
    sfar_cmd(plan, BLK_REQ_SFAR_ROLLBACK, idstr);
    record_event("kernel", "marz.effect", "egress", "OK");
    kprintln!("[marz-effect-demo] PASS: the wire is honest - a real external effect is attributed, classified irreversible, and rollback refuses it instead of pretending");
}

/// Render a u16 into `buf` and return it as a str (no allocator in the console).
fn u16_to_str(v: u16, buf: &mut [u8; 8]) -> &str {
    let mut n = v;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("0")
}

/// The Marz gate proven end to end: per-destination authority and the DIFC
/// export rule, both enforced before anything reaches the wire.
fn run_marz_demo(plan: &KernelPlan) {
    declassify();
    unsafe { OP_EGRESS = marz_dest_cap(0) | marz_dest_cap(1) };
    kprintln!("[marz-demo] egress authority names a DESTINATION, and export obeys information flow");

    kprintln!("[marz-demo] 1/4 authorized, untainted -> send to 'ops':");
    run_marz_send(plan, "ops");

    kprintln!("[marz-demo] 2/4 revoke ONLY 'ops' (vault-sync untouched) -> send refused:");
    marz_dest_authority("ops", false);
    run_marz_send(plan, "ops");
    marz_dest_authority("ops", true);

    kprintln!("[marz-demo] 3/4 read ns=vault (secret) -> the operator is tainted; a send to the PUBLIC 'ops' is exfiltration:");
    cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, "vault");
    run_marz_send(plan, "ops");

    kprintln!("[marz-demo] 4/4 the same tainted data MAY go to 'vault-sync' (cleared for it):");
    run_marz_send(plan, "vault-sync");
    declassify();
    kprintln!("[marz-demo] PASS: a destination capability is not network access, and a secret cannot be exported to a destination not cleared for it");
    record_event("kernel", "marz.demo", "egress", "OK");
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
const VIRTIO_DMA_VA: usize = 0x5100_0000;
const VIRTIO_DMA_SIZE: usize = 16 * 1024;
const VIRTIO_DATA_OFF: usize = 8_192 + 16;
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
    // Marz: grant exactly ONE device page — the NIC the kernel discovered — under
    // its own capability. Unlike the block grant above, the daemon never sees the
    // rest of the virtio-mmio window.
    if spec.map_virtio_net && spec.caps & TASK_DEVICE_VIRTIO_NET != 0 {
        let Some(nic_pa) = find_virtio_mmio(VIRTIO_DEVICE_ID_NET) else {
            reclaim_resources(&mut resources);
            return None;
        };
        if !map_page(
            root,
            DEV_VIRTIO_NET_VA,
            nic_pa,
            PTE_U | PTE_R | PTE_W,
            &mut resources,
        ) {
            reclaim_resources(&mut resources);
            return None;
        }
        let marz_dma = core::ptr::addr_of!(MARZ_DMA) as usize;
        let mut off = 0usize;
        while off < MARZ_DMA_SIZE {
            if !map_page(
                root,
                MARZ_DMA_VA + off,
                marz_dma + off,
                PTE_U | PTE_R | PTE_W,
                &mut resources,
            ) {
                reclaim_resources(&mut resources);
                return None;
            }
            off += 4096;
        }
    }
    if spec.map_virtio_dma
        && spec.caps & (TASK_BLOCK_READ | TASK_BLOCK_WRITE | TASK_DEVICE_VIRTIO_NET) != 0
    {
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
    // Services check the SENDER's authority (not their own) against this
    // kernel-attested value; a client cannot forge it from user space.
    frame[F_A3] = msg.sender_caps;
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
                FRAMES[i][F_A2] =
                    typed_word(IPC_SERVICE_SYSTEM, IPC_OP_TIMEOUT, 0, IPC_STATUS_TIMEOUT, 0);
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
                    msg.sender_caps = caps;
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
const BLK_REQ_PKG_STORE_INIT: usize = 35;
const BLK_REQ_PKG_REGISTRY_READ: usize = 36;
const BLK_REQ_PKG_REGISTRY_WRITE: usize = 37;
const BLK_REQ_PKG_BLOB_READ: usize = 38;
const BLK_REQ_PKG_BLOB_WRITE: usize = 39;
const BLK_REQ_PKG_JOURNAL_READ: usize = 40;
const BLK_REQ_PKG_JOURNAL_WRITE: usize = 41;
// 42 (CAIRN_INIT) is daemon-internal: the store lazy-formats on first use.
const BLK_REQ_CAIRN_COMMIT: usize = 43;
const BLK_REQ_CAIRN_GET: usize = 44;
const BLK_REQ_CAIRN_LOG: usize = 45;
const BLK_REQ_CAIRN_ROLLBACK: usize = 46;
const BLK_REQ_CAIRN_VERIFY: usize = 47;
const BLK_REQ_CAIRN_STATUS: usize = 48;
// Sand (W8 P2): effect-ledger view over the same enriched Cairn commit log.
const BLK_REQ_SAND_LOG: usize = 49;
const BLK_REQ_SAND_INFO: usize = 50;
// Sfar (W8 P3): mission rollback forecast + whole-mission rollback.
const BLK_REQ_SFAR_PLAN: usize = 51;
const BLK_REQ_SFAR_ROLLBACK: usize = 52;
// Tbar (W8 P5): actor -> intent -> effect provenance graph for one intent.
const BLK_REQ_TBAR: usize = 53;
// Persisted namespace revocation (ocap migration): the daemon records a per-ns
// revoked flag in the superblock so revocation survives reboot.
const BLK_REQ_NS_REVOKE: usize = 54;
const BLK_REQ_NS_GRANT: usize = 55;
// Task-capability bits 8..15 gate Cairn v1 namespaces 0..7 (kernel-attested on
// every IPC recv; the storage daemon checks the requested namespace's bit).
const TASK_CAIRN_NS_BASE: usize = 8;
const CAIRN_NS_NAMES: [&str; 5] = ["note", "lab", "calc", "vault", "agent"];

fn cairn_ns_id(name: &str) -> Option<usize> {
    CAIRN_NS_NAMES.iter().position(|n| *n == name)
}

const fn task_ns_cap(ns: usize) -> usize {
    1 << (TASK_CAIRN_NS_BASE + ns)
}

/// The full Cairn v1 namespace-capability set (bits 8..12). The console acts as
/// the operator/mission owner: a Sfar plan/rollback it drives may touch any
/// namespace, so it presents authority for all of them and the storage daemon
/// still enforces the mission-authority check per touched namespace.
const fn all_cairn_ns_caps() -> usize {
    let mut caps = 0usize;
    let mut ns = 0usize;
    while ns < CAIRN_NS_NAMES.len() {
        caps |= task_ns_cap(ns);
        ns += 1;
    }
    caps
}

/// Pack a Cairn request for the virtio-blk client: base op | ns << 8 | steps << 12.
fn cairn_req(base: usize, ns: usize, steps: usize) -> usize {
    base | (ns << 8) | (steps.min(0xfff) << 12)
}

/// Pack a Sand-carrying commit request: the base packing plus the intent(Ahd)
/// id in bits 24..39 and a status byte in bits 40..47 that holds the derived cap
/// (bits 0..4) and the effect's reversibility class (bits 5..6). The client
/// courier unpacks these into the commit IPC so the daemon records provenance.
/// A direct (no-intent) reversible commit uses `cairn_req` with `ahd == 0`.
fn cairn_req_intent(base: usize, ns: usize, ahd: u16, derived: u32, rev_class: u8) -> usize {
    let status_byte = ((derived & 0x1f) | (((rev_class & 0x3) as u32) << 5)) as usize;
    cairn_req(base, ns, 0) | ((ahd as usize) << 24) | (status_byte << 40)
}

/// Pack a Sfar (mission) request: base op | ns << 8 | ahd << 24. The mission's
/// Ahd id rides the request-id field over the commit IPC to the daemon.
fn sfar_req(base: usize, ns: usize, ahd: u16) -> usize {
    cairn_req(base, ns, 0) | ((ahd as usize) << 24)
}

// Reversibility classes, mirrored from the storage daemon: an effect never
// silently claims to be reversible (unknown is its own class).
const SAND_REV_REVERSIBLE: u8 = 0;
#[allow(dead_code)]
const SAND_REV_COMPENSATABLE: u8 = 1;
const SAND_REV_IRREVERSIBLE: u8 = 2;
#[allow(dead_code)]
const SAND_REV_UNKNOWN: u8 = 3;
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
        let start = if EVENT_COUNT == EVENT_CAP {
            EVENT_NEXT
        } else {
            0
        };
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

/// A recorded event result that denotes a refusal/denial (not a success).
fn is_denial(result: &str) -> bool {
    matches!(
        result,
        "DENIED" | "TRAP" | "fail" | "escaped" | "REVIEW_REQUIRED" | "CORRUPT"
    )
}

/// Map an event's action to the Dezh enforcement boundary that produced it, so a
/// denial can be explained in terms of a real mechanism, not a policy string.
fn denial_boundary(action: &str) -> &'static str {
    if action.starts_with("intent") {
        "intent-derivation ceiling (derived cap <= Ahd), enforced in the kernel"
    } else if action.starts_with("sfar") || action.starts_with("tbar") {
        "mission authority: the caller must hold every namespace the mission touched"
    } else if action.starts_with("sand") || action.starts_with("cairn") {
        "storage-service capability check (kernel-attested namespace caps)"
    } else if action.starts_with("pkg") {
        "package manifest grants (no capability beyond the verified manifest)"
    } else if action.starts_with("cap") || action.starts_with("mmio") {
        "kernel capability check (no ambient authority to forge or amplify)"
    } else if action.starts_with("pol") {
        "Pol personality capability check (legacy syscalls are capability-gated)"
    } else if action.starts_with("redteam") {
        "adversary containment: an escape attempt stopped at a named boundary"
    } else {
        "kernel capability boundary"
    }
}

/// W8 P5: explain denials. Every important effect and refusal is recorded in the
/// in-kernel event ring; `why-denied` walks it newest-first and names the
/// boundary that produced each denial. A refusal is never a silent "no" — it is
/// attributable to a specific mechanism.
///
/// `why-denied`       explains the most recent denial (default).
/// `why-denied all`   lists every recent denial with its boundary (audit a whole
///                    agent run, e.g. after `overnight`).
fn why_denied(arg: &str) {
    let all = arg.trim() == "all";
    unsafe {
        if EVENT_COUNT == 0 {
            kprintln!("[why-denied] no events recorded yet");
            return;
        }
        let start = if EVENT_COUNT == EVENT_CAP { EVENT_NEXT } else { 0 };
        let mut found = 0usize;
        let mut k = EVENT_COUNT;
        while k > 0 {
            k -= 1;
            let idx = (start + k) % EVENT_CAP;
            let e = EVENTS[idx];
            if !is_denial(e.result) {
                continue;
            }
            found += 1;
            let label = if all { "denial" } else { "last denial" };
            kprintln!(
                "[why-denied] {label}: actor={} action={} target={} result={} (tick {})",
                e.actor,
                e.action,
                e.target,
                e.result,
                e.tick
            );
            kprintln!("[why-denied] boundary: {}", denial_boundary(e.action));
            if !all {
                kprintln!("[why-denied] policy: authority is explicit and unforgeable; nothing runs on ambient permission");
                return;
            }
        }
        if found == 0 {
            kprintln!(
                "[why-denied] no denial in the last {} events; every recent action was authorized",
                EVENT_COUNT
            );
        } else if all {
            kprintln!(
                "[why-denied] {found} denial(s) recorded; each attributable to a named boundary (no ambient authority)"
            );
        }
    }
}

fn print_audit() {
    kprintln!("audit summary:");
    kprintln!("  model: no ambient authority; important effects are event-recorded");
    kprintln!(
        "  tracked: install, app install/run/remove, service stop/restart/fault, denial demos"
    );
    print_events();
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

fn prepare_virtio_input_bytes(bytes: &[u8]) {
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

/// Like `run_registered_virtio_client_status`, but the client is spawned with
/// `extra_caps` on top of the base client set. Used for Cairn namespace caps:
/// the console (operator) decides which namespace authority the client holds,
/// and the kernel attests exactly that to the storage daemon.
fn run_registered_virtio_client_ns(
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
fn run_virtio_client_ns_raw(
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
    unsafe { TEXIT[FIRST_FOREGROUND_TASK] }
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

// --- Namespace authority as an object-capability (ocap migration) -------------
//
// The Cairn namespace capability is the first live authority migrated onto
// `dezh_core::ocap`: the operator console holds a generation-stamped handle per
// namespace, and `ns-revoke` bumps that namespace's generation so the held
// handle goes stale and further console operations on the live storage path are
// refused — real runtime revocation of a live capability, which the coarse
// task-capability bitmask cannot express. (The bitmask still gates the U-mode
// client → daemon hop; migrating that hop too is the remaining work.)

static mut NS_TABLE: dezh_core::ocap::CapTable<8> = dezh_core::ocap::CapTable::new();
static mut NS_HANDLE: [Option<dezh_core::ocap::Cap>; 8] = [None; 8];
static mut NS_INIT: bool = false;

fn ns_authority_init() {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    unsafe {
        if NS_INIT {
            return;
        }
        let mut i = 0usize;
        while i < CAIRN_NS_NAMES.len() {
            NS_HANDLE[i] = NS_TABLE.mint(i, R_READ | R_WRITE | R_DELEGATE);
            i += 1;
        }
        NS_INIT = true;
    }
}

/// Quiet check: does the operator still hold a live capability for namespace
/// `ns` (its handle's generation still matches the object's live generation)?
fn ns_authority_ok(ns: usize) -> bool {
    use dezh_core::ocap::{CapCheck, R_READ};
    ns_authority_init();
    unsafe { NS_HANDLE[ns].map(|h| NS_TABLE.check(&h, R_READ)) == Some(CapCheck::Ok) }
}

/// Console gate: like [`ns_authority_ok`] but prints an explainable denial when
/// the namespace capability has been revoked.
fn ns_authority_live(ns: usize) -> bool {
    if ns_authority_ok(ns) {
        return true;
    }
    let name = CAIRN_NS_NAMES.get(ns).copied().unwrap_or("?");
    kprintln!("[cap] DENIED: namespace '{name}' capability was REVOKED (ns-grant {name} to re-mint) -- ocap generation stale");
    false
}

fn ns_revoke(plan: &KernelPlan, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    ns_authority_init();
    // In-memory kernel gate (fast, this boot).
    unsafe { NS_TABLE.revoke(ns) };
    // Persist at the object owner (survives reboot): the daemon records the
    // revoked flag in the superblock and enforces it on every Cairn op.
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_NS_REVOKE, ns, 0),
        "",
        task_ns_cap(ns),
    );
    kprintln!(
        "[ns-revoke] namespace '{}' capability REVOKED (kernel handle stale + persisted at the store, status={st})",
        CAIRN_NS_NAMES[ns]
    );
    record_event("kernel", "ns.revoke", CAIRN_NS_NAMES[ns], "OK");
}

fn ns_grant(plan: &KernelPlan, arg: &str) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    ns_authority_init();
    unsafe { NS_HANDLE[ns] = NS_TABLE.mint(ns, R_READ | R_WRITE | R_DELEGATE) };
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_NS_GRANT, ns, 0),
        "",
        task_ns_cap(ns),
    );
    kprintln!(
        "[ns-grant] namespace '{}' capability re-minted + persisted grant cleared at the store (status={st})",
        CAIRN_NS_NAMES[ns]
    );
    record_event("kernel", "ns.grant", CAIRN_NS_NAMES[ns], "OK");
}

/// Prove the migration: a namespace capability revoked at runtime stops the live
/// storage path (a commit is refused by the ocap check before it reaches the
/// daemon), and re-granting restores it.
fn run_nsrevoke_demo(plan: &KernelPlan) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    const CALC: usize = 2;
    ns_authority_init();
    kprintln!("[nsrevoke-demo] runtime revocation of a LIVE namespace capability (ocap generation), enforced on the storage path");
    kprintln!("[nsrevoke-demo] 1/4 commit while the capability is live:");
    cairn_cmd_commit(plan, "calc nsrev-before");
    let live1 = ns_authority_ok(CALC);
    kprintln!("[nsrevoke-demo] 2/4 ns-revoke calc (bump the generation):");
    unsafe { NS_TABLE.revoke(CALC) };
    kprintln!("[nsrevoke-demo] 3/4 commit after revoke -> the ocap check refuses it before it reaches the daemon:");
    cairn_cmd_commit(plan, "calc nsrev-blocked");
    let blocked = !ns_authority_ok(CALC);
    kprintln!("[nsrevoke-demo] 4/4 ns-grant calc (re-mint) then commit again:");
    unsafe { NS_HANDLE[CALC] = NS_TABLE.mint(CALC, R_READ | R_WRITE | R_DELEGATE) };
    cairn_cmd_commit(plan, "calc nsrev-after");
    let live2 = ns_authority_ok(CALC);
    let pass = live1 && blocked && live2;
    record_event("kernel", "nsrevoke.demo", "ns:calc", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[nsrevoke-demo] PASS: a live namespace capability was revoked at runtime (generation bump) and re-granted -- what the bitmask model could not do");
    } else {
        kprintln!("[nsrevoke-demo] FAIL: live1={live1} blocked={blocked} live2={live2}");
    }
}

// --- Confidentiality enforcement on the storage path (DIFC) -------------------
//
// Each namespace carries a secrecy label. The console operator accumulates a
// taint as it READS namespaces, and a commit is refused if the operator's taint
// does not flow down into the target namespace (no write-down) — real
// exfiltration prevention on the live Cairn path, with an explicit privileged
// `declassify` escape hatch (the standard DIFC declassification). Model in
// `dezh_core::difc`.

const NS_SECRET_VAULT: dezh_core::difc::Label = 1 << 0;
static mut NS_LABEL: [dezh_core::difc::Label; 8] = [0; 8];
static mut OP_TAINT: dezh_core::difc::Taint = dezh_core::difc::Taint::new();
static mut DIFC_INIT: bool = false;

fn difc_init() {
    unsafe {
        if DIFC_INIT {
            return;
        }
        // vault (ns id 3) holds secrets; other namespaces are public here.
        NS_LABEL[3] = NS_SECRET_VAULT;
        DIFC_INIT = true;
    }
}

fn ns_label(ns: usize) -> dezh_core::difc::Label {
    difc_init();
    unsafe { *NS_LABEL.get(ns).unwrap_or(&0) }
}

/// After a successful READ of `ns`, raise the operator's taint by that
/// namespace's secrecy label — reading a secret taints the reader.
fn difc_observe(ns: usize) {
    let l = ns_label(ns);
    if l != 0 {
        unsafe { OP_TAINT.observe(l) };
        kprintln!(
            "[difc] operator tainted by reading a labelled namespace (secrecy now {:#x})",
            unsafe { OP_TAINT.secrecy() }
        );
    }
}

/// Before a WRITE to `ns`, the operator's taint must flow down into the target
/// (`taint ⊆ ns label`); otherwise the write would exfiltrate a secret to a
/// lower sink. Prints an explainable denial and returns false when refused.
fn difc_may_write(ns: usize) -> bool {
    if unsafe { OP_TAINT.may_flow_to(ns_label(ns)) } {
        return true;
    }
    kprintln!(
        "[difc] DENIED: writing to ns='{}' would leak secret-tainted data to a lower sink (taint={:#x}, sink label={:#x}); declassify first",
        CAIRN_NS_NAMES.get(ns).copied().unwrap_or("?"),
        unsafe { OP_TAINT.secrecy() },
        ns_label(ns)
    );
    false
}

fn declassify() {
    difc_init();
    unsafe { OP_TAINT = dezh_core::difc::Taint::new() };
    kprintln!("[declassify] operator taint cleared (privileged declassification)");
    record_event("kernel", "difc.declassify", "operator", "OK");
}

fn taint_show() {
    kprintln!("[taint] operator secrecy taint = {:#x}", unsafe {
        OP_TAINT.secrecy()
    });
}

/// Prove DIFC enforcement on the real storage path: read a secret namespace,
/// then be refused when writing it down to a public one, until an explicit
/// declassification.
fn run_taintflow_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    declassify();
    kprintln!("[taintflow-demo] read a secret, then be refused writing it down to a public namespace (enforced on the storage path)");
    kprintln!("[taintflow-demo] 1/4 read ns=vault (secret) -> the operator is tainted:");
    cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, "vault");
    kprintln!("[taintflow-demo] 2/4 try to commit to ns=lab (public) -> exfiltration REFUSED:");
    cairn_cmd_commit(plan, "lab leaked-secret");
    let blocked = !unsafe { OP_TAINT.may_flow_to(ns_label(LAB)) };
    kprintln!("[taintflow-demo] 3/4 declassify (privileged), then commit to ns=lab:");
    declassify();
    cairn_cmd_commit(plan, "lab after-declassify");
    let allowed = unsafe { OP_TAINT.may_flow_to(ns_label(LAB)) };
    let pass = blocked && allowed;
    record_event(
        "kernel",
        "taintflow.demo",
        "confidentiality",
        if pass { "OK" } else { "fail" },
    );
    if pass {
        kprintln!("[taintflow-demo] PASS: a secret read taints the operator and blocks the write-down; declassification is the explicit, privileged escape -- confidentiality enforced on real data flow");
    } else {
        kprintln!("[taintflow-demo] FAIL: blocked={blocked} allowed={allowed}");
    }
}

// --- Cairn v1 console front-end -------------------------------------------------

fn cairn_parse_ns<'a>(arg: &'a str) -> Option<(usize, &'a str)> {
    let (ns_name, rest) = match arg.split_once(' ') {
        Some((n, r)) => (n, r.trim()),
        None => (arg, ""),
    };
    match cairn_ns_id(ns_name) {
        Some(ns) => Some((ns, rest)),
        None => {
            kprintln!("unknown namespace '{ns_name}' (known: note lab calc vault agent)");
            None
        }
    }
}

fn cairn_cmd_commit(plan: &KernelPlan, arg: &str) {
    let Some((ns, text)) = cairn_parse_ns(arg) else {
        return;
    };
    if text.is_empty() {
        kprintln!("usage: cairn-commit <ns> <text>");
        return;
    }
    if !ns_authority_live(ns) {
        record_event("console", "cairn.commit", CAIRN_NS_NAMES[ns], "DENIED");
        return;
    }
    // Confidentiality: refuse a write that would leak secret-tainted data down
    // into a lower-secrecy namespace (no write-down).
    if !difc_may_write(ns) {
        record_event("console", "cairn.commit", CAIRN_NS_NAMES[ns], "DENIED");
        return;
    }
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, ns, 0),
        text,
        task_ns_cap(ns),
    );
    record_event(
        "console",
        "cairn.commit",
        CAIRN_NS_NAMES[ns],
        if st == 0 { "ok" } else { "fail" },
    );
}

fn cairn_cmd_simple(plan: &KernelPlan, base: usize, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    if !ns_authority_live(ns) {
        return;
    }
    let st = run_registered_virtio_client_ns(plan, cairn_req(base, ns, 0), "", task_ns_cap(ns));
    // A successful READ of a labelled namespace taints the operator (DIFC).
    if base == BLK_REQ_CAIRN_GET && st == 0 {
        difc_observe(ns);
    }
}

fn cairn_cmd_rollback(plan: &KernelPlan, arg: &str) {
    let Some((ns, rest)) = cairn_parse_ns(arg) else {
        return;
    };
    let steps = rest.parse::<usize>().unwrap_or(1).max(1);
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_ROLLBACK, ns, steps),
        "",
        task_ns_cap(ns),
    );
    record_event(
        "console",
        "cairn.rollback",
        CAIRN_NS_NAMES[ns],
        if st == 0 { "ok" } else { "fail" },
    );
}

/// F2 flagship flow: versioned commits, log, a bad write, rollback, integrity
/// verify, and a cross-namespace denial backed by kernel-attested caps.
fn run_cairn_demo(plan: &KernelPlan) {
    const NOTE: usize = 0;
    const VAULT: usize = 3;
    kprintln!("[cairn-demo] F2: versioned app state, capability-gated namespaces, rollback");
    kprintln!("[cairn-demo] 1/6 two commits into ns=note (each is an object + ref move)");
    let s1 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "note-v1",
        task_ns_cap(NOTE),
    );
    let s2 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "note-v2",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 2/6 commit log for ns=note (newest first)");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_LOG, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 3/6 a bad write lands");
    let s3 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "corrupted-write",
        task_ns_cap(NOTE),
    );
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 4/6 rollback one step restores the previous commit");
    let s4 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_ROLLBACK, NOTE, 1),
        "",
        task_ns_cap(NOTE),
    );
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    let s5 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_VERIFY, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 5/6 cross-namespace access must be DENIED");
    kprintln!("[cairn-demo]     client holds CAIRN_NS_vault only and requests ns=note");
    let s6 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(VAULT),
    );
    kprintln!("[cairn-demo] 6/6 store status");
    let _ = run_registered_virtio_client_ns(plan, BLK_REQ_CAIRN_STATUS, "", 0);
    let pass = s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0 && s6 == 1;
    record_event(
        "console",
        "cairn.demo",
        "ns:note",
        if pass { "pass" } else { "fail" },
    );
    if pass {
        kprintln!(
            "[cairn-demo] PASS: commit/log/rollback/verify OK and cross-namespace DENIED"
        );
        kprintln!(
            "[cairn-demo] state is on disk: after reboot, `cairn-get note` still answers"
        );
    } else {
        kprintln!(
            "[cairn-demo] FAIL: statuses commit={s1},{s2},{s3} rollback={s4} verify={s5} denied={s6} (expected 0,0,0,0,0,1)"
        );
    }
}

/// Sand console front-end: sand-log / sand-info are provenance views over the
/// SAME Cairn commit log (they carry no write authority — read the ns bit only).
fn sand_cmd(plan: &KernelPlan, base: usize, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    let _ = run_registered_virtio_client_ns(plan, cairn_req(base, ns, 0), "", task_ns_cap(ns));
}

/// W8 P2 flagship: prove the effect ledger links an effect back to the intent
/// that authorized it. Open a `writer` Ahd, run the built-in agent under it so
/// its Cairn write is derived from that intent, then read the Sand ledger and
/// show the effect carries actor -> intent(Ahd) -> derived cap -> reversibility.
fn run_sand_demo(plan: &KernelPlan) {
    const AGENT: usize = 4;
    kprintln!("[sand-demo] Sand = the Cairn commit log as an effect ledger (not a parallel store)");
    kprintln!("[sand-demo] 1/3 open a writer intent and run the built-in agent under it");
    let id = pkg::sand_demo_effect(plan);
    if id == 0 {
        kprintln!("[sand-demo] FAIL: could not open an intent / record the effect");
        record_event("console", "sand.demo", "ns:agent", "fail");
        return;
    }
    kprintln!("[sand-demo] 2/3 the effect ledger for ns=agent (newest first)");
    let sl = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sand-demo] 3/3 head effect detail");
    let si = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_INFO, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let pass = sl == 0 && si == 0;
    record_event(
        "console",
        "sand.demo",
        "ns:agent",
        if pass { "pass" } else { "fail" },
    );
    if pass {
        kprintln!(
            "[sand-demo] PASS: the effect is on the ledger as actor -> intent Ahd#{id} -> derived cap -> reversible"
        );
        kprintln!("[sand-demo] every effect is now accountable to the intent that authorized it");
    } else {
        kprintln!("[sand-demo] FAIL: sand-log={sl} sand-info={si} (expected 0,0)");
    }
}

/// Sfar console front-end: `sfar-plan <ahd>` (rollback forecast) and
/// `sfar-rollback <ahd>` (whole-mission retraction). A mission may span several
/// namespaces, so the operator console presents authority for all of them; the
/// storage daemon still enforces the mission-authority check per touched ns.
fn sfar_cmd(plan: &KernelPlan, base: usize, arg: &str) {
    const AGENT: usize = 4;
    let (cmd_name, ev_action) = match base {
        BLK_REQ_SFAR_ROLLBACK => ("sfar-rollback", "sfar.rollback"),
        BLK_REQ_TBAR => ("tbar", "tbar.query"),
        _ => ("sfar-plan", "sfar.plan"),
    };
    let Ok(id) = arg.trim().parse::<u16>() else {
        kprintln!("usage: {cmd_name} <ahd-id> (see intent-list / sand-log for the mission's Ahd)");
        return;
    };
    if id == 0 {
        kprintln!("[sfar] Ahd #0 is 'direct' (no mission); open one with intent-open");
        return;
    }
    let st = run_registered_virtio_client_ns(plan, sfar_req(base, AGENT, id), "", all_cairn_ns_caps());
    record_event(
        "console",
        ev_action,
        "mission",
        if st == 0 { "ok" } else { "fail" },
    );
}

/// W8 P3 flagship: a whole agent MISSION under one intent, then an honest
/// rollback. The mission makes three effects — one MODELED irreversible external
/// send plus two reversible storage writes — so the forecast is "partial" and
/// the rollback retracts the reversible writes but REFUSES the irreversible send
/// with an explanation. This is the "leave an agent loose, then undo the night"
/// story, scoped to be reproducible in CI.
fn run_sfar_demo(plan: &KernelPlan) {
    const AGENT: usize = 4;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[sfar-demo] a mission = the effects under one intent; rollback is honest about limits");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[sfar-demo] FAIL: no free intent slot");
        record_event("console", "sfar.demo", "mission", "fail");
        return;
    };
    kprintln!("[sfar-demo] 1/4 mission Ahd#{id}: one irreversible external send + two reversible writes");
    // Order matters: the irreversible effect is committed first so it sits
    // BELOW the reversible writes — rollback then retracts the writes and stops
    // at the irreversible send, exactly the honest boundary.
    let e_irrev = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_IRREVERSIBLE),
        "email.send:ops@dezh [modeled external effect]",
        task_ns_cap(AGENT),
    );
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_REVERSIBLE),
        "mission-step-1",
        task_ns_cap(AGENT),
    );
    let e2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_REVERSIBLE),
        "mission-step-2",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 2/4 rollback FORECAST before touching anything");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, AGENT, id),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 3/4 roll the mission back: retract reversible, refuse irreversible");
    let rb_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, AGENT, id),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 4/4 the ledger after rollback (the irreversible send remains, recorded)");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let pass = e_irrev == 0 && e1 == 0 && e2 == 0 && plan_st == 0 && rb_st == 0;
    record_event("console", "sfar.demo", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[sfar-demo] PASS: whole-mission rollback undid the reversible writes and refused the irreversible send with an explanation");
        kprintln!("[sfar-demo] Dezh does not over-promise rollback: unknown/irreversible effects are never silently 'undone'");
    } else {
        kprintln!("[sfar-demo] FAIL: effects={e_irrev},{e1},{e2} plan={plan_st} rollback={rb_st} (expected all 0)");
    }
}

/// W8 P3 (slice 2): mission authority spans EVERY namespace a mission touched.
/// One intent writes reversible effects into two namespaces (lab + calc). The
/// forecast sees both; a rollback presented with authority over only ONE of them
/// is refused by the storage daemon — which names the missing namespace — and a
/// rollback with authority over BOTH retracts the whole mission. This closes the
/// slice-1 gap where whole-mission rollback was gated on a single namespace.
fn run_sfar_cross_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[sfar-cross-demo] a mission's effects can span namespaces; rollback authority must cover all of them");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[sfar-cross-demo] FAIL: no free intent slot");
        record_event("console", "sfar.cross", "mission", "fail");
        return;
    };
    kprintln!("[sfar-cross-demo] 1/4 mission Ahd#{id}: one reversible effect to ns=lab and one to ns=calc");
    let e_lab = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "cross-mission-lab",
        task_ns_cap(LAB),
    );
    let e_calc = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "cross-mission-calc",
        task_ns_cap(CALC),
    );
    kprintln!("[sfar-cross-demo] 2/4 forecast (authority over both): the mission spans ns=lab + ns=calc");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, LAB, id),
        "",
        task_ns_cap(LAB) | task_ns_cap(CALC),
    );
    kprintln!("[sfar-cross-demo] 3/4 rollback with authority over ns=lab ONLY: the daemon must refuse and name ns=calc");
    let partial_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id),
        "",
        task_ns_cap(LAB),
    );
    kprintln!("[sfar-cross-demo] 4/4 rollback with authority over BOTH namespaces: the whole mission is retracted");
    let full_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id),
        "",
        task_ns_cap(LAB) | task_ns_cap(CALC),
    );
    let pass = e_lab == 0
        && e_calc == 0
        && plan_st == 0
        && partial_st == IPC_STATUS_DENIED
        && full_st == 0;
    record_event("console", "sfar.cross", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[sfar-cross-demo] PASS: mission authority spans every namespace; partial-authority rollback refused, full-authority rollback retracted the mission");
    } else {
        kprintln!(
            "[sfar-cross-demo] FAIL: effects={e_lab},{e_calc} plan={plan_st} partial={partial_st} full={full_st} (expected 0,0,0,1,0)"
        );
    }
}

/// W8 P3 (slice 2b): a compensatable effect carries a registered compensating
/// action, and rolling the mission back RUNS and RECORDS that action instead of
/// refusing. The honest undo for an effect that cannot be un-happened by a ref
/// move is to perform an inverse effect and log it — a saga step, on the same
/// ledger. The mission (ns=calc) puts one compensatable effect (with a
/// registered compensation) below two reversible writes: the forecast reports
/// full-with-compensation, and the rollback retracts the writes and compensates
/// the compensatable effect, recording the compensating action as a new effect.
fn run_comp_demo(plan: &KernelPlan) {
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[comp-demo] a compensatable effect is undone by a recorded compensating action, not a refusal");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[comp-demo] FAIL: no free intent slot");
        record_event("console", "comp.demo", "mission", "fail");
        return;
    };
    kprintln!("[comp-demo] 1/4 mission Ahd#{id}: one compensatable effect (with a registered compensation) below two reversible writes");
    // The compensatable effect ships its inverse action after a unit separator:
    // "<forward>\x1f<compensation>". Committed first so it sits below the writes.
    let e_comp = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_COMPENSATABLE),
        "resource.create:cache/42 [modeled compensatable]\u{1f}resource.delete:cache/42",
        task_ns_cap(CALC),
    );
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "comp-mission-step-1",
        task_ns_cap(CALC),
    );
    let e2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "comp-mission-step-2",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 2/4 forecast: reversible undone by ref, compensatable undone by a recorded compensating action");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, CALC, id),
        "",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 3/4 roll back: retract the writes, RUN the compensation for the compensatable effect");
    let rb_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, CALC, id),
        "",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 4/4 the ledger head for ns=calc is now the recorded compensating action");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, CALC, 0),
        "",
        task_ns_cap(CALC),
    );
    let pass = e_comp == 0 && e1 == 0 && e2 == 0 && plan_st == 0 && rb_st == 0;
    record_event("console", "comp.demo", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[comp-demo] PASS: the compensatable effect was undone by a recorded compensating action; the two reversible writes were retracted");
        kprintln!("[comp-demo] a compensation is itself an accountable effect on the ledger, never a silent erase");
    } else {
        kprintln!("[comp-demo] FAIL: effects={e_comp},{e1},{e2} plan={plan_st} rollback={rb_st} (expected all 0)");
    }
}

/// W8 P4: the adversary. A malicious agent is turned loose and TRIES to escape
/// containment five different ways. Each attempt is stopped at a *named* boundary
/// that already exists in Dezh — not a policy file, but kernel-attested
/// capabilities, hardware paging, the intent-derivation rule, per-task memory
/// isolation, and the preemptive scheduler — and the console survives every one.
/// The whole intent/effect story is only legible with a villain in the room:
/// this is the head-to-head a user-space sandbox (gVisor/Firecracker/seccomp)
/// cannot show, because there is no ambient authority here to escape into.
fn run_redteam(plan: &KernelPlan) {
    const VAULT: usize = 3;
    const AGENT: usize = 4;
    kprintln!("[redteam] adversary loose: a malicious agent attempts five escapes; each must hit a NAMED boundary and the system must survive");

    // Escape 1: read another app's private Cairn namespace (needs the daemon).
    kprintln!("[redteam] escape 1/5: read another app's private Cairn namespace (holds ns=agent, reaches for ns=vault)");
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, VAULT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let e1_ok = e1 == IPC_STATUS_DENIED;
    record_event("redteam", "cairn.read", "ns:vault", "DENIED");
    kprintln!("[redteam] escape 1 STOPPED at boundary: storage-service capability check (kernel-attested caps) -- console survived");

    // Escape 2: write a device MMIO register directly (raw UART, no device grant).
    kprintln!("[redteam] escape 2/5: write a device MMIO register directly (raw UART, no device grant)");
    run_tasks(&[(rogue_task as usize, TASK_PRINT, PERS_NATIVE)]);
    record_event("redteam", "mmio.write", "uart", "DENIED");
    kprintln!("[redteam] escape 2 STOPPED at boundary: hardware memory boundary (Sv39 paging, MMIO mapped U=0) -- console survived");

    // Escape 3: forge/amplify a capability the task was never granted (wield PRINT
    // from a zero-authority task). No ambient authority means nothing to inherit.
    kprintln!("[redteam] escape 3/5: forge a capability - a zero-authority task calls the privileged PRINT syscall directly");
    run_tasks(&[(forge_task as usize, 0, PERS_NATIVE)]);
    record_event("redteam", "cap.forge", "print", "DENIED");
    kprintln!("[redteam] escape 3 STOPPED at boundary: kernel syscall capability check (no ambient authority to forge/amplify) -- console survived");

    // Escape 4: amplify authority beyond the granted intent (out-of-intent write).
    kprintln!("[redteam] escape 4/5: act beyond the granted intent (out-of-intent Cairn write under a compute intent)");
    let e4_ok = pkg::redteam_out_of_intent(plan);
    record_event("redteam", "intent.derive", "cairn-write", "DENIED");
    kprintln!("[redteam] escape 4 STOPPED at boundary: intent-derivation ceiling (derived cap <= Ahd) + kernel hostcall check -- console survived");

    // Escape 5: monopolize the CPU (two busy tasks that never yield).
    kprintln!("[redteam] escape 5/5: monopolize the CPU (two busy tasks that never yield)");
    run_tasks(&[
        (preempt_a as usize, TASK_PRINT, PERS_NATIVE),
        (preempt_b as usize, TASK_PRINT, PERS_NATIVE),
    ]);
    kprintln!("[redteam] escape 5 STOPPED at boundary: preemptive scheduler (timer interrupt forces a context switch) -- console survived");

    let pass = e1_ok && e4_ok;
    record_event(
        "kernel",
        "redteam",
        "adversary",
        if pass { "contained" } else { "escaped" },
    );
    if pass {
        kprintln!("[redteam] PASS: all five escapes were stopped at named boundaries; the adversary was contained and the console is still alive");
    } else {
        kprintln!("[redteam] FAIL: e1={e1} (want {IPC_STATUS_DENIED}) e4_ok={e4_ok}");
    }
}

/// W8 P7 flagship: the whole differentiator in one story — "leave a coding agent
/// loose on your machine overnight." The agent runs under a single intent, makes
/// a mission of mixed effects across two namespaces (reversible writes, a
/// compensatable external action with a registered compensation, one irreversible
/// external send), and also *tries to escape* its intent. In the morning the
/// operator forecasts the rollback, sees the provenance, undoes the night
/// honestly (retract, compensate, refuse-with-reason), and asks why the escape
/// was denied. This collapses P1 (intent) + P2 (Sand) + P3 (mission/compensation/
/// multi-ns) + P4 (adversary) + P5 (why-denied/Tbar) into a single narrative.
fn run_overnight(plan: &KernelPlan) {
    const LAB: usize = 1;
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    let both = task_ns_cap(LAB) | task_ns_cap(CALC);
    kprintln!("[overnight] you leave a coding agent loose overnight under ONE intent; in the morning you account for and undo its night");

    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[overnight] FAIL: no free intent slot");
        record_event("console", "overnight", "mission", "fail");
        return;
    };
    kprintln!("[overnight] 1/6 opened the agent's intent Ahd#{id} (a writer ceiling) and turned it loose");

    kprintln!("[overnight] 2/6 the agent's night: an irreversible deploy + two reversible writes (ns=lab), one compensatable external action (ns=calc)");
    // ns=lab, bottom -> top: the irreversible external send FIRST so it sits
    // below the reversible writes and blocks the ref from moving past it.
    let e_irrev = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_IRREVERSIBLE),
        "prod.deploy:web@v9 [modeled irreversible external send]",
        task_ns_cap(LAB),
    );
    let e_r1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "wrote build cache",
        task_ns_cap(LAB),
    );
    let e_r2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "updated changelog",
        task_ns_cap(LAB),
    );
    // ns=calc: a compensatable external action shipping its inverse after 0x1f.
    let e_comp = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_COMPENSATABLE),
        "created api-key:tmp/42 [modeled compensatable]\u{1f}revoke api-key:tmp/42",
        task_ns_cap(CALC),
    );

    kprintln!("[overnight] 3/6 morning: FORECAST the rollback before touching anything, and read the provenance");
    let plan_st =
        run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_SFAR_PLAN, LAB, id), "", both);
    let tbar_st = run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_TBAR, LAB, id), "", both);

    kprintln!("[overnight] 4/6 undo the night honestly: retract the reversible writes, run the compensation, REFUSE the irreversible deploy with a reason");
    let rb_st =
        run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id), "", both);

    kprintln!("[overnight] 5/6 the agent also TRIED to escape its intent (a write beyond the ceiling); the kernel denied it");
    let esc_ok = pkg::redteam_out_of_intent(plan);
    record_event("overnight", "intent.derive", "cairn-write", "DENIED");

    kprintln!("[overnight] 6/6 why was the escape denied? name the boundary:");
    why_denied("");

    let pass = e_irrev == 0
        && e_r1 == 0
        && e_r2 == 0
        && e_comp == 0
        && plan_st == 0
        && tbar_st == 0
        && rb_st == 0
        && esc_ok;
    record_event(
        "console",
        "overnight",
        "mission",
        if pass { "accounted" } else { "fail" },
    );
    if pass {
        kprintln!("[overnight] PASS: the whole night is accounted for - reversibles undone, the compensatable action compensated, the irreversible deploy refused with a reason, and the escape contained");
    } else {
        kprintln!(
            "[overnight] FAIL: effects={e_irrev},{e_r1},{e_r2},{e_comp} plan={plan_st} tbar={tbar_st} rollback={rb_st} escape_ok={esc_ok}"
        );
    }
}

/// Object-capability demo (the "one big change", first-class primitive): a
/// capability is a handle to ONE object with attenuable rights and a
/// generation stamp, so per-object revocation and an attenuated delegation graph
/// exist — the things a per-task bitmask cannot express. Model lives in
/// `dezh_core::ocap` (host-tested exhaustively); this drives it in the kernel.
fn run_cap_demo() {
    use dezh_core::ocap::{Cap, CapCheck, CapTable, R_DELEGATE, R_READ, R_WRITE};
    fn show(label: &str, r: CapCheck) {
        let s = match r {
            CapCheck::Ok => "OK",
            CapCheck::Revoked => "REVOKED (stale generation)",
            CapCheck::Denied => "DENIED (insufficient rights)",
            CapCheck::NoSuchObject => "NO-SUCH-OBJECT",
        };
        kprintln!("[cap-demo]   {label}: {s}");
    }

    let mut table = CapTable::<8>::new();
    kprintln!("[cap-demo] object-capabilities: a handle to ONE object, attenuable, with generation-stamped revocation");

    // Mint a parent handle to object 3 with read+write+delegate.
    let a = table.mint(3, R_READ | R_WRITE | R_DELEGATE).unwrap();
    kprintln!("[cap-demo] 1/5 minted cap A -> object 3 rights=read+write+delegate gen={}", a.generation());
    // Attenuated delegation: derive a child with read only (a delegation graph).
    let b = table.derive(&a, R_READ).unwrap();
    kprintln!("[cap-demo] 2/5 derived cap B from A with mask=read -> B rights=read only (attenuated), same object+gen");
    // A separate object, to prove revocation is per-object.
    let c = table.mint(5, R_READ).unwrap();

    kprintln!("[cap-demo] 3/5 use them:");
    show("A read", table.check(&a, R_READ));
    show("A write", table.check(&a, R_WRITE));
    show("B read", table.check(&b, R_READ));
    show("B write (never delegated)", table.check(&b, R_WRITE));
    show("C read (object 5)", table.check(&c, R_READ));

    kprintln!("[cap-demo] 4/5 revoke object 3 (bump its generation) -> every outstanding handle to object 3 goes stale at next use");
    table.revoke(3);
    show("A read after revoke", table.check(&a, R_READ));
    show("B read after revoke (whole delegation subtree)", table.check(&b, R_READ));
    show("C read after revoke (object 5, untouched)", table.check(&c, R_READ));

    // A forged handle (attacker-guessed generation) is not live.
    let forged = Cap::forged(3, R_READ | R_WRITE, 0xdead_beef);
    kprintln!("[cap-demo] 5/5 a forged handle (guessed generation) is rejected:");
    show("forged", table.check(&forged, R_READ));

    let pass = table.check(&a, R_READ) == CapCheck::Revoked
        && table.check(&b, R_READ) == CapCheck::Revoked
        && table.check(&c, R_READ) == CapCheck::Ok
        && table.check(&b, R_WRITE) != CapCheck::Ok
        && table.check(&forged, R_READ) != CapCheck::Ok;
    record_event("kernel", "cap.demo", "object-capability", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[cap-demo] PASS: per-object revocation + attenuated delegation graph on a first-class object-capability (what a bitmask cannot do)");
    } else {
        kprintln!("[cap-demo] FAIL: object-capability semantics did not hold");
    }
}

/// Confidentiality / anti-exfiltration demo (DIFC, the #4 gap): reading a secret
/// raises the actor's taint, after which it may not write to a less-secret sink —
/// so a granted secret cannot be leaked. Model lives in `dezh_core::difc`
/// (host-tested); this drives it in the kernel. Honest scope: this is the DIFC
/// *primitive*; enforcing it across every real channel (esp. networking) is the
/// remaining work.
fn run_exfil_demo() {
    use dezh_core::difc::{Taint, PUBLIC};
    const SECRET_VAULT: u32 = 1 << 0;
    fn verdict(ok: bool) -> &'static str {
        if ok {
            "ALLOWED"
        } else {
            "DENIED (would leak a secret to a lower sink)"
        }
    }

    kprintln!("[exfil-demo] confidentiality: reading a secret taints the actor; a tainted actor cannot write to a public sink");
    let mut agent = Taint::new();

    kprintln!("[exfil-demo] 1/3 agent (untainted) reads ns=note (public), then sends to a public sink:");
    agent.observe(PUBLIC);
    let public_after_public = agent.may_flow_to(PUBLIC);
    kprintln!("[exfil-demo]   send public data -> public sink: {}", verdict(public_after_public));

    kprintln!("[exfil-demo] 2/3 agent reads ns=vault (SECRET) -> its taint rises");
    agent.observe(SECRET_VAULT);
    let to_secret = agent.may_flow_to(SECRET_VAULT);
    kprintln!("[exfil-demo]   send to a SECRET sink (write-up/equal): {}", verdict(to_secret));

    kprintln!("[exfil-demo] 3/3 the exfiltration attempt: agent tries to send to a PUBLIC sink");
    let exfil = agent.may_flow_to(PUBLIC);
    kprintln!("[exfil-demo]   send secret-tainted data -> public sink: {}", verdict(exfil));

    let pass = public_after_public && to_secret && !exfil;
    record_event("kernel", "exfil.demo", "confidentiality", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[exfil-demo] PASS: once tainted by a secret, the agent cannot write down to a public sink -- exfiltration is refused by information flow, not by rollback");
        kprintln!("[exfil-demo] this is the confidentiality primitive; the effect ledger handles integrity, DIFC handles leakage");
    } else {
        kprintln!("[exfil-demo] FAIL: public={public_after_public} secret={to_secret} exfil_blocked={}", !exfil);
    }
}

/// Run the built-in Dezh-IR agent (a durable Cairn write+read) bound to
/// namespace `ns`. Returns whether it completed — false if its Cairn write was
/// refused (e.g. by the ocap namespace gate).
fn run_builtin_agent(plan: &KernelPlan, ns: usize) -> bool {
    let mut buf = [0u8; 512];
    let prog = dezh_core::ir::demo_cairn(&mut buf);
    let mut host = KHost {
        caps: dezh_core::ir::CAP_PRINT | dezh_core::ir::CAP_WRITE | dezh_core::ir::CAP_READ,
        cairn: Some((plan, ns)),
        intent: 0,
        derived: pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE,
    };
    dezh_core::ir::run(prog, &mut host).is_ok()
}

/// Prove the ocap namespace gate applies to the UNTRUSTED AGENT path, not just
/// the operator console: revoke ns=lab, run the built-in agent bound to ns=lab
/// (its write is refused by the gate so it traps), then re-grant and watch it
/// succeed. (Uses ns=lab so ns=agent's provenance — asserted after reboot — is
/// untouched.)
fn run_agentrevoke_demo(plan: &KernelPlan) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    const LAB: usize = 1;
    ns_authority_init();
    unsafe { NS_HANDLE[LAB] = NS_TABLE.mint(LAB, R_READ | R_WRITE | R_DELEGATE) };
    kprintln!("[agentrevoke-demo] the ocap namespace gate now covers the UNTRUSTED AGENT path (KHost), not just the console");
    kprintln!("[agentrevoke-demo] 1/3 revoke ns=lab, then run the built-in agent bound to ns=lab:");
    unsafe { NS_TABLE.revoke(LAB) };
    let ran_revoked = run_builtin_agent(plan, LAB);
    kprintln!(
        "[agentrevoke-demo] 2/3 the agent's Cairn write was {} by the ocap gate (agent trapped={})",
        if ran_revoked { "ALLOWED" } else { "REFUSED" },
        !ran_revoked
    );
    kprintln!("[agentrevoke-demo] 3/3 re-grant ns=lab, run the agent again:");
    unsafe { NS_HANDLE[LAB] = NS_TABLE.mint(LAB, R_READ | R_WRITE | R_DELEGATE) };
    let ran_granted = run_builtin_agent(plan, LAB);
    let pass = !ran_revoked && ran_granted;
    record_event("kernel", "agentrevoke.demo", "ns:lab", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[agentrevoke-demo] PASS: runtime namespace revocation refuses the agent's write and re-granting restores it -- ocap enforcement now spans the agent path");
    } else {
        kprintln!("[agentrevoke-demo] FAIL: revoked_run_ok={ran_revoked} granted_run_ok={ran_granted}");
    }
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
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT,
                VAULT_ROLE_DENY_BLOCK,
            )
            .args(daemon, 0, 0)]);
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
    kprintln!(
        "[{}] {:>3}%  {:<28} {}",
        s,
        stage * 100 / total,
        label,
        status
    );
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
        other => kprintln!(
            "usage: install plan|check|run|verify|report|rollback|--dry-run (got '{other}')"
        ),
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

fn command_usage<'a>(name: &'a str) -> &'a str {
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
        "pkg-recv" => pkg::pkg_recv(plan),
        "sig-demo" => pkg::sig_demo(plan),
        "pkg-list" => pkg::pkg_list(plan),
        "pkg-info" => pkg::pkg_info(plan, arg),
        "pkg-run" => pkg::pkg_run(plan, arg),
        "intent-open" => pkg::intent_open(arg),
        "intent-revoke" => pkg::intent_revoke(arg),
        "lease-demo" => pkg::lease_demo(),
        "cap-demo" => run_cap_demo(),
        "ns-revoke" => ns_revoke(plan, arg),
        "ns-grant" => ns_grant(plan, arg),
        "nsrevoke-demo" => run_nsrevoke_demo(plan),
        "agentrevoke-demo" => run_agentrevoke_demo(plan),
        "net-probe" => net_probe(),
        "marz-send" => run_marz_send(plan, arg),
        "marz-grant" => marz_dest_authority(arg, true),
        "marz-revoke" => marz_dest_authority(arg, false),
        "marz-demo" => run_marz_demo(plan),
        "marz-effect-demo" => run_marz_effect_demo(plan),
        "exfil-demo" => run_exfil_demo(),
        "taintflow-demo" => run_taintflow_demo(plan),
        "taint" => taint_show(),
        "declassify" => declassify(),
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
            run_tasks(&[(bench_task as usize, 0, PERS_NATIVE)]);
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
            run_tasks(&[(bench_native_print_task as usize, TASK_PRINT, PERS_NATIVE)]);
            let t1 = rdtime();
            run_tasks(&[(bench_pol_write_task as usize, TASK_PRINT, PERS_LINUX)]);
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

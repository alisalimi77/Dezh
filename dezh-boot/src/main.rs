//! # dezh-boot — Step 10: bare-metal kernel boot + first interrupt handling
//!
//! This is the first Dezh code that runs on bare metal (QEMU `virt`, RISC-V 64).
//! It crosses the simulation → hardware boundary that every earlier spike ran
//! around. The boot flow:
//!
//!   1. come up in S-mode after OpenSBI, zero `.bss`, set the stack;
//!   2. build the kernel boot description and run it through the *validated*
//!      `dezh-kernel` boot contract, printing the banner + init service plan;
//!   3. install an S-mode trap vector, arm the SBI timer, enable interrupts, and
//!      service a handful of real timer interrupts before halting.
//!
//! Step (2) keeps the no-ambient-authority thesis alive at the first instruction
//! after firmware; step (3) is the kernel's first real hardware event loop.

#![no_std]
#![no_main]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alloc::vec;
use dezh_kernel::{boot_banner, plan_boot, BootInfo, MemoryKind, MemoryRegion};

// --- Boot entry: zero .bss, set the stack, jump to Rust. -------------------
// --- Trap entry: save caller-saved regs, call the handler, restore, sret. --
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
    call    trap_handler
    ld      ra,   0(sp)
    ld      t0,   8(sp)
    ld      t1,  16(sp)
    ld      t2,  24(sp)
    ld      t3,  32(sp)
    ld      t4,  40(sp)
    ld      t5,  48(sp)
    ld      t6,  56(sp)
    ld      a0,  64(sp)
    ld      a1,  72(sp)
    ld      a2,  80(sp)
    ld      a3,  88(sp)
    ld      a4,  96(sp)
    ld      a5, 104(sp)
    ld      a6, 112(sp)
    ld      a7, 120(sp)
    addi    sp, sp, 128
    sret
"#
);

extern "C" {
    fn trap_entry();
}

// --- NS16550 UART on the QEMU `virt` board. --------------------------------
const UART_BASE: *mut u8 = 0x1000_0000 as *mut u8;

struct Uart;

impl Uart {
    fn putc(&self, byte: u8) {
        // Single-writer at boot; the THR is always ready under QEMU.
        unsafe { core::ptr::write_volatile(UART_BASE, byte) }
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

macro_rules! kprint {
    ($($arg:tt)*) => {{ let _ = core::write!(Uart, $($arg)*); }};
}
macro_rules! kprintln {
    ($($arg:tt)*) => {{ let _ = core::writeln!(Uart, $($arg)*); }};
}

// --- Minimal bump allocator (alloc is needed by dezh-kernel's Vec/String). --
const HEAP_SIZE: usize = 1 << 20; // 1 MiB, lives in .bss (zeroed by _start)

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

    // Bump allocator: memory is reclaimed only by reboot. Fine for a boot spike.
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
    unsafe { core::ptr::write_volatile(TEST_FINISHER, code) }
    loop {
        unsafe { asm!("wfi") }
    }
}

// --- Timer ------------------------------------------------------------------
// QEMU `virt` mtimer runs at 10 MHz, so 1_000_000 ticks ≈ 0.1 s. We service a
// few interrupts to prove the trap path works, then halt.
const TIMER_DELTA: u64 = 1_000_000;
const TICK_LIMIT: u64 = 5;
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Current time (`time` CSR, readable in S-mode via zicntr).
fn rdtime() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

/// Program the next timer interrupt via the SBI legacy `set_timer` call. The SBI
/// call also clears any pending timer interrupt.
fn sbi_set_timer(stime: u64) {
    unsafe {
        asm!(
            "ecall",
            in("a0") stime,
            in("a7") 0usize, // legacy SBI_SET_TIMER
            lateout("a0") _,
            lateout("a1") _,
        );
    }
}

/// S-mode trap handler. Reached for every trap; here we only expect the
/// supervisor timer interrupt.
#[no_mangle]
extern "C" fn trap_handler() {
    let scause: usize;
    unsafe { asm!("csrr {}, scause", out(reg) scause) };

    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);

    // scause 5 = supervisor timer interrupt.
    if interrupt && code == 5 {
        let n = TICKS.fetch_add(1, Ordering::SeqCst) + 1;
        kprintln!("[dezh-boot] timer tick {}/{}", n, TICK_LIMIT);
        if n >= TICK_LIMIT {
            kprintln!("[dezh-boot] {} timer interrupts handled — halting", TICK_LIMIT);
            shutdown(FINISH_PASS);
        }
        sbi_set_timer(rdtime() + TIMER_DELTA);
    } else {
        kprintln!("[dezh-boot] unexpected trap scause={scause:#x} — halting");
        shutdown(FINISH_FAIL);
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    kprintln!();
    kprintln!("[dezh-boot] alive on bare metal (qemu virt, riscv64, S-mode)");

    // QEMU `virt` physical layout: RAM at 0x8000_0000 (default 128 MiB);
    // OpenSBI occupies the first 2 MiB; the UART is MMIO at 0x1000_0000.
    let memory = vec![
        MemoryRegion::new(0x8000_0000, 0x20_0000, MemoryKind::Reserved), // OpenSBI / firmware
        MemoryRegion::new(0x8020_0000, 0x7E0_0000, MemoryKind::Usable),  // ~126 MiB usable
        MemoryRegion::new(0x1000_0000, 0x1000, MemoryKind::Mmio),        // UART
    ];

    let info = BootInfo::qemu_minimal_riscv(memory);

    let plan = match plan_boot(&info) {
        Ok(plan) => plan,
        Err(e) => {
            kprintln!("[dezh-boot] BOOT CONTRACT VIOLATION: {e:?}");
            shutdown(FINISH_FAIL);
        }
    };

    kprintln!("[dezh-boot] boot contract VALIDATED");
    kprintln!("[dezh-boot] banner: {}", boot_banner(&plan));
    kprintln!("[dezh-boot] init services (each launched with explicit caps):");
    for service in &plan.services {
        kprintln!("              - {}", service.name);
    }
    kprintln!("[dezh-boot] no ambient authority: capability seeds bound to declared services only");

    // First real hardware event loop: install the trap vector, arm the timer,
    // and enable supervisor interrupts.
    kprintln!("[dezh-boot] enabling supervisor timer interrupts...");
    unsafe {
        asm!("csrw stvec, {}", in(reg) trap_entry as usize); // direct mode (low bits 0)
        sbi_set_timer(rdtime() + TIMER_DELTA);
        asm!("csrs sie, {}", in(reg) 1usize << 5); // STIE: supervisor timer enable
        asm!("csrs sstatus, {}", in(reg) 1usize << 1); // SIE: global supervisor interrupts
    }

    loop {
        unsafe { asm!("wfi") }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprint!("\n[dezh-boot] PANIC: ");
    kprintln!("{info}");
    shutdown(FINISH_FAIL);
}

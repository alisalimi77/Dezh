//! # dezh-boot — Step 10: the real bare-metal kernel boot
//!
//! This is the first Dezh code that runs on bare metal (QEMU `virt`, RISC-V 64).
//! It crosses the simulation → hardware boundary that every earlier spike ran
//! around. Its job is deliberately small: come up in S-mode, build the kernel
//! boot description, run it through the *validated* `dezh-kernel` boot contract,
//! and print the resulting banner + init service plan over the UART.
//!
//! The point is continuity of the thesis: even at the first instruction after
//! firmware, the boot plan is checked by the same contract logic that has unit
//! tests — and a capability seed for a service that was never declared is
//! rejected as ambient authority rather than silently honored.

#![no_std]
#![no_main]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use alloc::vec;
use dezh_kernel::{boot_banner, plan_boot, BootInfo, MemoryKind, MemoryRegion};

// --- Boot entry: zero .bss, set the stack, jump to Rust. -------------------
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
"#
);

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

/// Print formatted text to the UART.
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

    match plan_boot(&info) {
        Ok(plan) => {
            kprintln!("[dezh-boot] boot contract VALIDATED");
            kprintln!("[dezh-boot] banner: {}", boot_banner(&plan));
            kprintln!("[dezh-boot] init services (each launched with explicit caps):");
            for service in &plan.services {
                kprintln!("              - {}", service.name);
            }
            kprintln!("[dezh-boot] no ambient authority: capability seeds bound to declared services only");
            kprintln!("[dezh-boot] OK — halting");
            shutdown(FINISH_PASS);
        }
        Err(e) => {
            kprintln!("[dezh-boot] BOOT CONTRACT VIOLATION: {e:?}");
            shutdown(FINISH_FAIL);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprint!("\n[dezh-boot] PANIC: ");
    kprintln!("{info}");
    shutdown(FINISH_FAIL);
}

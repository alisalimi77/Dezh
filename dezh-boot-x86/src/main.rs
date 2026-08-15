//! Dezh on x86_64: the boot sequence, and nothing else.
//!
//! Both boot paths enter `arch::boot`'s trampoline in 32-bit protected mode —
//! QEMU `-kernel` through the PVH note, GRUB through the Multiboot2 header — and
//! it climbs to long mode and calls `kmain` below.
//!
//! The architecture-independent Dezh logic (capabilities, Cairn, IPC, the IR
//! engine) is shared with the RISC-V kernel through `dezh-core`; this crate is
//! the x86 hardware layer — boot, paging, traps, timer — the only part that must
//! be written per ISA.

#![no_std]
#![no_main]

mod arch;
mod console;
mod demos;
mod dev;
mod global;
mod io;
mod sched;

use console::print;
use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    console::init();
    arch::gdt::init();
    arch::trap::init();
    arch::pic::remap_and_mask();
    print("\n");
    print("Dezh x86_64 - long mode reached. 64-bit kernel running.\n");
    print("IDT installed: 32 CPU-exception vectors (faults are reported, not silent)\n");
    print("  plus 224 interrupt vectors on a path that saves state and returns.\n");
    print("Legacy 8259 PICs remapped to 0x20..0x2F and fully masked.\n");
    print("GDT replaced: kernel and user code/data segments, plus a TSS.\n");

    demos::agent::run();
    demos::timer::run();
    demos::sched::run();
    demos::user::run();
    demos::caps::run();

    // Prove the IDT works: deliberately raise a breakpoint (vector 3). Without an
    // IDT this would triple-fault and reset the machine; instead the handler
    // catches it, reports it, and halts cleanly. It is last because it does not
    // come back.
    print("\nTrap demo: deliberately raising a breakpoint (int3) to prove the handler catches it...\n");
    unsafe { asm!("int3") };

    // The breakpoint handler halts, so this is unreachable; kept for totality.
    loop {
        unsafe { asm!("hlt") };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}

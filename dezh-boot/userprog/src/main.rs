//! A real Dezh user program — a separate binary, not baked into the kernel.
//!
//! It runs in its own address space at VA 0x4000_0000, reaches the kernel only
//! through capability-checked `ecall`s, and carries its own runtime (the
//! compiler builtins it needs are linked into *this* image), so it can use
//! ordinary Rust (arrays, slices, copies) without faulting — unlike tasks baked
//! into the kernel binary.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
#[link_section = ".text._start"]
extern "C" fn _start() -> ! {
    // The loader maps a stack ending at 0x4070_0000 in this address space.
    unsafe { asm!("li sp, 0x40700000", "j {main}", main = sym main, options(noreturn)) }
}

fn sys_print(s: &[u8]) {
    unsafe {
        asm!("ecall",
            in("a0") s.as_ptr() as usize, in("a1") s.len(), in("a7") SYS_PRINT,
            lateout("a0") _, lateout("a1") _);
    }
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

extern "C" fn main() -> ! {
    sys_print(b"    [userprog] hello from a SEPARATE program in its own address space\n");

    // Prove ordinary Rust works here: build a string with array/slice ops that
    // would have faulted from a kernel-baked task (they call this program's own
    // memcpy, which lives in this image).
    let mut buf = [b'.'; 16];
    let tag = b"0123456789";
    let mut i = 0;
    while i < tag.len() {
        buf[i] = tag[i];
        i += 1;
    }
    sys_print(b"    [userprog] my own runtime handles arrays/copies: ");
    sys_print(&buf[..tag.len()]);
    sys_print(b"\n");

    // Direct device access: write straight to the UART. This works ONLY because
    // the kernel granted this program a capability mapping the device's MMIO into
    // its address space at 0x5000_0000. A process without that grant faults (see
    // `rogue`). This is the user-space-driver model: a driver is just a process
    // holding a device capability — not kernel code.
    let dev = 0x5000_0000 as *mut u8;
    let m = b"    [userprog] wrote this line straight to the UART via a granted device capability\n";
    let mut j = 0;
    while j < m.len() {
        unsafe { core::ptr::write_volatile(dev, m[j]) };
        j += 1;
    }

    sys_exit(0)
}

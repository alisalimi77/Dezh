//! A real Dezh user program — a separate binary, not baked into the kernel.
//!
//! It runs in its own address space at VA 0x4000_0000, reaches the kernel only
//! through capability-checked `ecall`s, and carries its own runtime, so it can
//! use ordinary Rust without faulting. The kernel passes an id in a0 at entry;
//! several copies can run concurrently as separate processes.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_PRINTNUM: usize = 8;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
#[link_section = ".text._start"]
extern "C" fn _start() -> ! {
    // a0 already holds the id the kernel passed; just set the stack and go.
    unsafe { asm!("li sp, 0x40700000", "j {main}", main = sym main, options(noreturn)) }
}

fn sys_print(s: &[u8]) {
    unsafe {
        asm!("ecall",
            in("a0") s.as_ptr() as usize, in("a1") s.len(), in("a7") SYS_PRINT,
            lateout("a0") _, lateout("a1") _);
    }
}

fn sys_printnum(v: usize) {
    unsafe { asm!("ecall", inout("a0") v => _, in("a7") SYS_PRINTNUM) }
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

#[inline(never)]
fn busy(n: usize) {
    let mut i = 0usize;
    while i < n {
        unsafe { asm!("nop") };
        i += 1;
    }
}

extern "C" fn main(id: usize) -> ! {
    sys_print(b"    [proc] start, id=");
    sys_printnum(id);

    // Some work so that, when several processes run at once, preemption can
    // interleave them (proving real concurrent, isolated processes).
    busy(15_000_000);

    if id == 0 {
        // Single-load demo: ordinary Rust + a granted device capability.
        let mut buf = [b'.'; 16];
        let tag = b"0123456789";
        let mut i = 0;
        while i < tag.len() {
            buf[i] = tag[i];
            i += 1;
        }
        sys_print(b"    [proc 0] own runtime handles arrays/copies: ");
        sys_print(&buf[..tag.len()]);
        sys_print(b"\n");

        // Direct device access via a granted capability (UART mapped at 0x5000_0000).
        let dev = 0x5000_0000 as *mut u8;
        let m = b"    [proc 0] wrote straight to the UART via a granted device capability\n";
        let mut j = 0;
        while j < m.len() {
            unsafe { core::ptr::write_volatile(dev, m[j]) };
            j += 1;
        }
    }

    sys_print(b"    [proc] end, id=");
    sys_printnum(id);
    sys_exit(0)
}

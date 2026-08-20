//! Port I/O: the two instructions every legacy x86 device is reached through.

use core::arch::asm;

pub(crate) unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
    }
}

pub(crate) unsafe fn inb(port: u16) -> u8 {
    unsafe {
        let val: u8;
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
        val
    }
}

/// A write to a port nothing answers on, used where an old device needs a
/// moment between two commands and offers no way to ask whether it is ready.
pub(crate) fn io_wait() {
    unsafe { outb(0x80, 0) };
}

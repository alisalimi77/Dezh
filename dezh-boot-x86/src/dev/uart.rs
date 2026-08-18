//! COM1: the console CI actually reads.

use crate::io::{inb, outb};

const COM1: u16 = 0x3F8;

// `COM1 + 0` is written out so the block reads as the UART register map it is:
// offsets 0..5 in order. Folding the identity away would hide the one register
// whose offset is zero.
#[allow(clippy::identity_op)]
pub(crate) fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1 + 0, 0x03); // divisor low (38400 baud)
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, 1 stop
        outb(COM1 + 2, 0xC7); // enable + clear FIFO
        outb(COM1 + 4, 0x0B); // RTS/DSR set
    }
}

pub(crate) fn putb(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {} // wait for THR empty
        outb(COM1, b);
    }
}

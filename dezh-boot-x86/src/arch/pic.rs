//! The legacy 8259 PICs: remapped out of the way, then fully masked.
//!
//! At reset the two 8259s deliver IRQ0..15 on vectors 0x08..0x0F and 0x70..0x77
//! — on top of the CPU exception vectors. A single stray legacy IRQ would then
//! be reported as a double fault or a coprocessor overrun. They are moved to
//! 0x20..0x2F and every line is masked: this kernel takes its timer from the
//! Local APIC, so nothing should arrive here, and if a line ever unmasks itself
//! the vector it lands on says plainly which one it was.

use crate::io::{io_wait, outb};

pub(crate) fn remap_and_mask() {
    unsafe {
        outb(0x20, 0x11); // ICW1: init, expect ICW4
        outb(0xA0, 0x11);
        io_wait();
        outb(0x21, 0x20); // ICW2: master vector base
        outb(0xA1, 0x28); // ICW2: slave vector base
        io_wait();
        outb(0x21, 0x04); // ICW3: slave on master IRQ2
        outb(0xA1, 0x02); // ICW3: slave cascade identity
        io_wait();
        outb(0x21, 0x01); // ICW4: 8086 mode
        outb(0xA1, 0x01);
        io_wait();
        outb(0x21, 0xFF); // OCW1: mask every line
        outb(0xA1, 0xFF);
    }
}

//! VGA text mode at 0xB8000.
//!
//! A bootloader-loaded kernel on real hardware / VirtualBox has no serial
//! console on screen; it has the VGA text buffer. Every byte is mirrored to
//! both, so the demo is visible whether the reviewer watches a serial capture
//! (QEMU/CI) or the VM window (VirtualBox). 0xB8000 is inside the first 2 MiB
//! the trampoline identity-maps, so it is reachable in long mode.

use crate::global::Global;

const BUF: *mut u16 = 0xB8000 as *mut u16;
const COLS: usize = 80;
const ROWS: usize = 25;
const ATTR: u16 = 0x0F00; // white on black

/// The cursor. Touched by whatever is printing, which is kernel code on the
/// boot CPU with interrupts on — so an interrupt handler must not print on a
/// path that returns, or it would move a cursor mid-line under the code it
/// interrupted. The handlers that do print are the ones that never return.
static POS: Global<usize> = Global::new(0);

pub(crate) fn clear() {
    for i in 0..COLS * ROWS {
        unsafe { core::ptr::write_volatile(BUF.add(i), ATTR | b' ' as u16) };
    }
    unsafe { core::ptr::write(POS.get(), 0) };
}

pub(crate) fn putb(b: u8) {
    unsafe {
        let mut pos = core::ptr::read(POS.get());
        if b == b'\r' {
            return;
        } else if b == b'\n' {
            pos = (pos / COLS + 1) * COLS;
        } else {
            core::ptr::write_volatile(BUF.add(pos), ATTR | b as u16);
            pos += 1;
        }
        if pos >= COLS * ROWS {
            // scroll up one line
            for i in 0..COLS * (ROWS - 1) {
                let v = core::ptr::read_volatile(BUF.add(i + COLS));
                core::ptr::write_volatile(BUF.add(i), v);
            }
            for i in COLS * (ROWS - 1)..COLS * ROWS {
                core::ptr::write_volatile(BUF.add(i), ATTR | b' ' as u16);
            }
            pos = COLS * (ROWS - 1);
        }
        core::ptr::write(POS.get(), pos);
    }
}

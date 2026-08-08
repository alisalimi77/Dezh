//! The QEMU `virt` SiFive test finisher: the only way to exit the emulator
//! with a status instead of hanging, which is what makes the CI legs assertable.

use core::arch::asm;
use core::ptr::write_volatile;

pub(crate) const TEST_FINISHER: *mut u32 = 0x10_0000 as *mut u32;
pub(crate) const FINISH_PASS: u32 = 0x5555;
pub(crate) const FINISH_FAIL: u32 = 0x3333;

pub(crate) fn shutdown(code: u32) -> ! {
    unsafe { write_volatile(TEST_FINISHER, code) }
    loop {
        unsafe { asm!("wfi") }
    }
}

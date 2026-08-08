//! The NS16550 UART on the QEMU `virt` board — the kernel's only console.
//!
//! Deliberately the first module split out: it depends on nothing, everything
//! depends on it, and `kprint!`/`kprintln!` are the one thing every other
//! module needs before it can say anything at all.

use core::fmt::{self, Write};
use core::ptr::{read_volatile, write_volatile};

pub(crate) const UART_BASE: *mut u8 = 0x1000_0000 as *mut u8;
const UART_RBR: usize = 0;
const UART_THR: usize = 0;
const UART_FCR: usize = 2;
const UART_LSR: usize = 5;

pub(crate) struct Uart;

impl Uart {
    pub(crate) fn init(&self) {
        unsafe { write_volatile(UART_BASE.add(UART_FCR), 0x07) } // enable + clear FIFOs
    }
    pub(crate) fn putc(&self, byte: u8) {
        unsafe { write_volatile(UART_BASE.add(UART_THR), byte) }
    }
    pub(crate) fn getc(&self) -> u8 {
        loop {
            let lsr = unsafe { read_volatile(UART_BASE.add(UART_LSR)) };
            if lsr & 0x01 != 0 {
                return unsafe { read_volatile(UART_BASE.add(UART_RBR)) };
            }
        }
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

// `core::write!` resolves `write_str` by method lookup, so the trait has to be
// in scope wherever the macro is *used* — which made `use core::fmt::Write` a
// silent precondition of calling kprintln! from a new module. Naming the trait
// in the expansion moves that requirement into the macro, where it belongs: a
// module that prints now needs nothing but the macro itself.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::write!($crate::Uart, $($arg)*);
    }};
}
#[macro_export]
macro_rules! kprintln {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = core::writeln!($crate::Uart, $($arg)*);
    }};
}

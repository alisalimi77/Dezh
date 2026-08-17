//! The NS16550 UART on the QEMU `virt` board — the kernel's only console.
//!
//! Deliberately the first module split out: it depends on nothing, everything
//! depends on it, and `kprint!`/`kprintln!` are the one thing every other
//! module needs before it can say anything at all.

use core::arch::asm;
use core::fmt::{self, Write};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering};

pub(crate) const UART_BASE: *mut u8 = 0x1000_0000 as *mut u8;
const UART_RBR: usize = 0;
const UART_THR: usize = 0;
const UART_IER: usize = 1;
const UART_FCR: usize = 2;
const UART_LSR: usize = 5;
/// Received-data-available interrupt enable (16550 IER bit 0).
const IER_RX_AVAIL: u8 = 0x01;
/// LSR bit 0: a byte is waiting in the receive register or FIFO.
const LSR_RX_READY: u8 = 0x01;
/// `sstatus.SIE`, the supervisor interrupt enable.
const SSTATUS_SIE: usize = 1 << 1;

/// The receive ring, and why the console has one.
///
/// `getc` used to spin on `LSR` and read `RBR` directly. That works for a person
/// typing and loses bytes to anything faster: the hardware FIFO is sixteen deep
/// with no flow control, and the console spends most of its time *not* in
/// `getc` — echoing a character, then running a whole command and printing as it
/// goes. Anything arriving in that window had sixteen bytes of slack and then
/// overwrote itself.
///
/// It was not theoretical. Sending a 64-character line repeatedly, two of eight
/// arrived intact; the rest lost characters from the middle, and some lost the
/// newline too, so two commands merged into one nobody typed. A reviewer pasting
/// from the guide gets no error, just a command they did not write.
///
/// So the interrupt handler and `getc` both drain the FIFO into this ring, and
/// the console only ever reads the ring. Capacity is a power of two so the index
/// wrap is an `&`.
const RX_CAP: usize = 256;
const RX_MASK: usize = RX_CAP - 1;

static RX_BUF: [AtomicU8; RX_CAP] = [const { AtomicU8::new(0) }; RX_CAP];
/// Advanced only inside the drain lock, so it has one writer at a time.
static RX_HEAD: AtomicUsize = AtomicUsize::new(0);
/// Advanced only by `rx_pop`, in console context.
static RX_TAIL: AtomicUsize = AtomicUsize::new(0);
/// Bytes the ring dropped because it was full. A different and worse failure
/// than the FIFO overrun this replaces - it means the console fell 256 bytes
/// behind - so `irq-stat` reports it rather than leaving the operator to infer
/// it from a mangled command.
pub(crate) static RX_OVERRUNS: AtomicU32 = AtomicU32::new(0);

pub(crate) struct Uart;

impl Uart {
    pub(crate) fn init(&self) {
        unsafe {
            write_volatile(UART_BASE.add(UART_FCR), 0x07); // enable + clear FIFOs
            write_volatile(UART_BASE.add(UART_IER), IER_RX_AVAIL);
        }
    }
    pub(crate) fn putc(&self, byte: u8) {
        unsafe { write_volatile(UART_BASE.add(UART_THR), byte) }
    }
    /// Every received byte reaches the console through the ring, whether the
    /// interrupt handler put it there or this did.
    ///
    /// Draining here as well as in the handler is not redundancy, it is what
    /// makes the console independent of interrupts: it has to work before
    /// `plic_init` has routed anything, and in any path running with `SIE`
    /// clear. Waiting on the ring alone deadlocks the console in both.
    pub(crate) fn getc(&self) -> u8 {
        loop {
            if let Some(b) = rx_pop() {
                return b;
            }
            rx_drain();
        }
    }
}

/// Serialises drains. A ticket lock rather than a try-lock, and taken with this
/// hart's interrupts masked, because the three obvious cheaper shapes are each
/// wrong in a way that was measured rather than guessed:
///
/// - **`getc` reading `RBR` directly when the ring looks empty** reorders input.
///   An interrupt can queue bytes between the ring check and the read, so the
///   read returns a byte that arrived *after* ones still queued. A reordered
///   command is worse than a truncated one, because it still parses.
/// - **A load-then-store head with two harts** loses bytes: both read the same
///   head, both write the same slot. Identical code was lossless at `-smp 1` and
///   lossy at `-smp 4`, which is what pointed here.
/// - **A try-lock in the handler** livelocks. The 16550 deasserts only when the
///   receiver is empty, so a handler that skips the drain but still completes
///   the claim gets the interrupt straight back; with `getc` holding the lock
///   the boot hart does nothing but re-enter the handler. That measured *worse*
///   than no fix at all.
///
/// Masking is what makes a blocking lock safe here: `getc` can be interrupted on
/// the same hart that holds the lock, and the handler would then spin on it
/// forever.
static RX_LOCK: TicketLock = TicketLock::new();

struct TicketLock {
    next: AtomicU32,
    serving: AtomicU32,
}

impl TicketLock {
    const fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
        }
    }
    fn lock(&self) {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        while self.serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
    }
    fn unlock(&self) {
        self.serving
            .store(self.serving.load(Ordering::Relaxed) + 1, Ordering::Release);
    }
}

/// Clear `sstatus.SIE` and report whether it had been set.
fn irq_off() -> bool {
    let prev: usize;
    unsafe { asm!("csrrci {}, sstatus, 2", out(reg) prev) };
    prev & SSTATUS_SIE != 0
}

fn irq_restore(was_on: bool) {
    if was_on {
        unsafe { asm!("csrsi sstatus, 2") };
    }
}

/// Empty the hardware FIFO into the ring.
///
/// Called from the external-interrupt handler and from `getc`. It must take
/// *everything* available: the 16550 holds its interrupt line asserted while any
/// byte remains, so leaving one behind means the interrupt re-fires at once.
pub(crate) fn rx_drain() {
    let was_on = irq_off();
    RX_LOCK.lock();
    loop {
        let lsr = unsafe { read_volatile(UART_BASE.add(UART_LSR)) };
        if lsr & LSR_RX_READY == 0 {
            break;
        }
        let byte = unsafe { read_volatile(UART_BASE.add(UART_RBR)) };
        let head = RX_HEAD.load(Ordering::Relaxed);
        if head.wrapping_sub(RX_TAIL.load(Ordering::Acquire)) >= RX_CAP {
            // Full. Drop the newest rather than overwrite the oldest: the tail is
            // a line the console is part-way through reading, and corrupting it
            // is the failure this whole thing exists to remove.
            RX_OVERRUNS.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        RX_BUF[head & RX_MASK].store(byte, Ordering::Relaxed);
        RX_HEAD.store(head.wrapping_add(1), Ordering::Release);
    }
    RX_LOCK.unlock();
    irq_restore(was_on);
}

fn rx_pop() -> Option<u8> {
    let tail = RX_TAIL.load(Ordering::Relaxed);
    if RX_HEAD.load(Ordering::Acquire) == tail {
        return None;
    }
    let byte = RX_BUF[tail & RX_MASK].load(Ordering::Relaxed);
    RX_TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some(byte)
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

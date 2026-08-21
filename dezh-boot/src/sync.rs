//! The kernel's mutual-exclusion primitive, and the one rule that makes it safe
//! to use for state an interrupt handler also touches.
//!
//! Dezh grew three of these before it grew one: `smp` has a ticket lock for its
//! parallel rounds and run queues, and `dev::uart` grew its own when the console
//! receive ring needed one. The second copy is the point at which a shared
//! version pays for itself — W13 needs a third, over the scheduler's tables, and
//! that one is where getting it wrong stops being cosmetic.
//!
//! The queue itself now lives in `dezh_core::sync::Ticket`, and what is left
//! here is the half that is actually RISC-V: masking `sstatus.SIE`. That split
//! is not tidiness. Welded together, the lock could only be exercised by booting
//! a machine, and the one thing that could never be reached that way — the u32
//! wraparound — was wrong: `next` was handed out with `fetch_add`, which wraps,
//! while the release side did a plain `+ 1`, which on this kernel's dev profile
//! aborts. Four billion acquisitions in, inside a `Drop`, while holding it.
//!
//! ## Why the lock masks interrupts
//!
//! `plic_handle` runs in interrupt context and writes `TIRQ_WAITING` and
//! `TSTATE` — scheduler state the console also touches. So the moment those
//! tables go under a lock, this becomes reachable:
//!
//! 1. the console takes the lock,
//! 2. a device interrupt lands **on that same hart**,
//! 3. the handler waits for a lock whose holder cannot run until the handler
//!    returns.
//!
//! Nothing about a fair queue helps; the hart is simply stuck. Masking this
//! hart's interrupts for the length of the critical section removes step 2, and
//! is why `lock` returns a guard rather than leaving the caller to remember.
//!
//! This is not theoretical caution. The same shape was hit for real in the
//! console receive path: a try-lock was used there first precisely to dodge it,
//! and it livelocked instead — the handler kept losing the race, never drained
//! the FIFO, and the UART re-raised its line immediately. Masking is what let a
//! blocking lock be correct there, and the same reasoning applies here.
//!
//! ## What it does not do
//!
//! Masking is per-hart. It stops *this* hart from re-entering; other harts still
//! contend, which is what the ticket queue is for. And it is not a substitute
//! for keeping critical sections short: interrupts are held off for the whole
//! hold, so anything slow inside one is a latency bug even when it is correct.

use core::arch::asm;
use dezh_core::sync::Ticket;

/// `sstatus.SIE` — the supervisor global interrupt enable.
const SSTATUS_SIE: usize = 1 << 1;

/// A fair ticket lock: hand out `next`, serve them in order.
///
/// FIFO rather than test-and-set so no hart starves under contention. Acquire on
/// the way in and Release on the way out, so an ordinary read-modify-write
/// inside the critical section is correct.
pub(crate) struct TicketLock {
    queue: Ticket,
}

impl TicketLock {
    pub(crate) const fn new() -> Self {
        Self {
            queue: Ticket::new(),
        }
    }

    /// Mask this hart's interrupts, then take the lock. Both are undone when the
    /// returned guard is dropped.
    ///
    /// The order matters: masking first means the window between "I hold the
    /// lock" and "I cannot be interrupted" does not exist.
    pub(crate) fn lock(&self) -> Guard<'_> {
        let irq_was_on = irq_off();
        self.queue.acquire();
        Guard {
            lock: self,
            irq_was_on,
        }
    }
}

/// Holds the lock. Dropping it releases and restores the interrupt state, in
/// that order — releasing first means the hart is still masked when another hart
/// starts its critical section, which costs nothing and cannot be got wrong.
pub(crate) struct Guard<'a> {
    lock: &'a TicketLock,
    irq_was_on: bool,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.lock.queue.release();
        irq_restore(self.irq_was_on);
    }
}

/// Clear `sstatus.SIE`, reporting whether it had been set.
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

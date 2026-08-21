//! The ticket algebra behind the kernel's mutual-exclusion primitive.
//!
//! It lives here for the same reason the internet checksum does: in the kernel
//! it was welded to the thing that makes it untestable. `TicketLock::lock` masks
//! this hart's interrupts before taking a ticket — necessary, and RISC-V — so
//! the only way to exercise the lock was to boot a machine and watch a counter
//! come out right. That demo (`MUTEX-OK`) is real evidence and it is one case.
//!
//! What is arch-independent is the queue: hand out `next`, serve in order,
//! advance by one on release. That is what is here, and here it can be run
//! against real threads and at the boundary where the counters wrap.
//!
//! Interrupt masking stays in the kernel, wrapping this. That split is also why
//! both kernels can share one queue rather than deriving it twice.

use core::sync::atomic::{AtomicU32, Ordering};

/// A fair ticket queue: hand out `next`, serve them in order.
///
/// FIFO rather than test-and-set so no waiter starves under contention. Acquire
/// on the way in and Release on the way out, so an ordinary read-modify-write
/// inside the critical section is correct.
///
/// This is the algebra only. It does **not** mask interrupts, and on its own it
/// is not safe for state an interrupt handler also touches — that is the
/// caller's half, and the reason the kernel wraps rather than uses it directly.
pub struct Ticket {
    next: AtomicU32,
    serving: AtomicU32,
}

impl Default for Ticket {
    fn default() -> Self {
        Self::new()
    }
}

impl Ticket {
    pub const fn new() -> Self {
        Self {
            next: AtomicU32::new(0),
            serving: AtomicU32::new(0),
        }
    }

    /// Start the queue at a given point.
    ///
    /// This exists so a test can place the lock one release short of the u32
    /// boundary instead of performing four billion acquisitions to get there.
    /// The wraparound is the one part of this algebra that cannot be reached by
    /// exercising it normally, and it is the part that was wrong.
    pub const fn with_counters(next: u32, serving: u32) -> Self {
        Self {
            next: AtomicU32::new(next),
            serving: AtomicU32::new(serving),
        }
    }

    /// Take a ticket and spin until it is being served. Returns the ticket held.
    pub fn acquire(&self) -> u32 {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        while self.serving.load(Ordering::Acquire) != ticket {
            core::hint::spin_loop();
        }
        ticket
    }

    /// Advance the queue by one. Only the holder may call this.
    ///
    /// `wrapping_add`, and that is the fix rather than a detail. `next` is handed
    /// out with `fetch_add`, which is defined to wrap; this side used to be a
    /// plain `+ 1`, which in a debug build is a panic at `u32::MAX` and in a
    /// release build is a silent wrap. One side wrapping and the other not is a
    /// lock that stops being a lock after four billion acquisitions — on the
    /// kernel's own dev profile, where overflow checks are on, by aborting
    /// inside a `Drop` while holding it.
    pub fn release(&self) {
        // Only the holder gets here and each release advances the queue by one,
        // so a store is enough — no read-modify-write needed.
        let served = self.serving.load(Ordering::Relaxed);
        self.serving.store(served.wrapping_add(1), Ordering::Release);
    }

    /// `(next, serving)`, for a caller reporting on the queue.
    pub fn counters(&self) -> (u32, u32) {
        (
            self.next.load(Ordering::Relaxed),
            self.serving.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The crate is `no_std`; tests opt back into std for threads.
    extern crate std;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn an_uncontended_acquire_is_served_immediately() {
        let t = Ticket::new();
        assert_eq!(t.acquire(), 0);
        t.release();
        assert_eq!(t.acquire(), 1);
        t.release();
        assert_eq!(t.counters(), (2, 2));
    }

    #[test]
    fn release_advances_the_queue_by_exactly_one() {
        let t = Ticket::new();
        for expected in 0..64u32 {
            assert_eq!(t.acquire(), expected);
            assert_eq!(t.counters().1, expected, "serving must equal the held ticket");
            t.release();
        }
        assert_eq!(t.counters(), (64, 64));
    }

    /// The regression. `next` is handed out with `fetch_add`, which wraps; the
    /// release side must wrap the same way. It used to be a plain `+ 1`, which
    /// on the kernel's dev profile panics here rather than wrapping — inside a
    /// `Drop`, while holding the lock.
    #[test]
    fn the_queue_survives_the_u32_boundary() {
        let t = Ticket::with_counters(u32::MAX, u32::MAX);
        assert_eq!(t.acquire(), u32::MAX, "the last ticket before the wrap");
        t.release();
        assert_eq!(t.counters(), (0, 0), "both counters wrap together");

        // And the queue still works on the far side of it.
        assert_eq!(t.acquire(), 0);
        t.release();
        assert_eq!(t.counters(), (1, 1));
    }

    /// Crossing the boundary while a waiter already holds a ticket: the waiter's
    /// ticket is `u32::MAX` and the release before it must land on exactly that
    /// value, not one past it.
    #[test]
    fn a_waiter_at_the_boundary_is_still_served() {
        let t = Ticket::with_counters(u32::MAX - 1, u32::MAX - 1);
        assert_eq!(t.acquire(), u32::MAX - 1);
        t.release();
        assert_eq!(t.acquire(), u32::MAX);
        t.release();
        assert_eq!(t.counters(), (0, 0));
    }

    /// A counter that is *not* atomic, mutated only under the lock. Atomics
    /// would prove nothing here: the point is that the lock serialises plain
    /// read-modify-write, which is what the kernel does inside its critical
    /// sections.
    struct Shared {
        value: core::cell::UnsafeCell<u64>,
        lock: Ticket,
    }
    // Safety: `value` is only ever touched between `lock.acquire()` and
    // `lock.release()`, which is the property under test.
    unsafe impl Sync for Shared {}
    unsafe impl Send for Shared {}

    #[test]
    fn the_lock_serialises_a_non_atomic_read_modify_write() {
        const THREADS: u64 = 8;
        const EACH: u64 = 2_000;

        let shared = Arc::new(Shared {
            value: core::cell::UnsafeCell::new(0),
            lock: Ticket::new(),
        });

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    for _ in 0..EACH {
                        shared.lock.acquire();
                        // Safety: held the lock across the whole read-modify-write.
                        unsafe {
                            let p = shared.value.get();
                            *p = (*p).wrapping_add(1);
                        }
                        shared.lock.release();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let total = unsafe { *shared.value.get() };
        assert_eq!(
            total,
            THREADS * EACH,
            "a lost update means the lock did not serialise"
        );
        assert_eq!(shared.lock.counters(), ((THREADS * EACH) as u32, (THREADS * EACH) as u32));
    }

    /// Every ticket is handed out exactly once, even when several threads race
    /// for them. A duplicate would mean two holders at the same time.
    #[test]
    fn every_ticket_is_handed_out_exactly_once() {
        const THREADS: usize = 8;
        const EACH: usize = 500;

        let lock = Arc::new(Ticket::new());
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    let mut mine = Vec::with_capacity(EACH);
                    for _ in 0..EACH {
                        mine.push(lock.acquire());
                        lock.release();
                    }
                    mine
                })
            })
            .collect();

        let mut seen: Vec<u32> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, THREADS * EACH);
        assert_eq!(seen.len(), before, "a ticket was handed out twice");
        assert_eq!(seen[0], 0);
        assert_eq!(seen[before - 1], (THREADS * EACH - 1) as u32);
    }
}

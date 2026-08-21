//! A bounded FIFO ring, and the check it was missing.
//!
//! The kernel has two of these — one for the SMP demo's jobs, one for the AP
//! slots — written twice with the same body, each behind its own ticket lock.
//! The lock is the caller's half and stays there; the ring is arithmetic, so it
//! is here, where it can be run against real interleaving instead of against one
//! demo whose size happens to fit.
//!
//! Which is the point. `push` used to be:
//!
//! ```text
//! q.buf[q.tail % RUNQ_CAP] = slot;
//! q.tail += 1;
//! ```
//!
//! with no test for a full ring. The demo pushes 48 jobs into 64 slots and
//! asserts each one runs exactly once, and it passes — because 48 is less than
//! 64. Push 65 and the 65th silently overwrites an entry nobody has popped yet:
//! one job lost, one run twice, and the property the demo exists to prove is
//! broken without anything saying so.
//!
//! So `push` reports. A refusal is a return value here for the same reason it is
//! everywhere else in Dezh: the alternative is a caller that cannot tell the
//! difference between success and quiet loss.

/// A fixed-capacity FIFO of `u32` handles.
///
/// Not synchronised. Every method takes `&mut self`, so the caller's lock is
/// what makes it safe for several harts — which is deliberate: the kernel's lock
/// masks interrupts, and that decision does not belong in a ring buffer.
pub struct RunQueue<const N: usize> {
    buf: [u32; N],
    head: usize,
    tail: usize,
}

impl<const N: usize> Default for RunQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RunQueue<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            head: 0,
            tail: 0,
        }
    }

    /// Items waiting to be popped.
    pub const fn len(&self) -> usize {
        self.tail - self.head
    }

    pub const fn is_empty(&self) -> bool {
        self.head == self.tail
    }

    pub const fn is_full(&self) -> bool {
        self.len() >= N
    }

    /// Append `value`. Returns `false` — and changes nothing — when the ring is
    /// full.
    ///
    /// The old version had no full check and would overwrite the oldest
    /// unpopped entry. That is the failure this type exists to make visible.
    pub fn push(&mut self, value: u32) -> bool {
        if self.is_full() {
            return false;
        }
        self.buf[self.tail % N] = value;
        self.tail += 1;
        true
    }

    /// Take the oldest item, or `None` when empty.
    pub fn pop(&mut self) -> Option<u32> {
        if self.is_empty() {
            return None;
        }
        let value = self.buf[self.head % N];
        self.head += 1;
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The crate is `no_std`; tests opt back into std for threads.
    extern crate std;
    use crate::sync::Ticket;
    use std::sync::Arc;
    use std::vec::Vec;

    #[test]
    fn it_is_first_in_first_out() {
        let mut q = RunQueue::<8>::new();
        for i in 0..5 {
            assert!(q.push(i));
        }
        for i in 0..5 {
            assert_eq!(q.pop(), Some(i));
        }
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn an_empty_ring_pops_nothing() {
        let mut q = RunQueue::<4>::new();
        assert!(q.is_empty());
        assert_eq!(q.pop(), None);
        assert_eq!(q.len(), 0);
    }

    /// The bug. A full ring must refuse, and must not touch what is already in
    /// it — the old code wrote over the oldest unpopped entry, so the item that
    /// vanished was the one closest to being run.
    #[test]
    fn a_full_ring_refuses_instead_of_overwriting() {
        let mut q = RunQueue::<4>::new();
        for i in 0..4 {
            assert!(q.push(i), "the ring should accept up to its capacity");
        }
        assert!(q.is_full());
        assert!(!q.push(99), "a full ring must refuse");
        assert!(!q.push(100));

        // Nothing was lost and nothing was replaced.
        assert_eq!(q.len(), 4);
        for i in 0..4 {
            assert_eq!(q.pop(), Some(i), "a refused push must not disturb the ring");
        }
        assert_eq!(q.pop(), None);
    }

    /// The ring index wraps but the order does not: pushing and popping past the
    /// capacity keeps every item in sequence.
    #[test]
    fn the_ring_index_wraps_without_reordering() {
        let mut q = RunQueue::<4>::new();
        let mut next_in = 0u32;
        let mut next_out = 0u32;

        // Prime it, then run a long way past the capacity one at a time.
        for _ in 0..3 {
            assert!(q.push(next_in));
            next_in += 1;
        }
        for _ in 0..100 {
            assert!(q.push(next_in));
            next_in += 1;
            assert_eq!(q.pop(), Some(next_out));
            next_out += 1;
        }
        while let Some(v) = q.pop() {
            assert_eq!(v, next_out);
            next_out += 1;
        }
        assert_eq!(next_out, next_in);
    }

    /// Interleaved bursts: fill part-way, drain part-way, repeat. Every item
    /// comes out once, in order, and the ring never claims to hold what it does
    /// not.
    #[test]
    fn interleaved_bursts_keep_every_item_exactly_once() {
        let mut q = RunQueue::<8>::new();
        let mut produced = 0u32;
        let mut consumed = Vec::new();

        for burst in 1..=6u32 {
            for _ in 0..burst {
                if q.push(produced) {
                    produced += 1;
                }
            }
            assert_eq!(q.len(), q.len().min(8));
            for _ in 0..(burst / 2) {
                if let Some(v) = q.pop() {
                    consumed.push(v);
                }
            }
        }
        while let Some(v) = q.pop() {
            consumed.push(v);
        }

        let expected: Vec<u32> = (0..produced).collect();
        assert_eq!(consumed, expected, "items must come out once, in order");
    }

    /// The real interleaving, with the kernel's own lock around it: several
    /// consumers draining one ring while a producer fills it, and every handle
    /// popped exactly once — none lost to a torn dequeue, none returned to two
    /// consumers that both thought they had it.
    ///
    /// This is what `QUEUE-OK` asserts in QEMU, run here where the ring can be
    /// made smaller than the workload and the wraparound actually happens.
    #[test]
    fn many_consumers_drain_one_ring_each_item_exactly_once() {
        const ITEMS: u32 = 4_000;
        const CONSUMERS: usize = 6;

        struct Shared {
            q: core::cell::UnsafeCell<RunQueue<16>>,
            lock: Ticket,
        }
        // Safety: `q` is only touched between acquire and release.
        unsafe impl Sync for Shared {}
        unsafe impl Send for Shared {}

        let shared = Arc::new(Shared {
            q: core::cell::UnsafeCell::new(RunQueue::<16>::new()),
            lock: Ticket::new(),
        });
        let done = Arc::new(core::sync::atomic::AtomicBool::new(false));

        let consumers: Vec<_> = (0..CONSUMERS)
            .map(|_| {
                let shared = Arc::clone(&shared);
                let done = Arc::clone(&done);
                std::thread::spawn(move || {
                    let mut mine = Vec::new();
                    loop {
                        shared.lock.acquire();
                        let got = unsafe { (*shared.q.get()).pop() };
                        shared.lock.release();
                        match got {
                            Some(v) => mine.push(v),
                            None => {
                                if done.load(core::sync::atomic::Ordering::Acquire) {
                                    break;
                                }
                                std::thread::yield_now();
                            }
                        }
                    }
                    mine
                })
            })
            .collect();

        // The producer respects the refusal rather than overwriting: a full ring
        // means wait for a consumer, which is the whole point of reporting it.
        let mut pushed = 0u32;
        while pushed < ITEMS {
            shared.lock.acquire();
            let ok = unsafe { (*shared.q.get()).push(pushed) };
            shared.lock.release();
            if ok {
                pushed += 1;
            } else {
                std::thread::yield_now();
            }
        }
        done.store(true, core::sync::atomic::Ordering::Release);

        let mut seen: Vec<u32> = consumers
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(seen.len(), ITEMS as usize, "an item was lost or double-popped");
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ITEMS as usize, "an item was popped twice");
        assert_eq!(seen[0], 0);
        assert_eq!(seen[ITEMS as usize - 1], ITEMS - 1);
    }
}

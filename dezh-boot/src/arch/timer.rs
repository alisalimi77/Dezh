//! The supervisor timer: a silent uptime tick, and the preemption quantum.
//!
//! `QUANTUM` is the reason a task that never yields cannot monopolise a hart -
//! `redteam`'s fifth escape is stopped here.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64};

pub(crate) const TIMER_DELTA: u64 = 1_000_000;
pub(crate) const TIMER_HZ: u64 = 10;
pub(crate) const QUANTUM: u64 = 50_000; // ~5 ms scheduler time slice for preemption
pub(crate) const STIE: usize = 1 << 5; // supervisor timer interrupt enable (in `sie`)
pub(crate) static TICKS: AtomicU64 = AtomicU64::new(0);
pub(crate) static SKIP_LF_AFTER_CR: AtomicBool = AtomicBool::new(false);

pub(crate) fn rdtime() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

pub(crate) fn sbi_set_timer(stime: u64) {
    unsafe {
        asm!("ecall", in("a0") stime, in("a7") 0usize, lateout("a0") _, lateout("a1") _);
    }
}

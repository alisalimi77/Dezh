//! Three tasks that never yield, and a timer that takes the CPU off them anyway.
//!
//! W16.1 proved an interrupt gives the CPU back to the work it interrupted.
//! This is the opposite claim: the interrupt can decline to, and hand the CPU to
//! someone else instead. Nothing in the worker below cooperates — no yield, no
//! sleep, no syscall — so every time one of them stops running it is because it
//! was stopped.
//!
//! The workers do not print. Printing walks a shared VGA cursor, and three
//! preemptible tasks sharing it would interleave mid-line; only the boot task
//! prints, and only after the scheduler is stopped.

use crate::arch::timer;
use crate::console::{print, print_i64};
use crate::sched;
use core::sync::atomic::{AtomicU64, Ordering};

const WORKERS: usize = 3;
/// How long the workers run for, in ticks.
///
/// A deadline in ticks rather than a target in rounds of work, because a worker
/// cannot advance the tick count itself — only the interrupt handler does. So
/// the run takes the same wall-clock time on a fast machine as on a slow one,
/// and every worker is necessarily stopped and resumed several times whatever
/// the machine or the build profile. A round target instead made the whole
/// demonstration finish inside four switches in the release profile.
const RUN_TICKS: u64 = 30;
/// The fewest turns a worker must be given for this to have demonstrated
/// anything. Round-robin over four tasks for RUN_TICKS ticks gives each about a
/// quarter of them; three is far below that and far above one.
const MIN_TURNS: usize = 3;
const WORK_CHUNK_SUM: u64 = 500_500;
static DEADLINE: AtomicU64 = AtomicU64::new(0);

static ROUNDS: [AtomicU64; WORKERS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static WRONG: [AtomicU64; WORKERS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static DONE: AtomicU64 = AtomicU64::new(0);

/// The same deterministic sum the W16.1 proof used. Under preemption it carries
/// a second meaning: its intermediate state is in registers when the switch
/// happens, so a frame that was saved or restored wrongly shows up as a wrong
/// total rather than as a plausible number.
fn work_chunk() -> u64 {
    let mut sum: u64 = 0;
    let mut i: u64 = 1;
    while i <= 1000 {
        sum = sum.wrapping_add(core::hint::black_box(i));
        i += 1;
    }
    sum
}

fn worker(id: usize) -> ! {
    while timer::ticks() < DEADLINE.load(Ordering::Relaxed) {
        if work_chunk() != WORK_CHUNK_SUM {
            WRONG[id].fetch_add(1, Ordering::Relaxed);
        }
        ROUNDS[id].fetch_add(1, Ordering::Relaxed);
    }
    // Counted before going Idle, so that a tick landing between the two cannot
    // leave the boot task waiting on a total that will never arrive.
    DONE.fetch_add(1, Ordering::Relaxed);
    sched::finish();
    loop {
        // Runs only until the next tick, which will never choose this task again.
        core::hint::spin_loop();
    }
}

extern "C" fn worker1() -> ! {
    worker(0)
}
extern "C" fn worker2() -> ! {
    worker(1)
}
extern "C" fn worker3() -> ! {
    worker(2)
}

pub(crate) fn run() {
    print("\nScheduler: three tasks that never yield, preempted by the timer.\n");
    if !timer::rearm() {
        print("  [sched] no calibrated timer; scheduler not started.\n");
        return;
    }

    sched::spawn(1, worker1);
    sched::spawn(2, worker2);
    sched::spawn(3, worker3);
    print("  [sched] 3 tasks spawned, round-robin with the boot task, 1 tick per turn\n");

    DEADLINE.store(timer::ticks() + RUN_TICKS, Ordering::Relaxed);
    sched::start();
    timer::sti();

    // The boot task is task 0 and is preempted along with the rest; this loop is
    // what it does with its turns.
    while DONE.load(Ordering::Relaxed) < WORKERS as u64 {}

    sched::stop();
    timer::cli();
    timer::mask();

    let mut order = [0u8; sched::TRACE_LEN];
    let n = sched::trace(&mut order);
    print("  [sched] first turns went to task ");
    for &id in &order[..n] {
        print_i64(id as i64);
        print(" ");
    }
    print("\n");

    let mut wrong = 0;
    let mut starved = 0;
    for (i, rounds) in ROUNDS.iter().enumerate() {
        let turns = sched::turns(i + 1);
        if turns < MIN_TURNS {
            starved += 1;
        }
        print("  [sched] task ");
        print_i64(i as i64 + 1);
        print(": ");
        print_i64(turns as i64);
        print(" turns, ");
        print_i64(rounds.load(Ordering::Relaxed) as i64);
        print(" rounds, ");
        let bad = WRONG[i].load(Ordering::Relaxed);
        wrong += bad;
        if bad == 0 {
            print("checksum OK\n");
        } else {
            print("CHECKSUM WRONG\n");
        }
    }

    print("  [sched] ");
    print_i64(sched::switches() as i64);
    print(" context switches over ");
    print_i64(timer::ticks() as i64);
    print(" ticks; the boot task took ");
    print_i64(sched::turns(0) as i64);
    print(" turns of its own\n");

    if wrong == 0 && starved == 0 {
        print("  [sched] preemption works: every task was stopped and resumed, none yielded\n");
    } else {
        print("  [sched] FAILED: a task was starved or its arithmetic was corrupted\n");
    }
}

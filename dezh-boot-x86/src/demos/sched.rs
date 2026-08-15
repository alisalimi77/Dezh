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

use crate::arch::paging;
use crate::arch::timer;
use crate::console::{print, print_hex, print_i64};
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

/// The address every worker's private page is mapped at — the same number in
/// each of them, which is the point: what a task finds there depends on which
/// address space is loaded, not on the address it asked for.
const PRIVATE_VA: u64 = 1 << 39;
/// What each worker expects to find there. Distinct per task, so a task reading
/// its neighbour's page would see a number that is unmistakably not its own.
fn private_magic(id: usize) -> u64 {
    0xDE20_0000_0000 + id as u64
}

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
        // Read through the same address every worker uses. Whatever is there is
        // whatever this task's own `cr3` maps, so a switch that failed to change
        // address spaces would show up here as a neighbour's magic number.
        let seen = unsafe { core::ptr::read_volatile(PRIVATE_VA as *const u64) };
        if seen != private_magic(id) {
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

    // Each worker gets an address space of its own, and in it one page at the
    // same address as everyone else's, holding a different number.
    for id in 1..=WORKERS {
        let cr3 = match paging::new_address_space() {
            Some(cr3) => cr3,
            None => {
                print("  [sched] out of pages; tasks share one address space\n");
                break;
            }
        };
        match paging::map_new_page(cr3, PRIVATE_VA, paging::WRITABLE) {
            None => {
                print("  [sched] out of pages; tasks share one address space\n");
                break;
            }
            Some(page) => unsafe {
                core::ptr::write_volatile(page as *mut u64, private_magic(id - 1))
            },
        }
        sched::set_address_space(id, cr3);
    }
    print("  [sched] each task has its own cr3 and a private page at ");
    print_hex(PRIVATE_VA);
    print(" (");
    print_i64(paging::pages_used() as i64);
    print(" pages used)\n");

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
        print("  [sched] each task read its own page through its own cr3, every round\n");
    } else {
        print("  [sched] FAILED: a task was starved or its arithmetic was corrupted\n");
    }
}

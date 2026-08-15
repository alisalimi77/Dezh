//! Preemptive round-robin over kernel tasks.
//!
//! There is no yield in this file and none in the tasks it runs. A task is taken
//! off the CPU by the timer interrupt and nothing else, which is the point: a
//! task that never cooperates cannot hold the machine.
//!
//! What this is not: there is no user mode, no address space per task, and one
//! CPU. Every task runs at CPL0 on the kernel's own GDT, so a task can reach any
//! memory — isolation on x86 is not part of this step. The RISC-V kernel has
//! that; this does not yet.

use crate::arch::timer;
use crate::global::Global;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const MAX_TASKS: usize = 4;
const STACK_SIZE: usize = 16 * 1024;

/// The 22 qwords `isr_ext_common` saves and restores: 15 general-purpose
/// registers, the (vector, error) pair its stub pushed, and the CPU's five-qword
/// iretq frame. A task's whole context is this and its stack.
const FRAME_QWORDS: usize = 22;
/// A stack that cannot hold a task's initial frame would corrupt whatever sits
/// below it the moment the task is first resumed.
const _: () = assert!(STACK_SIZE > FRAME_QWORDS * 8);

#[derive(Clone, Copy, PartialEq)]
enum State {
    /// Never started, or started and finished. Never chosen.
    Idle,
    /// Wants the CPU.
    Runnable,
}

#[derive(Clone, Copy)]
struct Task {
    /// Where this task's saved frame sits. Meaningless while the task is the one
    /// running, since its context is live in the CPU rather than on its stack.
    frame: u64,
    state: State,
}

/// The task table. Written by `spawn` before the scheduler starts, and after
/// that by `on_tick`, which runs in the timer interrupt with interrupts off on
/// the one CPU this kernel uses. The one other writer is `finish`, which runs
/// in a task and therefore turns interrupts off around its write — otherwise a
/// tick could land in the middle of it and read a half-written table.
static TASKS: Global<[Task; MAX_TASKS]> = Global::new(
    [Task {
        frame: 0,
        state: State::Idle,
    }; MAX_TASKS],
);

/// Task stacks. Task 0 is the boot context and keeps the boot stack, so slot 0
/// is never used; it is kept so that a task's index and its stack's index are
/// the same number.
static STACKS: Global<[[u8; STACK_SIZE]; MAX_TASKS]> = Global::new([[0; STACK_SIZE]; MAX_TASKS]);

static ENABLED: AtomicBool = AtomicBool::new(false);
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static SWITCHES: AtomicUsize = AtomicUsize::new(0);
/// Turns each task was given. This, not elapsed rounds of work, is what says
/// preemption happened: a task cannot give itself a turn, and how many rounds it
/// fits into a turn depends only on how fast the machine is.
static TURNS: [AtomicUsize; MAX_TASKS] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// The first few tasks the scheduler chose, in order. Round-robin is a claim
/// about order, so the order is recorded and printed rather than asserted from
/// the outside.
pub(crate) const TRACE_LEN: usize = 12;
static TRACE: Global<[u8; TRACE_LEN]> = Global::new([0; TRACE_LEN]);
static TRACED: AtomicUsize = AtomicUsize::new(0);

/// Builds a frame that `isr_ext_common` will restore into a task starting at
/// `entry`. The layout is written from the top of the stack downwards, in the
/// order the stub pops it: iretq frame highest, then (error, vector), then the
/// 15 registers.
///
/// Safety: `stack_top` must be the top of an otherwise unused stack of at least
/// `FRAME_QWORDS * 8` bytes.
unsafe fn build_frame(stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
    let top = (stack_top as u64) & !0xF;
    let mut sp = top;
    let mut push = |v: u64| {
        sp -= 8;
        core::ptr::write(sp as *mut u64, v);
    };
    push(0); // ss: null is legal at CPL0 in long mode, and is what boot set
    push(top); // rsp the task runs on once iretq has restored it
    push(0x202); // rflags: interrupts enabled, plus the always-set bit 1
    push(0x08); // cs: the 64-bit code segment from the boot GDT
    push(entry as usize as u64); // rip
    push(0); // the stub's dummy error code
    push(timer::VEC_TIMER as u64); // the stub's vector slot
    // Whatever is left of the frame after the 5-qword iretq part and the stub's
    // 2: the 15 general-purpose registers, all zero for a task that has not run.
    for _ in 0..FRAME_QWORDS - 7 {
        push(0);
    }
    sp
}

/// Makes task `id` runnable, starting at `entry`. Must be called before
/// `start`, with the scheduler off.
pub(crate) fn spawn(id: usize, entry: extern "C" fn() -> !) {
    assert!(id > 0 && id < MAX_TASKS, "task 0 is the boot context");
    unsafe {
        let stacks = STACKS.get() as *mut [u8; STACK_SIZE];
        let stack_top = (stacks.add(id) as *mut u8).add(STACK_SIZE);
        let frame = build_frame(stack_top, entry);
        let t = (TASKS.get() as *mut Task).add(id);
        core::ptr::write(
            t,
            Task {
                frame,
                state: State::Runnable,
            },
        );
    }
}

/// The boot context becomes task 0 and the timer starts preempting.
pub(crate) fn start() {
    unsafe {
        let t = TASKS.get() as *mut Task;
        core::ptr::write(
            t,
            Task {
                frame: 0,
                state: State::Runnable,
            },
        );
    }
    CURRENT.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Release);
}

/// Stops preempting. The caller keeps the CPU from here on.
pub(crate) fn stop() {
    ENABLED.store(false, Ordering::Release);
}

/// Called by a task that has nothing left to do. It goes Idle and is never
/// chosen again; the loop after this call runs only until the next tick takes
/// the CPU away for the last time.
pub(crate) fn finish() {
    timer::cli();
    let id = CURRENT.load(Ordering::Relaxed);
    unsafe {
        let t = (TASKS.get() as *mut Task).add(id);
        (*t).state = State::Idle;
    }
    timer::sti();
}

pub(crate) fn switches() -> usize {
    SWITCHES.load(Ordering::Relaxed)
}

pub(crate) fn turns(id: usize) -> usize {
    TURNS[id].load(Ordering::Relaxed)
}

pub(crate) fn trace(out: &mut [u8; TRACE_LEN]) -> usize {
    let n = TRACED.load(Ordering::Relaxed).min(TRACE_LEN);
    unsafe { core::ptr::copy_nonoverlapping(TRACE.get() as *const u8, out.as_mut_ptr(), n) };
    n
}

/// Called from the timer interrupt with interrupts off, given the interrupted
/// context's `rsp`. Returns the context to resume — a different one whenever
/// another task is runnable.
pub(crate) fn on_tick(frame: u64) -> u64 {
    if !ENABLED.load(Ordering::Acquire) {
        return frame;
    }
    let tasks = TASKS.get() as *mut Task;
    let cur = CURRENT.load(Ordering::Relaxed);
    unsafe { (*tasks.add(cur)).frame = frame };

    // Round-robin: the next runnable task after this one, wrapping. If nothing
    // else wants the CPU this lands back on `cur`, and the frame handed back is
    // the one just saved.
    let mut next = cur;
    for step in 1..=MAX_TASKS {
        let cand = (cur + step) % MAX_TASKS;
        if unsafe { (*tasks.add(cand)).state } == State::Runnable {
            next = cand;
            break;
        }
    }
    if next == cur {
        return frame;
    }

    CURRENT.store(next, Ordering::Relaxed);
    SWITCHES.fetch_add(1, Ordering::Relaxed);
    TURNS[next].fetch_add(1, Ordering::Relaxed);
    let traced = TRACED.fetch_add(1, Ordering::Relaxed);
    if traced < TRACE_LEN {
        unsafe { core::ptr::write((TRACE.get() as *mut u8).add(traced), next as u8) };
    }
    unsafe { (*tasks.add(next)).frame }
}

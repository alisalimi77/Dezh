//! Preemptive round-robin over kernel tasks.
//!
//! There is no yield in this file and none in the tasks it runs. A task is taken
//! off the CPU by the timer interrupt and nothing else, which is the point: a
//! task that never cooperates cannot hold the machine.
//!
//! A task is a saved frame plus a stack, and optionally an address space and a
//! privilege level. Kernel tasks run at CPL0 on the kernel's own stack and can
//! reach anything. User tasks run at CPL3 on a stack of their own, in their own
//! address space, and reach the kernel only through one IDT gate — the CPU
//! switches to the kernel stack named in the TSS on the way in, which is why the
//! switch below sets it.
//!
//! What this is not: one CPU, no priorities, no blocking, and nothing frees a
//! task's stack or pages when it ends.

use crate::arch::gdt;
use crate::arch::paging;
use crate::arch::timer;
use crate::global::Global;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub(crate) const MAX_TASKS: usize = 8;
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
    /// Top of the stack the CPU must switch to when an interrupt arrives while
    /// this task is running at CPL3. Zero for the boot task, which never leaves
    /// ring 0 and therefore keeps whatever stack it is already on.
    kstack_top: u64,
    /// This task's address space. Zero means it has none of its own and runs on
    /// whatever the kernel booted with.
    cr3: u64,
    /// The authority this task holds, in `dezh_core::mcap`'s live task-capability
    /// bits. Zero is the honest default: a task that was never granted anything
    /// can do nothing but exit.
    caps: usize,
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
        kstack_top: 0,
        cr3: 0,
        caps: 0,
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

/// Offsets into a saved frame, in qwords, in the order `isr_ext_common` pops it.
pub(crate) const FRAME_RDI: usize = 8;
pub(crate) const FRAME_RAX: usize = 14;
pub(crate) const FRAME_ERR: usize = 16;
pub(crate) const FRAME_RIP: usize = 17;
pub(crate) const FRAME_CS: usize = 18;

/// Reads one qword out of a saved frame.
///
/// Safety: `frame` must be a frame the interrupt stub saved or `build_frame`
/// wrote, and `slot` must be one of the offsets above.
pub(crate) unsafe fn frame_get(frame: u64, slot: usize) -> u64 {
    core::ptr::read((frame as *const u64).add(slot))
}

/// Writes one qword into a saved frame — how a syscall returns a value: the
/// task resumes with the register restored from what was written here.
///
/// Safety: as `frame_get`.
pub(crate) unsafe fn frame_set(frame: u64, slot: usize, value: u64) {
    core::ptr::write((frame as *mut u64).add(slot), value);
}

/// Builds a frame that `isr_ext_common` will restore into a task starting at
/// `entry`. The layout is written from the top of the stack downwards, in the
/// order the stub pops it: iretq frame highest, then (error, vector), then the
/// 15 registers.
///
/// Safety: `stack_top` must be the top of an otherwise unused stack of at least
/// `FRAME_QWORDS * 8` bytes.
unsafe fn write_frame(
    stack_top: *mut u8,
    rip: u64,
    cs: u16,
    ss: u16,
    task_rsp: u64,
) -> u64 {
    let mut sp = (stack_top as u64) & !0xF;
    let mut push = |v: u64| {
        sp -= 8;
        core::ptr::write(sp as *mut u64, v);
    };
    push(ss as u64);
    push(task_rsp); // the rsp the task runs on once iretq has restored it
    push(0x202); // rflags: interrupts enabled, plus the always-set bit 1
    push(cs as u64);
    push(rip);
    push(0); // the stub's dummy error code
    push(timer::VEC_TIMER as u64); // the stub's vector slot
    // Whatever is left of the frame after the 5-qword iretq part and the stub's
    // 2: the 15 general-purpose registers, all zero for a task that has not run.
    for _ in 0..FRAME_QWORDS - 7 {
        push(0);
    }
    sp
}

/// A kernel task: it runs on the same stack the frame sits on, and `ss` stays
/// null, which is legal at CPL0 in long mode and is what boot left it as.
unsafe fn build_frame(stack_top: *mut u8, entry: extern "C" fn() -> !) -> u64 {
    let top = (stack_top as u64) & !0xF;
    write_frame(stack_top, entry as usize as u64, gdt::KERNEL_CS, 0, top)
}

/// A user task: `cs` and `ss` name the ring-3 descriptors, and the stack it runs
/// on is its own, not the kernel stack the frame is written to. `iretq` reading
/// a `cs` whose RPL is 3 is exactly what drops the CPU to CPL3.
unsafe fn build_user_frame(kstack_top: *mut u8, entry_va: u64, ustack_top: u64) -> u64 {
    write_frame(
        kstack_top,
        entry_va,
        gdt::USER_CS,
        gdt::USER_DS,
        ustack_top & !0xF,
    )
}

/// Grants task `id` exactly `caps`. Must be called after `spawn_user` and before
/// `start`; there is no way for a task to widen this from the inside.
pub(crate) fn set_caps(id: usize, caps: usize) {
    unsafe { (*(TASKS.get() as *mut Task).add(id)).caps = caps };
}

/// The authority of the task that is running — which, inside a syscall, is the
/// caller's. Read from the table rather than from anything the caller passed.
pub(crate) fn current_caps() -> usize {
    unsafe { (*(TASKS.get() as *mut Task).add(CURRENT.load(Ordering::Relaxed))).caps }
}

/// Gives task `id` an address space of its own. Must be called after `spawn`
/// and before `start`.
pub(crate) fn set_address_space(id: usize, cr3: u64) {
    unsafe { (*(TASKS.get() as *mut Task).add(id)).cr3 = cr3 };
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
                kstack_top: stack_top as u64,
                cr3: 0,
                caps: 0,
                state: State::Runnable,
            },
        );
    }
}

/// Makes task `id` runnable at CPL3, entering `entry_va` on `ustack_top`, in the
/// address space `cr3`. The kernel stack stays the one `spawn` would have given
/// it: that is the stack the CPU switches to, out of the TSS, when an interrupt
/// arrives while the task is in ring 3.
pub(crate) fn spawn_user(id: usize, entry_va: u64, ustack_top: u64, cr3: u64) {
    unsafe {
        let stacks = STACKS.get() as *mut [u8; STACK_SIZE];
        let kstack_top = (stacks.add(id) as *mut u8).add(STACK_SIZE);
        let frame = build_user_frame(kstack_top, entry_va, ustack_top);
        let t = (TASKS.get() as *mut Task).add(id);
        core::ptr::write(
            t,
            Task {
                frame,
                kstack_top: kstack_top as u64,
                cr3,
                caps: 0,
                state: State::Runnable,
            },
        );
    }
}

/// A syscall arrived. The frame is the caller's, on its kernel stack; `rax`
/// selects what it asked for and the answer is written back into `rax`.
pub(crate) fn on_syscall(frame: u64) -> u64 {
    let (nr, arg) = unsafe { (frame_get(frame, FRAME_RAX), frame_get(frame, FRAME_RDI)) };
    // The caller's code selector, as the CPU wrote it on entry. Its low two bits
    // are the privilege the call came from, and a task cannot forge them.
    LAST_SYSCALL_CS.store(unsafe { frame_get(frame, FRAME_CS) }, Ordering::Relaxed);
    match SYSCALL_HOOK.load(Ordering::Acquire) {
        0 => unsafe { frame_set(frame, FRAME_RAX, u64::MAX) },
        hook => {
            let hook: fn(u64, u64) -> u64 = unsafe { core::mem::transmute(hook) };
            let ret = hook(nr, arg);
            unsafe { frame_set(frame, FRAME_RAX, ret) };
        }
    }
    // A task that asked to exit is Idle by now and must not be resumed, so the
    // same round-robin that runs on a tick decides who continues.
    if unsafe { (*(TASKS.get() as *mut Task).add(CURRENT.load(Ordering::Relaxed))).state }
        == State::Idle
    {
        return reschedule(frame);
    }
    frame
}

/// Where syscalls go. A raw pointer rather than a trait object because there is
/// no allocator; set once before any user task runs.
static SYSCALL_HOOK: AtomicUsize = AtomicUsize::new(0);
static LAST_SYSCALL_CS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn set_syscall_hook(hook: fn(u64, u64) -> u64) {
    SYSCALL_HOOK.store(hook as usize, Ordering::Release);
}

pub(crate) fn last_syscall_cs() -> u64 {
    LAST_SYSCALL_CS.load(Ordering::Relaxed)
}

/// Ends the running task from inside an interrupt handler, where interrupts are
/// already off — unlike `finish`, which is called by a task and has to turn them
/// off itself.
pub(crate) fn exit_current() {
    let id = CURRENT.load(Ordering::Relaxed);
    unsafe { (*(TASKS.get() as *mut Task).add(id)).state = State::Idle };
}

/// The boot context becomes task 0 and the timer starts preempting.
pub(crate) fn start() {
    unsafe {
        let t = TASKS.get() as *mut Task;
        core::ptr::write(
            t,
            Task {
                frame: 0,
                kstack_top: 0,
                cr3: paging::current_cr3(),
                // The boot task is the kernel; its authority is not modelled by
                // the same bits and it never goes through the syscall gate.
                caps: 0,
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

pub(crate) fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Tasks killed by a fault of their own making, and the last one to go.
static KILLS: AtomicUsize = AtomicUsize::new(0);
static LAST_KILLED: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn kills() -> usize {
    KILLS.load(Ordering::Relaxed)
}

pub(crate) fn last_killed() -> usize {
    LAST_KILLED.load(Ordering::Relaxed)
}

/// Ends the running task because it faulted, and returns whoever runs next.
///
/// If nothing else is runnable there is no next: resuming the frame would take
/// the same fault forever, so the machine stops and says why.
pub(crate) fn kill_current(frame: u64) -> u64 {
    let id = CURRENT.load(Ordering::Relaxed);
    KILLS.fetch_add(1, Ordering::Relaxed);
    LAST_KILLED.store(id, Ordering::Relaxed);
    exit_current();
    let next = reschedule(frame);
    if next == frame {
        crate::console::print("[trap] nothing else to run; halting.
");
        loop {
            unsafe { core::arch::asm!("hlt") };
        }
    }
    next
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
    reschedule(frame)
}

/// Saves `frame` against the running task and returns whichever task should run
/// next. Called from the tick, and from a syscall whose caller has just stopped
/// being runnable.
fn reschedule(frame: u64) -> u64 {
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

    // The address space goes with the task. Doing this from inside the handler
    // is only safe because every address space maps the kernel identically, so
    // the code performing the switch stays mapped across it.
    let cr3 = unsafe { (*tasks.add(next)).cr3 };
    if cr3 != 0 && cr3 != paging::current_cr3() {
        paging::set_cr3(cr3);
    }

    // Point the TSS at the incoming task's own kernel stack before resuming it.
    // It has no effect while every task runs in ring 0, and it is the difference
    // between an isolated task and a broken one the moment one does not.
    let kstack = unsafe { (*tasks.add(next)).kstack_top };
    if kstack != 0 {
        gdt::set_kernel_stack(kstack);
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

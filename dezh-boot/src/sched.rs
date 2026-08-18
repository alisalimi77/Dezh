//! Cooperative multitasking over U-mode tasks.
//!
//! Several U-mode tasks share the CPU by yielding (round-robin). Each has a
//! full register frame (saved and restored by `utrap`), its own 64 KiB stack
//! carved from the top of the user region, and its own capability set. Timer
//! preemption is a future refinement; for now switches happen on yield/exit.
//!
//! This is what was left under the "cooperative multitasking scheduler" banner
//! after steps 11-14 removed the event ledger, the IPC/block ABI, the service
//! registry and the block-daemon client. The move itself is pure relocation:
//! step 15 already converted every table here to `Global<T>`, so this commit
//! changes no semantics at all.
//!
//! Boot hart only for the tables below. `smp` runs tasks on secondary harts
//! through its own path today; W13 is where the two become one scheduler, and
//! where every `Global` here needs a concurrency argument stronger than
//! "only one hart reaches it".

use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::dev::plic::{EXT_IRQS, SCAUSE_EXTERNAL};
use crate::proc::loader::{ProcessSpec, EMPTY_TASK_RESOURCES, TaskKind, TaskResources};
use crate::mm::paging::set_active_task_mem;
use crate::mm::paging::{task_stack_top};
use crate::abi::{
    typed_word, FIRST_FOREGROUND_TASK, IPC_OP_TIMEOUT, IPC_SERVICE_SYSTEM, IPC_STATUS_TIMEOUT,
};
use crate::arch::finisher::{shutdown, FINISH_FAIL};
use crate::arch::timer::{rdtime, sbi_set_timer, QUANTUM, TICKS, TIMER_DELTA};
use crate::mm::global::Global;
use crate::smp::{current_hart, BOOT_HART, MAX_HARTS};
use crate::sync::TicketLock;
use crate::proc::loader::{build_address_space, kernel_satp, proc_satp, USER_STACK_TOP};
use crate::{kprintln, plic_handle, reclaim_resources, restore_kernel_ctx, run_first, trap_entry, utrap, Uart, TASK_IPC, TASK_PRINT, TASK_TIME};
// Every one of these is used as a MATCH PATTERN below. A const that is not in
// scope does not fail to compile there - it silently becomes an irrefutable
// binding that matches everything, collapsing the whole syscall dispatch into
// its first arm. `cargo build` accepted exactly that; only the clippy gate's
// unreachable-pattern lint caught it. They are named explicitly, not globbed,
// so a future move cannot lose one quietly.
use crate::{SYS_DENIED, SYS_EXIT, SYS_IRQ_WAIT, SYS_NULL, SYS_PRINT, SYS_PRINTNUM, SYS_RECV, SYS_RECV_TIMEOUT, SYS_REPORT, SYS_SEND, SYS_UPTIME, SYS_YIELD};

pub(crate) const MAX_TASKS: usize = 4;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TaskState {
    Unused,
    Ready,
    Blocked, // waiting on msg_recv until a message arrives
    Done,
}

static TEXIT: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);

#[derive(Clone, Copy)]
struct IpcStats {
    sends: usize,
    receives: usize,
    denied_sends: usize,
    timeouts: usize,
    queue_full: usize,
    max_depth: usize,
}

static IPC_STATS: Global<IpcStats> = Global::new(IpcStats {
    sends: 0,
    receives: 0,
    denied_sends: 0,
    timeouts: 0,
    queue_full: 0,
    max_depth: 0,
});

// Small FIFO mailbox per task for capability-passing IPC. A message carries a
// small payload plus a *granted* capability set (attenuated to what the sender
// holds). Bounded queues avoid the classic service overwrite bug: two clients
// can enqueue while a service is busy, but unbounded memory growth is still
// impossible.
const MAILBOX_DEPTH: usize = 4;

#[derive(Clone, Copy)]
struct IpcMessage {
    from: usize,
    len: usize,
    grant: usize,
    sender_caps: usize, // kernel-attested caps the sender held at send time
    word: usize, // a register-passed scalar (used by the value-IPC / Cairn demo)
    buf: [u8; 64],
}

const EMPTY_IPC_MESSAGE: IpcMessage = IpcMessage {
    from: 0,
    len: 0,
    grant: 0,
    sender_caps: 0,
    word: 0,
    buf: [0; 64],
};

#[derive(Clone, Copy)]
struct Mailbox {
    head: usize,
    tail: usize,
    count: usize,
    slots: [IpcMessage; MAILBOX_DEPTH],
}

const EMPTY_MAILBOX: Mailbox = Mailbox {
    head: 0,
    tail: 0,
    count: 0,
    slots: [EMPTY_IPC_MESSAGE; MAILBOX_DEPTH],
};

static MBOX: Global<[Mailbox; MAX_TASKS]> = Global::new([EMPTY_MAILBOX; MAX_TASKS]);

static TRECV_WAITING: Global<[bool; MAX_TASKS]> = Global::new([false; MAX_TASKS]);
static TRECV_DEADLINE: Global<[u64; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TRECV_PTR: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TRECV_LEN: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);

static FRAMES: Global<[[usize; FRAME_SLOTS]; MAX_TASKS]> =
    Global::new([[0; FRAME_SLOTS]; MAX_TASKS]);
static TSTATE: Global<[TaskState; MAX_TASKS]> = Global::new([TaskState::Unused; MAX_TASKS]);
/// Tasks parked until a device interrupt arrives (see `SYS_IRQ_WAIT`).
static TIRQ_WAITING: Global<[bool; MAX_TASKS]> = Global::new([false; MAX_TASKS]);
static TCAPS: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TPERS: Global<[u8; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
// each task's address space (satp)
static TSATP: Global<[usize; MAX_TASKS]> = Global::new([0; MAX_TASKS]);
static TRES: Global<[TaskResources; MAX_TASKS]> = Global::new([EMPTY_TASK_RESOURCES; MAX_TASKS]);
/// The task each hart is running.
///
/// Per-hart because "the task that is running" is not one fact about the
/// kernel, it is one fact per hart: `utrap_handler` reads it to know whose
/// syscall it is serving, and `pick_next` reads it as the round-robin cursor.
/// One cell for both, with two harts in the scheduler, means a syscall charged
/// to the wrong task's capabilities - an authority bug, not a lost tick.
///
/// Indexed by `current_hart()`, like `KCTX` and `ktrap_stack`. The index is
/// bounded once, at boot, where `tp` is checked against the id SBI passes.
///
/// `NO_TASK` means this hart is not in the scheduler at all - it is on the
/// console, or it has just left through `restore_kernel_ctx`. That distinction
/// is what turns this table into the run claim as well: an entry that is not
/// `NO_TASK` is a hart asserting it owns that task, and `pick_next` honours it.
/// Keeping the claim and the running task in one cell is deliberate; two tables
/// could disagree, and the disagreement would be two harts on one register
/// frame.
static CURRENT: Global<[usize; MAX_HARTS]> = Global::new([NO_TASK; MAX_HARTS]);

/// No task on this hart. Not a valid index: `MAX_TASKS` is far below it.
const NO_TASK: usize = usize::MAX;

/// The running task on the calling hart. Every read and write of it goes
/// through this pair, so the hart index cannot be dropped at one site.
unsafe fn current_task() -> usize {
    (*CURRENT.get())[current_hart()]
}

unsafe fn set_current_task(i: usize) {
    (*CURRENT.get())[current_hart()] = i;
}

fn clear_mailbox(i: usize) {
    unsafe {
        (*MBOX.get())[i] = EMPTY_MAILBOX;
        (*TRECV_WAITING.get())[i] = false;
        (*TRECV_DEADLINE.get())[i] = 0;
        (*TRECV_PTR.get())[i] = 0;
        (*TRECV_LEN.get())[i] = 0;
    }
}

unsafe fn recv_message_into(task: usize, frame: &mut [usize]) -> bool {
    if (*MBOX.get())[task].count == 0 {
        return false;
    }
    let head = (*MBOX.get())[task].head;
    let msg = (*MBOX.get())[task].slots[head];
    let n = msg.len.min(frame[F_A1]);
    if n > 0 {
        let dst = core::slice::from_raw_parts_mut(frame[F_A0] as *mut u8, n);
        dst.copy_from_slice(&msg.buf[..n]);
    }
    (*TCAPS.get())[task] |= msg.grant;
    (*MBOX.get())[task].slots[head] = EMPTY_IPC_MESSAGE;
    (*MBOX.get())[task].head = (head + 1) % MAILBOX_DEPTH;
    (*MBOX.get())[task].count -= 1;
    frame[F_A0] = n;
    frame[F_A1] = msg.from;
    frame[F_A2] = msg.word;
    // Services check the SENDER's authority (not their own) against this
    // kernel-attested value; a client cannot forge it from user space.
    frame[F_A3] = msg.sender_caps;
    (*IPC_STATS.get()).receives += 1;
    true
}

unsafe fn expire_recv_timeouts() {
    let now = TICKS.load(Ordering::Relaxed);
    let mut i = 0usize;
    while i < MAX_TASKS {
        if (*TRECV_WAITING.get())[i] && (*TSTATE.get())[i] == TaskState::Blocked && (*TRECV_DEADLINE.get())[i] <= now {
            if (*MBOX.get())[i].count > 0 {
                (*TRECV_WAITING.get())[i] = false;
                (*TSTATE.get())[i] = TaskState::Ready;
            } else {
                (*TRECV_WAITING.get())[i] = false;
                (*TRECV_DEADLINE.get())[i] = 0;
                (*TRECV_PTR.get())[i] = 0;
                (*TRECV_LEN.get())[i] = 0;
                (*FRAMES.get())[i][F_SEPC] += 4;
                (*FRAMES.get())[i][F_A0] = IPC_STATUS_TIMEOUT;
                (*FRAMES.get())[i][F_A1] = usize::MAX;
                (*FRAMES.get())[i][F_A2] =
                    typed_word(IPC_SERVICE_SYSTEM, IPC_OP_TIMEOUT, 0, IPC_STATUS_TIMEOUT, 0);
                (*TSTATE.get())[i] = TaskState::Ready;
                (*IPC_STATS.get()).timeouts += 1;
            }
        }
        i += 1;
    }
}

pub(crate) fn task_kind_name(kind: TaskKind) -> &'static str {
    match kind {
        TaskKind::Empty => "-",
        TaskKind::Foreground => "foreground",
        TaskKind::Daemon => "daemon",
        TaskKind::LegacyBakedTask => "legacy",
    }
}

/// Free a task's frames and clear its row. Takes the lock, for callers outside
/// this module.
pub(crate) fn reclaim_task_resources(slot: usize) {
    let _held = SCHED_LOCK.lock();
    reclaim_task_resources_locked(slot);
}

/// The same, for a caller that already holds `SCHED_LOCK`. This split is what
/// lets the run entries below hold the lock across their whole table setup: the
/// ticket lock is not reentrant, so without a `_locked` inner a hart would wait
/// on a lock it already holds, with the release on the far side of the wait.
fn reclaim_task_resources_locked(slot: usize) {
    unsafe {
        if slot >= MAX_TASKS || (*TRES.get())[slot].count == 0 {
            (*TSATP.get())[slot] = 0;
            return;
        }
        reclaim_resources(&mut (*TRES.get())[slot]);
        (*TSATP.get())[slot] = 0;
        (*TCAPS.get())[slot] = 0;
        (*TPERS.get())[slot] = PERS_NATIVE;
        clear_mailbox(slot);
    }
}

pub(crate) fn task_owned_frames(slot: usize) -> usize {
    unsafe {
        if slot < MAX_TASKS {
            (*TRES.get())[slot].count
        } else {
            0
        }
    }
}

pub(crate) fn owned_frames_by_kind(kind: TaskKind) -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TRES.get())[i].kind == kind {
                total += (*TRES.get())[i].count;
            }
            i += 1;
        }
        total
    }
}

pub(crate) fn process_owned_frames() -> usize {
    unsafe {
        let mut total = 0usize;
        let mut i = 0usize;
        while i < MAX_TASKS {
            total += (*TRES.get())[i].count;
            i += 1;
        }
        total
    }
}

fn reclaim_finished_foreground_tasks() {
    let _held = SCHED_LOCK.lock();
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            if (*TSTATE.get())[i] == TaskState::Done {
                reclaim_task_resources_locked(i);
            }
            i += 1;
        }
    }
}

// A task's syscall personality: which ABI its `ecall`s speak.
pub(crate) const PERS_NATIVE: u8 = 0; // Dezh native syscalls (SYS_*)
pub(crate) const PERS_LINUX: u8 = 1; // Linux RISC-V syscall ABI, serviced by the Pol layer

// Frame index of the third arg register a2 = x12 -> 11.
const F_A2: usize = 11;

// Linux (riscv64 generic) syscall numbers we recognize; everything else ENOSYS.
pub(crate) const LINUX_WRITE: usize = 64;
pub(crate) const LINUX_EXIT: usize = 93;
const LINUX_EXIT_GROUP: usize = 94;
// Linux negative errno values, as returned in a0.
const LINUX_EBADF: usize = (-9i64) as usize;
const LINUX_EACCES: usize = (-13i64) as usize;
const LINUX_ENOSYS: usize = (-38i64) as usize;

// Frame index of register xN is N-1; a0=x10 -> 9, a1=x11 -> 10, a7=x17 -> 16,
// sp=x2 -> 1, sepc -> 31.
pub(crate) const F_A0: usize = 9;
pub(crate) const F_A1: usize = 10;
const F_A3: usize = 12;
const F_A4: usize = 13;
pub(crate) const F_A7: usize = 16;
pub(crate) const F_SP: usize = 1;
pub(crate) const F_SEPC: usize = 31;
/// Slot 32: the id of the hart that dispatched this task.
///
/// Not part of the saved register file - the frame is 0..=31 for that, index 31
/// being `sepc`, with no room left. This is written by whichever hart is about
/// to run the task, and read by `utrap` on the way in to put the kernel's own
/// identity back in `tp`.
///
/// It has to travel in the frame because there is nowhere else the trap path can
/// look. A U-mode task owns every integer register, so `tp` holds the task's
/// value by the time it traps and `current_hart()` is meaningless there. Writing
/// it at dispatch is also what makes it survive migration: the answer is set by
/// the hart that will actually run the task, every time it is chosen.
pub(crate) const F_HART: usize = 32;
/// Saved register file (0..=31) plus the dispatching hart id.
pub(crate) const FRAME_SLOTS: usize = 33;

fn frame_ptr(i: usize) -> *mut usize {
    unsafe { core::ptr::addr_of_mut!((*FRAMES.get())[i]) as *mut usize }
}

unsafe fn pick_next() -> Option<usize> {
    expire_recv_timeouts();
    // Resume after the task this hart ran last, or at 0 if it is arriving from
    // the console with nothing to resume after.
    let cur = current_task();
    let start = if cur == NO_TASK { 0 } else { cur + 1 };
    for off in 0..MAX_TASKS {
        let i = (start + off) % MAX_TASKS;
        if (*TSTATE.get())[i] == TaskState::Ready && !claimed_elsewhere(i) {
            return Some(i);
        }
    }
    None
}

/// Is another hart running task `i` right now?
///
/// A task stays `Ready` while it runs - `Ready` means runnable, not idle - so
/// without this two harts would both find the same slot and enter it, and the
/// second would resume from a register frame the first is still saving into.
/// The claim is read here and written only in `schedule_or_return` and the run
/// entries, always under `SCHED_LOCK`, so a claim cannot be granted twice.
///
/// Linear in `MAX_HARTS` per candidate, which is 8. The alternative - a
/// `Running` state - would have to be understood by all eleven places that
/// write `TaskState`, and every one of them would have to get the
/// Ready/Running distinction right for a property none of them are about.
unsafe fn claimed_elsewhere(i: usize) -> bool {
    let me = current_hart();
    (0..MAX_HARTS).any(|h| h != me && (*CURRENT.get())[h] == i)
}

/// Pick the next Ready task and return its frame, or longjmp back to the console
/// if every task is finished.
/// Only tasks parked on a DEVICE may make the kernel idle. A task blocked on
/// IPC is waiting for another task, not for hardware - idling for it would wait
/// for an interrupt that is never coming.
unsafe fn any_irq_waiting() -> bool {
    (0..MAX_TASKS).any(|i| (*TIRQ_WAITING.get())[i] && (*TSTATE.get())[i] == TaskState::Blocked)
}

/// Idle until hardware makes someone runnable. We service the PLIC by hand
/// rather than relying on interrupt delivery: on trap entry the hardware clears
/// `sstatus.SIE`, so a pending interrupt would wake `wfi` but never be taken,
/// and the blocked task would never be woken. Claiming it here closes that hole.
unsafe fn idle_until_device() {
    const IDLE_LIMIT: u64 = 50_000_000;
    let mut spins = 0u64;
    loop {
        // The test reads the task table, so it takes the lock - and gives it
        // back before sleeping. `plic_handle` below takes the same lock through
        // `wake_irq_waiters`, so holding it across the wait would leave this
        // hart waiting for a lock only it can release, with the release on the
        // far side of the wait.
        let idle = {
            let _held = SCHED_LOCK.lock();
            pick_next().is_none() && any_irq_waiting()
        };
        if !idle {
            return;
        }
        asm!("wfi");
        plic_handle();
        spins += 1;
        if spins > IDLE_LIMIT {
            // A device that never reports back must not wedge the machine.
            return;
        }
    }
}

unsafe fn schedule_or_return() -> *const usize {
    // Nothing ready but something blocked means work is pending on a device:
    // wait for it instead of abandoning the run. This is what makes blocking on
    // I/O possible at all - without it the kernel returns from the task loop and
    // orphans the sleeping driver.
    //
    // The lock is taken twice on purpose rather than held across the middle.
    // `idle_until_device` sleeps and services the PLIC, which reaches
    // `wake_irq_waiters` and this same lock; a hart holding it there would be
    // waiting for itself. Re-deciding after the wait is not a cost, it is the
    // point - the wait exists precisely because the answer was expected to
    // change.
    let should_idle = {
        let _held = SCHED_LOCK.lock();
        pick_next().is_none() && any_irq_waiting()
    };
    if should_idle {
        idle_until_device();
    }

    // The address-space switch belongs inside the critical section with the
    // choice that produced it: picking task `i` and then having another hart
    // change `TSATP[i]` before the write would install a page table for a task
    // this hart is no longer about to run.
    let next = {
        let _held = SCHED_LOCK.lock();
        match pick_next() {
            Some(i) => {
                // A task without its own address space can only run on the boot
                // hart, and this is where that gets enforced rather than assumed.
                //
                // `set_active_task_mem` flips `PTE_U` in the SHARED kernel page
                // table so exactly one task's stack is reachable from U-mode.
                // That is one global view: two harts running two such tasks
                // would race, the last writer would win, and the loser's task
                // would fault on its own stack - a corruption, not a crash.
                // Tasks with a private `satp` carry their own mapping and are
                // free of it, which is why `smp` builds one per AP slot.
                if current_hart() != BOOT_HART.load(Ordering::Relaxed)
                    && (*TSATP.get())[i] == kernel_satp()
                {
                    kprintln!(
                        "
[dezh-boot] FATAL: hart {} picked task {i}, which shares the kernel address space -- halting",
                        current_hart()
                    );
                    shutdown(FINISH_FAIL);
                }
                set_current_task(i);
                // Stamp the running hart into the frame before the task resumes,
                // so `utrap` can restore kernel identity on the way back in.
                (*FRAMES.get())[i][F_HART] = current_hart();
                // Only a baked task needs this, and only a baked task can be
                // harmed by it. Calling it for a loaded process wrote `PTE_U`
                // onto baked stack region `i` in the L1 that every process root
                // also points at - exposing 2 MiB of kernel RAM inside that
                // process's address space for as long as it ran. No run mixes
                // the two kinds, so that region held nothing and no task's data
                // was reachable; it was slack, and it is now closed.
                //
                // It is also the last piece of global paging state written on
                // every pick, which is what a second hart in this function would
                // have raced on: one shared L1, last writer wins, and the loser's
                // baked task faults on its own stack.
                if (*TSATP.get())[i] == kernel_satp() {
                    set_active_task_mem(i); // give the new task its stack, hide others
                }
                // Switch to the task's address space (own satp for a loaded
                // process, the shared kernel satp for a baked task).
                asm!("csrw satp, {}", in(reg) (*TSATP.get())[i]);
                asm!("sfence.vma");
                Some(frame_ptr(i) as *const usize)
            }
            None => {
                // Leaving the scheduler: drop the claim here, inside the same
                // section that reads claims, rather than on the far side of the
                // longjmp - which never returns, so there is no far side.
                // A hart that kept its claim would fence off a perfectly
                // runnable task for the rest of the boot.
                set_current_task(NO_TASK);
                None
            }
        }
    };
    match next {
        Some(f) => f,
        // Outside the lock: this longjmps back to the console and never returns,
        // so a guard here would never be dropped.
        None => restore_kernel_ctx(),
    }
}

#[no_mangle]
extern "C" fn utrap_handler(frame_ptr: *mut usize) -> *const usize {
    let scause: usize;
    unsafe { asm!("csrr {}, scause", out(reg) scause) };
    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);
    let frame = unsafe { core::slice::from_raw_parts_mut(frame_ptr, FRAME_SLOTS) };

    unsafe {
        // `utrap` restored the kernel's `tp` from the frame's hart stamp, and
        // everything per-hart below trusts it. Bound it before it indexes
        // anything: `CURRENT` is the next line's subscript, and a `tp` past the
        // end of it would read whatever sits after the array and call it a
        // claim.
        if current_hart() >= MAX_HARTS {
            kprintln!(
                "
[dezh-boot] FATAL: trap with tp={}, beyond MAX_HARTS={MAX_HARTS} -- halting",
                current_hart()
            );
            shutdown(FINISH_FAIL);
        }
        // Snapshot before any reschedule (avoids &static_mut), and under the
        // lock because another hart may be choosing the next task right now.
        let cur = {
            let _held = SCHED_LOCK.lock();
            current_task()
        };
        // The identity check, and it names the invariant rather than a hart.
        //
        // It used to read `current_hart() != BOOT_HART`, which was true only
        // because the boot hart was the only dispatcher; a secondary joining
        // would have had to weaken it. The property that actually has to hold is
        // that this hart holds a claim: a trap from U-mode means it is running a
        // task, so `CURRENT[hart]` must name that task. A restored `tp` pointing
        // at some other hart fails this for free - that hart's claim is either
        // `NO_TASK` or a task this frame is not - and it keeps holding when a
        // second hart starts dispatching, with no set of "allowed" harts to
        // maintain alongside the claim it would duplicate.
        //
        // Also load-bearing on its own: if the claim were dropped while the task
        // was live, another hart would be free to enter this same frame.
        if cur == NO_TASK {
            kprintln!(
                "
[dezh-boot] FATAL: trap on hart {} which holds no task -- halting",
                current_hart()
            );
            shutdown(FINISH_FAIL);
        }
        if interrupt {
            // Supervisor timer = preemption: the running task's full frame is
            // already saved, so round-robin to the next ready task. A task that
            // never yields can no longer monopolize the CPU.
            if code == 5 {
                TICKS.fetch_add(1, Ordering::Relaxed);
                sbi_set_timer(rdtime() + QUANTUM);
                let _ = cur;
                return schedule_or_return();
            }
            // A device finished. Service it and resume the task: the machine no
            // longer has to spin waiting for hardware.
            if code == SCAUSE_EXTERNAL {
                plic_handle();
                return frame_ptr;
            }
            kprintln!("\n[dezh-boot] unexpected interrupt in task (scause={scause:#x}) -- halting");
            shutdown(FINISH_FAIL);
        }

        // A task that touches memory outside its region is killed (thesis at the
        // hardware boundary still holds for scheduled tasks).
        if matches!(code, 12 | 13 | 15) {
            let stval: usize;
            asm!("csrr {}, stval", out(reg) stval);
            kprintln!(
                "  [kernel] task {} DENIED: faulted on {stval:#x} (outside its grant) -- killing",
                cur
            );
            {
                let _held = SCHED_LOCK.lock();
                (*TSTATE.get())[cur] = TaskState::Done;
                (*TEXIT.get())[cur] = SYS_DENIED;
            }
            return schedule_or_return();
        }

        if code == 8 {
            frame[F_SEPC] += 4; // resume after the ecall
            // One short section for the two things every arm needs. Read
            // together so an arm cannot act on this task's capabilities while
            // believing it is a different task.
            let caps = {
                let _held = SCHED_LOCK.lock();
                (*TCAPS.get())[cur]
            };

            // Pol: a Linux-personality task speaks the Linux syscall ABI. We
            // translate each Linux syscall into a capability-checked Dezh action;
            // anything we do not support returns ENOSYS, just like the user-space
            // Linux personality spike (D014).
            let personality = {
                let _held = SCHED_LOCK.lock();
                (*TPERS.get())[cur]
            };
            if personality == PERS_LINUX {
                match frame[F_A7] {
                    LINUX_WRITE => {
                        let fd = frame[F_A0];
                        if fd == 1 || fd == 2 {
                            if caps & TASK_PRINT != 0 {
                                let s = core::slice::from_raw_parts(
                                    frame[F_A1] as *const u8,
                                    frame[F_A2],
                                );
                                for &b in s {
                                    Uart.putc(b);
                                }
                                frame[F_A0] = frame[F_A2]; // bytes written
                            } else {
                                kprintln!(
                                    "  [pol/linux] write(fd={fd}) DENIED: task lacks PRINT capability -> -EACCES"
                                );
                                frame[F_A0] = LINUX_EACCES;
                            }
                        } else {
                            frame[F_A0] = LINUX_EBADF;
                        }
                        return frame_ptr;
                    }
                    LINUX_EXIT | LINUX_EXIT_GROUP => {
                        kprintln!("  [pol/linux] app exit (code {})", frame[F_A0]);
                        {
                            let _held = SCHED_LOCK.lock();
                            (*TSTATE.get())[cur] = TaskState::Done;
                            (*TEXIT.get())[cur] = frame[F_A0];
                        }
                        return schedule_or_return();
                    }
                    other => {
                        kprintln!("  [pol/linux] unsupported syscall {other} -> ENOSYS");
                        frame[F_A0] = LINUX_ENOSYS;
                        return frame_ptr;
                    }
                }
            }

            match frame[F_A7] {
                SYS_YIELD => {
                    {
                        let _held = SCHED_LOCK.lock();
                        (*TSTATE.get())[cur] = TaskState::Ready;
                    }
                    return schedule_or_return();
                }
                SYS_EXIT => {
                    kprintln!("  [kernel] task {} exited (code {})", cur, frame[F_A0]);
                    {
                        // State and exit code together: a supervisor that saw
                        // Done and then read a stale code would report the wrong
                        // reason a service died.
                        let _held = SCHED_LOCK.lock();
                        (*TSTATE.get())[cur] = TaskState::Done;
                        (*TEXIT.get())[cur] = frame[F_A0];
                    }
                    return schedule_or_return();
                }
                SYS_PRINT => {
                    if caps & TASK_PRINT != 0 {
                        let s = core::slice::from_raw_parts(frame[F_A0] as *const u8, frame[F_A1]);
                        for &b in s {
                            Uart.putc(b);
                        }
                        frame[F_A0] = 0;
                    } else {
                        kprintln!("  [kernel] DENIED print: task {cur} holds no PRINT capability");
                        frame[F_A0] = SYS_DENIED;
                    }
                    return frame_ptr;
                }
                SYS_UPTIME => {
                    if caps & TASK_TIME != 0 {
                        frame[F_A0] = TICKS.load(Ordering::Relaxed) as usize;
                    } else {
                        frame[F_A0] = SYS_DENIED;
                    }
                    return frame_ptr;
                }
                SYS_NULL => {
                    // Minimal syscall: the cheapest possible round trip.
                    return frame_ptr;
                }
                SYS_PRINTNUM => {
                    kprintln!("{}", frame[F_A0]);
                    frame[F_A0] = 0;
                    return frame_ptr;
                }
                SYS_REPORT => {
                    let ticks = frame[F_A0];
                    let iters = frame[F_A1];
                    // QEMU `virt` time CSR is 10 MHz => 1 tick = 100 ns.
                    let ns = ticks.saturating_mul(100).checked_div(iters).unwrap_or(0);
                    kprintln!(
                        "  [bench] ecall round-trip: ~{ns} ns/call  ({ticks} ticks / {iters} calls, QEMU-emulated)"
                    );
                    frame[F_A0] = 0;
                    return frame_ptr;
                }
                SYS_SEND => {
                    // msg_send(to=a0, ptr=a1, len=a2, grant_caps=a3)
                    if caps & TASK_IPC == 0 {
                        kprintln!("  [kernel] DENIED send: task {cur} holds no IPC capability");
                        {
                            let _held = SCHED_LOCK.lock();
                            (*IPC_STATS.get()).denied_sends += 1;
                        }
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let to = frame[F_A0];
                    let len = frame[F_A2].min(64);
                    let requested = frame[F_A3];
                    // The whole of a send is one decision, and every step of it
                    // depends on the one before still being true. Between the
                    // liveness check and the enqueue the receiver could exit;
                    // between the depth check and the write another sender could
                    // take the last slot; between the enqueue and the wake the
                    // receiver could park, and then a message sits in a mailbox
                    // with nobody coming for it. So: one section, not five.
                    let _held = SCHED_LOCK.lock();
                    if to >= MAX_TASKS
                        || (*TSTATE.get())[to] == TaskState::Unused
                        || (*TSTATE.get())[to] == TaskState::Done
                    {
                        (*IPC_STATS.get()).denied_sends += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // ATTENUATION: a sender can only delegate capabilities it
                    // itself holds — never widen. (caps = sender's TCAPS.)
                    let granted = requested & caps;
                    if (*MBOX.get())[to].count == MAILBOX_DEPTH {
                        (*IPC_STATS.get()).queue_full += 1;
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    let tail = (*MBOX.get())[to].tail;
                    let msg = &mut (*MBOX.get())[to].slots[tail];
                    if len > 0 {
                        let src = core::slice::from_raw_parts(frame[F_A1] as *const u8, len);
                        msg.buf[..len].copy_from_slice(src);
                    }
                    msg.len = len;
                    msg.from = cur;
                    msg.grant = granted;
                    msg.sender_caps = caps;
                    msg.word = frame[F_A4]; // register-passed scalar (value-IPC)
                    (*MBOX.get())[to].tail = (tail + 1) % MAILBOX_DEPTH;
                    (*MBOX.get())[to].count += 1;
                    (*IPC_STATS.get()).sends += 1;
                    if (*MBOX.get())[to].count > (*IPC_STATS.get()).max_depth {
                        (*IPC_STATS.get()).max_depth = (*MBOX.get())[to].count;
                    }
                    if (*TSTATE.get())[to] == TaskState::Blocked {
                        (*TRECV_WAITING.get())[to] = false;
                        (*TSTATE.get())[to] = TaskState::Ready;
                    }
                    frame[F_A0] = 0;
                    // Every exit from this arm resumes the caller, so the guard
                    // drops here rather than needing to be released before a
                    // reschedule.
                    return frame_ptr;
                }
                SYS_RECV => {
                    // msg_recv(dest=a0, dest_cap=a1) -> bytes received in a0.
                    // Blocks (restartably) until a message is present.
                    if caps & TASK_IPC == 0 {
                        kprintln!("  [kernel] DENIED recv: task {cur} holds no IPC capability");
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // The drain and the park are one decision: a sender that
                    // enqueued between them would find this task already
                    // Blocked with a message waiting and no one to wake it.
                    let got = {
                        let _held = SCHED_LOCK.lock();
                        if recv_message_into(cur, frame) {
                            true
                        } else {
                            // Re-run the ecall when we are scheduled again.
                            frame[F_SEPC] -= 4;
                            (*TSTATE.get())[cur] = TaskState::Blocked;
                            false
                        }
                    };
                    if got {
                        return frame_ptr;
                    }
                    return schedule_or_return();
                }
                SYS_RECV_TIMEOUT => {
                    if caps & TASK_IPC == 0 {
                        kprintln!(
                            "  [kernel] DENIED recv-timeout: task {cur} holds no IPC capability"
                        );
                        frame[F_A0] = SYS_DENIED;
                        return frame_ptr;
                    }
                    // Same decision as `SYS_RECV`, with a deadline: drain, or
                    // arm the timeout and park. Splitting it would let a sender
                    // enqueue into a mailbox this task is about to stop watching.
                    let timeout = frame[F_A2] as u64;
                    let parked = {
                        let _held = SCHED_LOCK.lock();
                        if recv_message_into(cur, frame) {
                            false
                        } else if timeout == 0 {
                            frame[F_A0] = IPC_STATUS_TIMEOUT;
                            frame[F_A1] = usize::MAX;
                            frame[F_A2] = typed_word(
                                IPC_SERVICE_SYSTEM,
                                IPC_OP_TIMEOUT,
                                0,
                                IPC_STATUS_TIMEOUT,
                                0,
                            );
                            (*IPC_STATS.get()).timeouts += 1;
                            false
                        } else {
                            (*TRECV_WAITING.get())[cur] = true;
                            (*TRECV_PTR.get())[cur] = frame[F_A0];
                            (*TRECV_LEN.get())[cur] = frame[F_A1];
                            (*TRECV_DEADLINE.get())[cur] =
                                TICKS.load(Ordering::Relaxed).saturating_add(timeout);
                            frame[F_SEPC] -= 4;
                            (*TSTATE.get())[cur] = TaskState::Blocked;
                            true
                        }
                    };
                    if !parked {
                        return frame_ptr;
                    }
                    return schedule_or_return();
                }
                SYS_IRQ_WAIT => {
                    // Restartable: park the task and rewind past the ecall, so on
                    // wake it re-runs, sees the advanced count, and returns.
                    //
                    // The comparison MUST be inside the same section as the park.
                    // This is the lost-wakeup shape: read the count, decide to
                    // sleep, and have `wake_irq_waiters` run in the gap - it
                    // finds nothing parked, the task then parks, and nothing
                    // will ever wake it. One hart cannot hit it because the trap
                    // runs with interrupts masked; a second hart can.
                    let prev = frame[F_A0];
                    let parked = {
                        let _held = SCHED_LOCK.lock();
                        let now = EXT_IRQS.load(Ordering::Relaxed) as usize;
                        if now != prev {
                            frame[F_A0] = now;
                            false
                        } else {
                            (*TIRQ_WAITING.get())[cur] = true;
                            frame[F_SEPC] -= 4;
                            (*TSTATE.get())[cur] = TaskState::Blocked;
                            true
                        }
                    };
                    if !parked {
                        return frame_ptr;
                    }
                    return schedule_or_return();
                }
                _ => {
                    frame[F_A0] = SYS_DENIED;
                    return frame_ptr;
                }
            }
        }

        kprintln!("\n[dezh-boot] unexpected trap in task (scause={scause:#x}) -- halting");
        shutdown(FINISH_FAIL);
    }
}

/// Set up `specs` as Ready tasks and run them round-robin until all finish.
/// Each spec is (entry, caps). Returns when every task is Done.
pub(crate) fn run_tasks(specs: &[(usize, usize, u8)]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        // Index form is still deliberate, for a different reason than before:
        // the tables are `Global<T>` now, and the iterator rewrite would need a
        // reference into `(*TSTATE.get())` - exactly the aliasing `Global` is
        // here to prevent. Indexing through the raw pointer never makes one.
        // One section for the whole setup and the first claim. Everything in it
        // is table work on baked tasks - no ELF load, no page-table build - so
        // it is short, and it has to be atomic: a hart that saw this table
        // half-built would pick a task whose frame is not written yet.
        let _held = SCHED_LOCK.lock();
        #[allow(clippy::needless_range_loop)]
        for i in 0..MAX_TASKS {
            reclaim_task_resources_locked(i);
            (*TSTATE.get())[i] = TaskState::Unused;
            clear_mailbox(i);
        }
        for (i, &(entry, caps, pers)) in specs.iter().take(n).enumerate() {
            let f = &mut (*FRAMES.get())[i];
            *f = [0; FRAME_SLOTS];
            f[F_SEPC] = entry;
            f[F_SP] = task_stack_top(i); // each task owns a private 2 MiB stack region
            (*TCAPS.get())[i] = caps;
            (*TPERS.get())[i] = pers;
            (*TSATP.get())[i] = kernel_satp(); // baked tasks share the kernel address space
            (*TRES.get())[i] = EMPTY_TASK_RESOURCES;
            (*TRES.get())[i].kind = TaskKind::LegacyBakedTask;
            (*TSTATE.get())[i] = TaskState::Ready;
        }
        set_current_task(0);
        // First dispatch of the run does not go through `schedule_or_return`,
        // so it stamps the hart itself.
        (*FRAMES.get())[0][F_HART] = current_hart();
        set_active_task_mem(0); // expose only task 0's stack region to start
        // The claim is taken; drop the lock before entering U-mode, because
        // `run_first` does not return here - it longjmps back through
        // `restore_kernel_ctx`, so a guard still alive at this point would never
        // be dropped and the lock would be held for the rest of the boot.
        drop(_held);
        // Switch to the multitasking trap path and arm the preemption timer.
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        run_first(frame_ptr(0) as *const usize);
        // Returned via restore_kernel_ctx once every task is Done.
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA); // restore the console uptime cadence
    }
}

/// Load and run several separate programs as real processes: each gets its own
/// ELF, its own address space (satp), an id in a0, and a capability set. They
/// run concurrently under the preemptive scheduler, isolated from one another,
/// and return to the console once all have exited. Zero ambient authority — a
/// process holds only the capabilities passed here (no fork).
pub(crate) fn run_processes(specs: &[ProcessSpec]) {
    let n = specs.len().min(MAX_TASKS);
    unsafe {
        // A loaded process must not see any baked-task stack region.
        set_active_task_mem(usize::MAX);
        {
            // Index form is still deliberate, for a different reason than before:
            // the tables are `Global<T>` now, and the iterator rewrite would need a
            // reference into `(*TSTATE.get())` - exactly the aliasing `Global` is
            // here to prevent. Indexing through the raw pointer never makes one.
            let _held = SCHED_LOCK.lock();
            #[allow(clippy::needless_range_loop)]
            for i in 0..MAX_TASKS {
                reclaim_task_resources_locked(i);
                (*TSTATE.get())[i] = TaskState::Unused;
                clear_mailbox(i);
            }
        }
        let mut launched = 0usize;
        let mut first_ready = usize::MAX;
        for (i, spec) in specs.iter().take(n).enumerate() {
            // The load stays outside the lock - see `spawn_process_at`. One
            // section per task after it, which is the same unit step 3b chose
            // for syscalls: a whole task's row appears at once or not at all.
            let Some(build) = build_address_space(spec, TaskKind::Foreground) else {
                kprintln!("  [kernel] process launch failed: out of frames");
                continue;
            };
            let _held = SCHED_LOCK.lock();
            let f = &mut (*FRAMES.get())[i];
            *f = [0; FRAME_SLOTS];
            f[F_SEPC] = build.entry;
            f[F_SP] = USER_STACK_TOP; // each process has its own stack in its own space
            f[F_A0] = spec.arg0;
            f[F_A1] = spec.arg1;
            f[F_A2] = spec.arg2;
            f[F_A3] = spec.arg3;
            (*TCAPS.get())[i] = spec.caps;
            (*TPERS.get())[i] = spec.personality;
            (*TSATP.get())[i] = proc_satp(build.root);
            (*TRES.get())[i] = build.resources;
            (*TSTATE.get())[i] = TaskState::Ready;
            if first_ready == usize::MAX {
                first_ready = i;
            }
            launched += 1;
        }
        if launched == 0 {
            return;
        }
        // The claim and the address space it names, together - the same pairing
        // `schedule_or_return` makes, and for the same reason: a satp installed
        // for a task this hart no longer owns is a task running in someone
        // else's memory. Released before `run_first`, which never returns here.
        let entry_satp = {
            let _held = SCHED_LOCK.lock();
            set_current_task(first_ready);
            (*FRAMES.get())[first_ready][F_HART] = current_hart();
            (*TSATP.get())[first_ready]
        };
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) entry_satp); // enter the first process's address space
        asm!("sfence.vma");
        run_first(frame_ptr(first_ready) as *const usize);
        // Back in the kernel address space once every process has exited.
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        let _held = SCHED_LOCK.lock();
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TSTATE.get())[i] == TaskState::Done {
                reclaim_task_resources_locked(i);
            }
            i += 1;
        }
    }
}

pub(crate) fn run_scheduler_from(first: usize) {
    unsafe {
        let entry_satp = {
            let _held = SCHED_LOCK.lock();
            set_current_task(first);
            // The third dispatch entry, and the one that was missed: this is the
            // path a Pol process takes. Every `run_first` call site has to stamp,
            // because none of them go through `schedule_or_return`.
            (*FRAMES.get())[first][F_HART] = current_hart();
            (*TSATP.get())[first]
        };
        asm!("csrw stvec, {}", in(reg) utrap as *const () as usize);
        sbi_set_timer(rdtime() + QUANTUM);
        asm!("csrw satp, {}", in(reg) entry_satp);
        asm!("sfence.vma");
        run_first(frame_ptr(first) as *const usize);
        asm!("csrw satp, {}", in(reg) kernel_satp());
        asm!("sfence.vma");
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
    }
}

pub(crate) fn spawn_process_at(slot: usize, spec: &ProcessSpec, kind: TaskKind) -> bool {
    // Outside the lock on purpose: `build_address_space` loads an ELF and walks
    // page tables, and the lock masks this hart's interrupts. A section that
    // long would hold off every device interrupt on the hart for the length of a
    // program load. It touches the frame allocator, not the task table, so the
    // table stays consistent without it.
    reclaim_task_resources(slot);
    let build = build_address_space(spec, kind);
    let _held = SCHED_LOCK.lock();
    unsafe {
        let Some(build) = build else {
            kprintln!("  [kernel] process launch failed: out of frames");
            (*TSTATE.get())[slot] = TaskState::Unused;
            clear_mailbox(slot);
            return false;
        };
        let f = &mut (*FRAMES.get())[slot];
        *f = [0; FRAME_SLOTS];
        f[F_SEPC] = build.entry;
        f[F_SP] = USER_STACK_TOP;
        f[F_A0] = spec.arg0;
        f[F_A1] = spec.arg1;
        f[F_A2] = spec.arg2;
        f[F_A3] = spec.arg3;
        (*TCAPS.get())[slot] = spec.caps;
        (*TPERS.get())[slot] = spec.personality;
        (*TSATP.get())[slot] = proc_satp(build.root);
        (*TRES.get())[slot] = build.resources;
        (*TEXIT.get())[slot] = 0;
        clear_mailbox(slot);
        (*TSTATE.get())[slot] = TaskState::Ready;
        true
    }
}

fn clear_foreground_tasks() {
    let _held = SCHED_LOCK.lock();
    unsafe {
        let mut i = FIRST_FOREGROUND_TASK;
        while i < MAX_TASKS {
            reclaim_task_resources_locked(i);
            (*TSTATE.get())[i] = TaskState::Unused;
            clear_mailbox(i);
            (*TCAPS.get())[i] = 0;
            (*TEXIT.get())[i] = 0;
            i += 1;
        }
    }
}

pub(crate) fn run_foreground_processes(specs: &[ProcessSpec]) {
    let n = specs.len().min(MAX_TASKS - FIRST_FOREGROUND_TASK);
    set_active_task_mem(usize::MAX);
    clear_foreground_tasks();
    let mut launched = 0usize;
    let mut first_ready = usize::MAX;
    for (i, spec) in specs.iter().take(n).enumerate() {
        let slot = FIRST_FOREGROUND_TASK + i;
        if spawn_process_at(slot, spec, TaskKind::Foreground) {
            if first_ready == usize::MAX {
                first_ready = slot;
            }
            launched += 1;
        }
    }
    if launched == 0 {
        return;
    }
    run_scheduler_from(first_ready);
    reclaim_finished_foreground_tasks();
}

// --- The task table's public surface. ----------------------------------------
//
// Six modules used to reach into `TSTATE`, `TEXIT`, `TCAPS`, `TRES` and
// `TIRQ_WAITING` directly, twenty-five sites in all. That was harmless while one
// hart reached them and is the blocker for W13 step 2: a lock cannot be put
// around state that half the kernel pokes at from outside.
//
// So the tables are private now and this is the whole of what the rest of the
// kernel may ask. The point is not tidiness - it is that the compiler, not a
// convention, decides whether a new call site can skip the lock these are about
// to acquire. Adding the lock becomes a change inside this file.
//
// `wake_irq_waiters` is the one that mattered most: it moves a *mutation* that
// ran in interrupt context, in `plic_handle`, back into the module that owns the
// state it mutates.

/// A task's row, for reporting. Read as one call so a caller cannot print a
/// half-updated mixture of two tasks' fields.
#[derive(Clone, Copy)]
pub(crate) struct TaskRow {
    pub(crate) state: TaskState,
    pub(crate) kind: TaskKind,
    pub(crate) caps: usize,
    pub(crate) exit: usize,
}

/// Guards the task table.
///
/// **What it covers now:** every write to the table, and every read that has to
/// be consistent with one. That includes the scheduler's own internals —
/// `utrap_handler` and `schedule_or_return` in scopes rather than across their
/// whole bodies, and the four run entries across their table setup and their
/// first claim. `wake_irq_waiters` is in it too, and is the only write from
/// interrupt context, which is why the lock masks.
///
/// Step 2 drew the boundary at the public surface and said why the internals
/// were left out: one loop,
///
/// ```text
/// schedule_or_return -> idle_until_device -> plic_handle -> wake_irq_waiters
/// ```
///
/// A ticket lock is not reentrant, so a guard held across that path would have
/// the hart wait on a lock only it can release, with the release inside the
/// wait. Step 3a answered it for the scheduler entry by taking the lock in
/// scopes around the sleep instead of across it. The run entries needed the
/// other half of the restructuring step 2 named: a `_locked` inner for the one
/// function called both from outside and from within,
/// `reclaim_task_resources`.
///
/// **What is deliberately outside it.** `build_address_space` — it loads an ELF
/// and walks page tables, and the lock masks this hart's interrupts, so a
/// section that long would hold off every device interrupt for the length of a
/// program load. It touches the frame allocator, not the table.
///
/// The failure mode of getting this wrong is a hang, not a crash, so guard
/// placement is checked by `tools/ci/check_sched_lock.py` rather than by
/// reading: it walks brace depth and fails the build if any call inside a guard
/// reaches a function that takes the lock again.
static SCHED_LOCK: TicketLock = TicketLock::new();

// Raw table reads. Callers already hold `SCHED_LOCK`; these exist so the public
// wrappers do not take it twice.
fn state_of(slot: usize) -> TaskState {
    unsafe { (*TSTATE.get())[slot] }
}

fn exit_of(slot: usize) -> usize {
    unsafe { (*TEXIT.get())[slot] }
}

pub(crate) fn task_row(slot: usize) -> TaskRow {
    let _held = SCHED_LOCK.lock();
    unsafe {
        TaskRow {
            state: state_of(slot),
            kind: (*TRES.get())[slot].kind,
            caps: (*TCAPS.get())[slot],
            exit: exit_of(slot),
        }
    }
}

pub(crate) fn task_state(slot: usize) -> TaskState {
    let _held = SCHED_LOCK.lock();
    state_of(slot)
}

pub(crate) fn task_exit_code(slot: usize) -> usize {
    let _held = SCHED_LOCK.lock();
    exit_of(slot)
}

/// Is this task alive in the sense a supervisor cares about - runnable, or
/// parked waiting for something?
pub(crate) fn task_is_live(slot: usize) -> bool {
    let _held = SCHED_LOCK.lock();
    matches!(state_of(slot), TaskState::Blocked | TaskState::Ready)
}

/// The exit code of the foreground task, which is how every synchronous console
/// verb - block I/O, Marz, the package paths - reads its result back.
pub(crate) fn foreground_exit_code() -> usize {
    let _held = SCHED_LOCK.lock();
    exit_of(FIRST_FOREGROUND_TASK)
}

/// A device reported completion: make every task parked on one runnable again.
/// Returns how many were woken.
///
/// Called from `plic_handle`, in interrupt context, which is why the lock has to
/// mask: without that, the console could hold it, take a device interrupt on the
/// same hart, and wait here forever for itself.
pub(crate) fn wake_irq_waiters() -> u64 {
    let _held = SCHED_LOCK.lock();
    let mut woken = 0u64;
    unsafe {
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TIRQ_WAITING.get())[i] {
                (*TIRQ_WAITING.get())[i] = false;
                if (*TSTATE.get())[i] == TaskState::Blocked {
                    (*TSTATE.get())[i] = TaskState::Ready;
                }
                woken += 1;
            }
            i += 1;
        }
    }
    woken
}

pub(crate) fn print_ipcstat() {
    unsafe {
        let stats = *IPC_STATS.get();
        kprintln!(
            "ipcstat: sends={} receives={} denied_sends={} timeouts={} queue_full={} max_depth={}",
            stats.sends,
            stats.receives,
            stats.denied_sends,
            stats.timeouts,
            stats.queue_full,
            stats.max_depth
        );
    }
}

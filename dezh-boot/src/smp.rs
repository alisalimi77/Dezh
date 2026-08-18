//! Symmetric multiprocessing: real secondary harts, a shared run queue, and
//! U-mode tasks running on more than one core at once.
//!
//! Brings APs up through the SBI HSM protocol, gives each its own trap stack
//! and kernel context, and hands them work through a ticket-locked run queue.
//! `ap_schedule` runs U-mode tasks on secondary harts through a slot table
//! with its own address spaces.
//!
//! The per-hart trap path here is separate from `sched`'s. That duplication is
//! the whole subject of W13, which merges the two into one scheduler; keeping
//! them in two modules makes the seam that has to close visible instead of
//! implied.

use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::mm::frames::{frame_alloc, frame_free};
use crate::mm::paging::{stack_region_l1_index, task_stack_top, L1, PTE_U, PTE_V, ROOT};
use crate::sched::{F_A0, F_A1, F_A7, F_SEPC, F_SP, MAX_TASKS};
// Match patterns, not values - see the same note in `sched`. A const that is
// not in scope becomes an irrefutable binding here and silently eats every
// syscall. This is the second time the split hit it, and the second time only
// the clippy gate noticed; `cargo fix` proposed renaming SYS_EXIT to
// `_sys_exit`, which would have made the bug permanent and quiet.
use crate::{SYS_EXIT, SYS_PRINT};
use crate::{
    _hart_start, kprintln, sys_exit, sys_print, Uart, SYS_DENIED,
};
use crate::arch::timer::{rdtime, sbi_set_timer, QUANTUM, STIE};
use crate::sync::TicketLock;

/// `scause` code for a supervisor timer interrupt, with the interrupt bit
/// already stripped by the caller.
const SCAUSE_TIMER: usize = 5;

/// How long an idle secondary hart sleeps before re-checking for work.
///
/// Deliberately shorter than `QUANTUM`: this is a wake-up latency the SMP
/// rounds pay, not a scheduling slice. At the 10 MHz `rdtime` of the `virt`
/// board it is about a millisecond, which is invisible next to the bounded spin
/// the boot hart already allows a round, and far cheaper than the alternative -
/// a hart spinning here costs the console real throughput on an emulated host.
const IDLE_TICK: u64 = 10_000;

//
// Honest scope. The boot hart runs the OS: the scheduler and every kernel data
// structure are single-threaded `static mut`, so a symmetric scheduler over them
// would need a lock on each, and that is a named future milestone (see ROADMAP),
// not this step. What this step proves is the foundation such a scheduler stands
// on: that Dezh can actually *drive hardware parallelism* — start the other harts
// through the standard SBI Hart State Management call, give each its own stack and
// identity (`tp` = hart id), and show more than one hart executing concurrently
// under our control with coherent atomics. QEMU parks secondary harts in the SBI
// firmware until `sbi_hart_start`; only the boot hart ever enters `_start`, so the
// boot path has no concurrent-entry race to guard.
pub(crate) const MAX_HARTS: usize = 8;
const HART_STACK: usize = 8 * 1024;
// Atomic increments each secondary hammers onto ONE shared counter per round. The
// contention is the point: a coherent total proves the harts share memory and the
// hardware serialises their atomics, not that they ran one-after-another.
pub(crate) const SMP_WORK: u64 = 200_000;
// Bounded spin so a hart that never checks in cannot hang the boot or the console.
const SMP_SPIN_LIMIT: u64 = 300_000_000;

// Per-hart stacks and the secondary entry point live in the asm block so a hart
// has a stack before any Rust runs. `sbi_hart_start` enters here with a0 = hart id.
global_asm!(
    r#"
    .section .bss
    .align 16
    .globl hart_stacks
hart_stacks:
    .space {TOTAL}

    .section .text
    .align 4
    .globl _hart_start
_hart_start:
    mv      tp, a0              # per-hart identity: tp = hart id
    la      t0, hart_stacks
    li      t1, {STK}
    addi    t2, a0, 1
    mul     t2, t2, t1          # (hart id + 1) * HART_STACK = top of this hart's slot
    add     sp, t0, t2
    call    hart_main           # a0 still = hart id
1:  wfi
    j       1b
"#,
    STK = const HART_STACK,
    TOTAL = const HART_STACK * MAX_HARTS,
);

// --- Per-hart U-mode trap path (for running a task on a secondary hart). ------
//
// This is deliberately SEPARATE from the boot hart's `utrap`/`KCTX`/`ktrap_stack`
// so that dispatching tasks onto secondary harts cannot perturb the single-hart
// console scheduler that everything else depends on.
//
// Finding per-hart state without `tp`. A U-mode task owns every integer register,
// so by the time it traps, `tp` holds whatever the task left there — the trap path
// must not use it. Instead each hart's state lives in one `ApCtx` whose FIRST field
// is the trap frame, and `sscratch` points at it while the task runs. On trap,
// `csrrw sp, sscratch, sp` lands sp on that hart's own `ApCtx`, from which the
// per-hart trap stack (offset 256) and saved kernel context (offset 264) are read.
// That makes several harts able to be in a trap at the same time.
//   ap_run(frame, kctx): save callee-saved into kctx, then sret into the task.
//   ap_return(kctx):     longjmp back to just after ap_run (used on task exit).
//   utrap_ap:            U-mode trap entry; per-hart state via sscratch, not tp.
const AP_TRAP_STK: usize = 8192;
/// Byte offsets inside `ApCtx`, mirrored in the assembly below.
const AP_OFF_TRAPTOP: usize = 256;
// Read by the assembly below through its own literal, not by Rust, but it
// documents half of the ApCtx layout: dropping it would leave the trap path's
// second offset recorded nowhere a reader of this file would look.
#[allow(dead_code)]
const AP_OFF_KCTX: usize = 264;
global_asm!(
    r#"
    .section .bss
    .align 16
    .globl ap_trap_stacks
ap_trap_stacks:
    .space {AP_TOTAL}

    .section .text
    .align 4
    .globl utrap_ap
utrap_ap:
    csrrw   sp, sscratch, sp        # sp = &frame, sscratch = user sp
    sd      x1, 0(sp)
    sd      x3, 16(sp)
    sd      x4, 24(sp)
    sd      x5, 32(sp)
    csrr    x5, sscratch            # x5 = user sp (x5 already saved)
    sd      x5, 8(sp)
    sd      x6, 40(sp)
    sd      x7, 48(sp)
    sd      x8, 56(sp)
    sd      x9, 64(sp)
    sd      x10, 72(sp)
    sd      x11, 80(sp)
    sd      x12, 88(sp)
    sd      x13, 96(sp)
    sd      x14, 104(sp)
    sd      x15, 112(sp)
    sd      x16, 120(sp)
    sd      x17, 128(sp)
    sd      x18, 136(sp)
    sd      x19, 144(sp)
    sd      x20, 152(sp)
    sd      x21, 160(sp)
    sd      x22, 168(sp)
    sd      x23, 176(sp)
    sd      x24, 184(sp)
    sd      x25, 192(sp)
    sd      x26, 200(sp)
    sd      x27, 208(sp)
    sd      x28, 216(sp)
    sd      x29, 224(sp)
    sd      x30, 232(sp)
    sd      x31, 240(sp)
    csrr    x5, sepc
    sd      x5, 248(sp)
    mv      a0, sp                  # a0 = &frame == &ApCtx for THIS hart
    ld      sp, {OFF_TRAPTOP}(a0)   # this hart's own trap stack (found via sscratch)
    call    ap_trap_handler         # returns &frame to resume in a0
    j       ap_frame_restore

    .globl ap_run
ap_run:                             # a0 = &frame, a1 = &kctx
    sd      ra, 0(a1)
    sd      sp, 8(a1)
    sd      s0, 16(a1)
    sd      s1, 24(a1)
    sd      s2, 32(a1)
    sd      s3, 40(a1)
    sd      s4, 48(a1)
    sd      s5, 56(a1)
    sd      s6, 64(a1)
    sd      s7, 72(a1)
    sd      s8, 80(a1)
    sd      s9, 88(a1)
    sd      s10, 96(a1)
    sd      s11, 104(a1)
    # fall through into the restore with a0 = frame

ap_frame_restore:                   # a0 = &frame to resume
    mv      t0, a0
    ld      t1, 248(t0)
    csrw    sepc, t1
    csrw    sscratch, t0            # sscratch = &frame for the next trap
    ld      sp, 8(t0)               # user sp
    ld      x1, 0(t0)
    ld      x3, 16(t0)
    ld      x4, 24(t0)
    ld      x6, 40(t0)
    ld      x7, 48(t0)
    ld      x8, 56(t0)
    ld      x9, 64(t0)
    ld      x11, 80(t0)
    ld      x12, 88(t0)
    ld      x13, 96(t0)
    ld      x14, 104(t0)
    ld      x15, 112(t0)
    ld      x16, 120(t0)
    ld      x17, 128(t0)
    ld      x18, 136(t0)
    ld      x19, 144(t0)
    ld      x20, 152(t0)
    ld      x21, 160(t0)
    ld      x22, 168(t0)
    ld      x23, 176(t0)
    ld      x24, 184(t0)
    ld      x25, 192(t0)
    ld      x26, 200(t0)
    ld      x27, 208(t0)
    ld      x28, 216(t0)
    ld      x29, 224(t0)
    ld      x30, 232(t0)
    ld      x31, 240(t0)
    ld      x10, 72(t0)             # a0
    ld      x5, 32(t0)              # t0 itself, last
    sret

    .globl ap_return
ap_return:                          # a0 = &kctx: longjmp back to after ap_run
    ld      ra, 0(a0)
    ld      sp, 8(a0)
    ld      s0, 16(a0)
    ld      s1, 24(a0)
    ld      s2, 32(a0)
    ld      s3, 40(a0)
    ld      s4, 48(a0)
    ld      s5, 56(a0)
    ld      s6, 64(a0)
    ld      s7, 72(a0)
    ld      s8, 80(a0)
    ld      s9, 88(a0)
    ld      s10, 96(a0)
    ld      s11, 104(a0)
    ret
"#,
    AP_TOTAL = const AP_TRAP_STK * MAX_HARTS,
    OFF_TRAPTOP = const AP_OFF_TRAPTOP,
);

unsafe extern "C" {
    fn utrap_ap();
    fn ap_run(frame: *const usize, kctx: *const usize);
    fn ap_return(kctx: *const usize) -> !;
}

const SBI_EXT_HSM: usize = 0x48534D; // "HSM"
const SBI_HSM_HART_START: usize = 0;

/// SBI Hart State Management: ask the firmware to start `hartid` at `start_addr`.
/// Returns the SBI error code (0 = SBI_SUCCESS; nonzero e.g. for an absent hart).
pub(crate) fn sbi_hart_start(hartid: usize, start_addr: usize, opaque: usize) -> isize {
    let err: isize;
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_EXT_HSM,
            in("a6") SBI_HSM_HART_START,
            in("a0") hartid,
            in("a1") start_addr,
            in("a2") opaque,
            lateout("a0") err,
            lateout("a1") _,
        );
    }
    err
}

pub(crate) static BOOT_HART: AtomicUsize = AtomicUsize::new(0);
pub(crate) static SMP_STARTED: AtomicU64 = AtomicU64::new(0); // secondaries the firmware accepted
pub(crate) static HARTS_ONLINE: AtomicU64 = AtomicU64::new(0); // secondaries that reached Rust
pub(crate) static SMP_GEN: AtomicU64 = AtomicU64::new(0); // round counter the boot hart bumps
pub(crate) static SMP_COUNTER: AtomicU64 = AtomicU64::new(0); // the shared target of the parallel work
pub(crate) static HART_RAN: [AtomicBool; MAX_HARTS] = [const { AtomicBool::new(false) }; MAX_HARTS];
pub(crate) static HART_ROUNDS: [AtomicU64; MAX_HARTS] = [const { AtomicU64::new(0) }; MAX_HARTS];
/// Supervisor timer interrupts each hart has taken while a U-mode task ran on
/// it. Per hart rather than one counter, because the claim being made is that a
/// secondary is interrupted by *its own* timer - a single total could be
/// satisfied entirely by the boot hart, which has had a timer since W9.
pub(crate) static HART_TICKS: [AtomicU64; MAX_HARTS] = [const { AtomicU64::new(0) }; MAX_HARTS];

/// A fair ticket spinlock. This is the load-bearing primitive for symmetric
/// scheduling: a run queue shared by more than one hart cannot exist without
/// mutual exclusion, and the kernel had none (single-hart discipline covered
/// everything until now). Ticket order (hand out `next`, serve them in turn)
/// gives FIFO fairness — no hart starves under contention, unlike a bare
/// test-and-set. `lock` publishes with Acquire and `unlock` with Release, so an
/// ordinary read-modify-write inside the critical section is correct.
static SMP_LOCK: TicketLock = TicketLock::new();
/// A deliberately NON-atomic counter, mutated only under `SMP_LOCK`. Atomics
/// (as in `SMP_COUNTER`) prove coherent shared memory but cannot prove a lock
/// works — the hardware serialises them regardless. A plain read-modify-write
/// under contention loses updates unless mutual exclusion actually holds, so a
/// total of exactly (participants x work) is the proof the lock is correct.
static mut SMP_GUARDED: u64 = 0;
pub(crate) const SMP_LOCK_WORK: u64 = 50_000;

// --- A shared run queue drained by every hart at once. -----------------------
//
// This is the shape of a symmetric scheduler's core: ONE queue of work, and
// several harts each popping the next item and running it, in parallel, under a
// lock. The property that must hold — and the thing the lock buys — is that every
// item runs EXACTLY once: none lost to a torn dequeue, none run twice by two
// harts that both thought they popped it. The jobs here are just markers so the
// proof is checkable; the next step is for a job to be a U-mode task dispatch
// (which additionally needs per-hart trap state + address-space switching).
pub(crate) const NJOBS: usize = 48;
const RUNQ_CAP: usize = 64;
static SMP_RUNQ_LOCK: TicketLock = TicketLock::new();

struct RunQueue {
    buf: [u32; RUNQ_CAP],
    head: usize,
    tail: usize,
}
static mut RUNQ: RunQueue = RunQueue {
    buf: [0; RUNQ_CAP],
    head: 0,
    tail: 0,
};
/// How many times each job was executed — must be exactly 1 everywhere.
pub(crate) static JOB_RUNS: [AtomicU32; NJOBS] = [const { AtomicU32::new(0) }; NJOBS];
/// Which hart ran each job (0xffff_ffff = not yet) — to show the spread.
pub(crate) static JOB_HART: [AtomicU32; NJOBS] = [const { AtomicU32::new(u32::MAX) }; NJOBS];
pub(crate) static JOBS_DONE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn runq_push(id: u32) {
    let _held = SMP_RUNQ_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(RUNQ);
        q.buf[q.tail % RUNQ_CAP] = id;
        q.tail += 1;
    }
}

pub(crate) fn runq_pop() -> Option<u32> {
    let _held = SMP_RUNQ_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(RUNQ);
        if q.head == q.tail {
            None
        } else {
            let v = q.buf[q.head % RUNQ_CAP];
            q.head += 1;
            Some(v)
        }
    }
}

/// Pop and "run" jobs until the queue is empty. Called concurrently by every hart.
pub(crate) fn drain_runq(hartid: usize) {
    while let Some(id) = runq_pop() {
        let i = id as usize;
        if i < NJOBS {
            JOB_RUNS[i].fetch_add(1, Ordering::Relaxed);
            JOB_HART[i].store(hartid as u32, Ordering::Relaxed);
        }
        JOBS_DONE.fetch_add(1, Ordering::Release);
    }
}

// --- Symmetric U-mode scheduling: any task, any hart, several at once. -------
//
// The run queue above moves markers. This moves REAL U-mode tasks: the boot hart
// fills a task queue, every secondary hart pops from it and runs whatever it gets
// in U-mode, and several tasks execute on several harts at the same time — while
// the boot hart stays on the console.
//
// Isolation is not dropped to get there. Each task gets its OWN address space: a
// private copy of the page tables in which only that task's stack region carries
// the U bit. Two tasks running concurrently on two harts therefore cannot touch
// each other's memory — proven by `smp-isolate`, where a task that reaches into a
// neighbour's stack takes a page fault instead.

/// Per-hart AP state. The trap frame MUST stay first: `sscratch` points here while
/// a task runs, so the trap entry lands on it and reads `trap_top`/`kctx` at the
/// fixed offsets `AP_OFF_TRAPTOP` / `AP_OFF_KCTX` — never via `tp`, which a U-mode
/// task is free to clobber.
#[repr(C, align(16))]
struct ApCtx {
    frame: [usize; 32],
    trap_top: usize,
    kctx: [usize; 14],
    slot: usize,
}
const EMPTY_AP_CTX: ApCtx = ApCtx {
    frame: [0; 32],
    trap_top: 0,
    kctx: [0; 14],
    slot: usize::MAX,
};
static mut AP_CTX: [ApCtx; MAX_HARTS] = [EMPTY_AP_CTX; MAX_HARTS];

unsafe extern "C" {
    static ap_trap_stacks: u8;
}

pub(crate) fn ap_trap_top(hartid: usize) -> usize {
    (core::ptr::addr_of!(ap_trap_stacks) as usize) + (hartid + 1) * AP_TRAP_STK
}

/// Task slots. One per per-task stack region, so each has an isolated stack.
pub(crate) const AP_SLOTS: usize = MAX_TASKS;
static AP_SLOT_ENTRY: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_ARG: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_SATP: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
pub(crate) static AP_SLOT_RUNS: [AtomicU32; AP_SLOTS] = [const { AtomicU32::new(0) }; AP_SLOTS];
pub(crate) static AP_SLOT_HART: [AtomicU32; AP_SLOTS] = [const { AtomicU32::new(u32::MAX) }; AP_SLOTS];
pub(crate) static AP_SLOT_EXIT: [AtomicU64; AP_SLOTS] = [const { AtomicU64::new(u64::MAX) }; AP_SLOTS];
pub(crate) static AP_SLOT_FAULT: [AtomicBool; AP_SLOTS] = [const { AtomicBool::new(false) }; AP_SLOTS];
/// The two page-table frames each slot's address space owns, so they can be freed.
static AP_SLOT_ROOT: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];
static AP_SLOT_L1: [AtomicUsize; AP_SLOTS] = [const { AtomicUsize::new(0) }; AP_SLOTS];

/// The task queue the harts pull from, plus liveness gauges.
static AP_Q_LOCK: TicketLock = TicketLock::new();
static mut AP_Q: RunQueue = RunQueue {
    buf: [0; RUNQ_CAP],
    head: 0,
    tail: 0,
};
pub(crate) static AP_SCHED_ON: AtomicBool = AtomicBool::new(false);
pub(crate) static AP_TASKS_DONE: AtomicU64 = AtomicU64::new(0);
/// U-mode tasks executing right now, and the high-water mark — the number that
/// proves tasks really overlapped rather than running one after another.
pub(crate) static AP_LIVE: AtomicU64 = AtomicU64::new(0);
pub(crate) static AP_LIVE_MAX: AtomicU64 = AtomicU64::new(0);

pub(crate) fn ap_q_push(slot: u32) {
    let _held = AP_Q_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(AP_Q);
        q.buf[q.tail % RUNQ_CAP] = slot;
        q.tail += 1;
    }
}

pub(crate) fn ap_q_pop() -> Option<u32> {
    let _held = AP_Q_LOCK.lock();
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(AP_Q);
        if q.head == q.tail {
            None
        } else {
            let v = q.buf[q.head % RUNQ_CAP];
            q.head += 1;
            Some(v)
        }
    }
}

/// Build a private address space for one task slot: copy the kernel page tables,
/// then clear the U bit on EVERY task stack region except this slot's. Shared task
/// code stays U+X (read/execute only), so tasks share code but never data.
pub(crate) fn build_ap_slot_space(slot: usize) -> usize {
    let root_pa = frame_alloc();
    let l1_pa = frame_alloc();
    if root_pa == 0 || l1_pa == 0 {
        return 0;
    }
    unsafe {
        let src_root = &(*ROOT.get()).0;
        let src_l1 = &(*L1.get()).0;
        let dr = root_pa as *mut u64;
        let dl = l1_pa as *mut u64;
        for i in 0..512usize {
            write_volatile(dr.add(i), src_root[i]);
            write_volatile(dl.add(i), src_l1[i]);
        }
        // No task stack is reachable from U-mode...
        for i in 0..MAX_TASKS {
            let idx = stack_region_l1_index(i);
            let e = read_volatile(dl.add(idx)) & !PTE_U;
            write_volatile(dl.add(idx), e);
        }
        // ...except this slot's own.
        let mine = stack_region_l1_index(slot);
        let e = read_volatile(dl.add(mine)) | PTE_U;
        write_volatile(dl.add(mine), e);
        // Point the copied root at the copied L1.
        write_volatile(dr.add(2), ((l1_pa as u64 >> 12) << 10) | PTE_V);
    }
    AP_SLOT_ROOT[slot].store(root_pa, Ordering::Relaxed);
    AP_SLOT_L1[slot].store(l1_pa, Ordering::Relaxed);
    (8usize << 60) | (root_pa >> 12)
}

/// A U-mode worker. Lives in the user region (U+X) and speaks only through
/// syscalls — zero ambient authority, exactly like a boot-hart task. `a0` is its
/// slot id; it spins briefly so concurrent workers genuinely overlap in time.
#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn ap_worker_task(slot: usize) -> ! {
    sys_print(b"  [ap-task] hello from a U-mode task running on a SECONDARY hart\n");
    let mut i = 0usize;
    while i < 300_000 {
        unsafe { asm!("nop") };
        i += 1;
    }
    sys_print(b"  [ap-task] my syscalls are being serviced off the boot hart; exiting\n");
    sys_exit(slot)
}

/// A U-mode task that never yields, long enough to outrun a scheduling quantum.
///
/// `ap_worker_task` spins too, but only far enough to show a syscall being
/// serviced off the boot hart - whether it crosses a quantum depends on how fast
/// the host emulates, which is not something to assert on. This one is sized so
/// that it cannot finish inside one, so "the hart's timer fired" is a claim
/// about the kernel rather than about the host's speed.
#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn ap_spin_task(slot: usize) -> ! {
    sys_print(b"  [ap-spin] a U-mode task on a secondary hart that never yields\n");
    let mut i = 0usize;
    while i < AP_SPIN_ITERS {
        unsafe { asm!("nop") };
        i += 1;
    }
    sys_print(b"  [ap-spin] finished - and it was interrupted on the way\n");
    sys_exit(slot)
}

/// Chosen by measurement, not by feel: at 10 MHz `rdtime` a `QUANTUM` of 50,000
/// is ~5 ms, and this many iterations takes comfortably longer than that under
/// QEMU TCG, which is the slowest thing CI runs on.
pub(crate) const AP_SPIN_ITERS: usize = 40_000_000;

/// A U-mode task that reaches into ANOTHER task's stack (address in `a1`). With
/// per-task address spaces that page is not mapped U here, so it must fault.
#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn ap_rogue_task(_slot: usize, victim: usize) -> ! {
    unsafe {
        asm!("sb {v}, 0({p})", v = in(reg) 0x41usize, p = in(reg) victim);
    }
    sys_print(b"  [ap-rogue] (BUG) a cross-task stack write was NOT blocked\n");
    sys_exit(0)
}

/// AP U-mode trap handler. Per-hart state comes from the frame pointer (which is
/// the hart's `ApCtx`), never from `tp`.
#[no_mangle]
extern "C" fn ap_trap_handler(frame: *mut usize) -> *const usize {
    let scause: usize;
    unsafe {
        asm!("csrr {}, scause", out(reg) scause);
    }
    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);
    let ctx = frame as *mut ApCtx;
    let kctx = unsafe { core::ptr::addr_of!((*ctx).kctx) } as *const usize;
    let slot = unsafe { (*ctx).slot };
    let f = unsafe { &mut *(frame as *mut [usize; 32]) };

    if interrupt {
        // Supervisor timer. Until now the AP ran with interrupts masked, so a
        // U-mode task on a secondary held that hart until it exited: no timer
        // was armed there and nothing could take the CPU back. The hart now
        // arms its own timer before entering U-mode, so this arrives on the
        // hart the task is running on, is serviced there, and the task resumes.
        //
        // Resuming rather than switching is the whole of the change: this hart
        // still runs one task to completion. What it no longer does is run it
        // *uninterruptibly*. Choosing a different task here is the next step,
        // and it needs the console's task table under a lock first.
        if code == SCAUSE_TIMER {
            // Which hart this is, without reading `tp` - the task owns `tp` and
            // the trap path is required not to touch it. `ap_execute` recorded
            // the hart in `AP_SLOT_HART` before entering U-mode, and `slot`
            // reached us through `sscratch`, so the answer is already here.
            if slot < AP_SLOTS {
                let hid = AP_SLOT_HART[slot].load(Ordering::Relaxed) as usize;
                if hid < MAX_HARTS {
                    HART_TICKS[hid].fetch_add(1, Ordering::Relaxed);
                }
            }
            sbi_set_timer(rdtime() + QUANTUM);
            return frame;
        }
        // Anything else: the AP enables no other interrupt source.
        return frame;
    }
    if code == 8 {
        // Environment call from U-mode. Resume after the ecall.
        f[F_SEPC] += 4;
        match f[F_A7] {
            SYS_PRINT => {
                // Lock the UART: other harts and the console may print too.
                {
                    let _held = SMP_LOCK.lock();
                    let s = unsafe { core::slice::from_raw_parts(f[F_A0] as *const u8, f[F_A1]) };
                    for &b in s {
                        Uart.putc(b);
                    }
                }
                f[F_A0] = 0;
            }
            SYS_EXIT => {
                if slot < AP_SLOTS {
                    AP_SLOT_EXIT[slot].store(f[F_A0] as u64, Ordering::Relaxed);
                }
                unsafe { ap_return(kctx) }
            }
            _ => {
                f[F_A0] = SYS_DENIED;
            }
        }
        return frame;
    }
    // Any exception (a cross-task access, a bad pointer) ends the task cleanly on
    // this hart; the hart returns to the scheduler loop and takes the next task.
    if slot < AP_SLOTS {
        AP_SLOT_FAULT[slot].store(true, Ordering::Relaxed);
    }
    unsafe { ap_return(kctx) }
}

/// Run one task slot on this hart: enter the task's private address space, drop to
/// U-mode, and come back when it exits or faults. Kernel-region VAs are
/// identity-mapped in every space, so this hart's own PC/SP stay valid across the
/// satp switch.
unsafe fn ap_execute(hartid: usize, slot: usize) {
    let ctx = &mut *core::ptr::addr_of_mut!(AP_CTX[hartid]);
    ctx.frame = [0; 32];
    ctx.frame[F_SEPC] = AP_SLOT_ENTRY[slot].load(Ordering::Relaxed);
    ctx.frame[F_SP] = task_stack_top(slot);
    ctx.frame[F_A0] = slot;
    ctx.frame[F_A1] = AP_SLOT_ARG[slot].load(Ordering::Relaxed);
    ctx.trap_top = ap_trap_top(hartid);
    ctx.slot = slot;

    AP_SLOT_RUNS[slot].fetch_add(1, Ordering::Relaxed);
    AP_SLOT_HART[slot].store(hartid as u32, Ordering::Relaxed);

    // Track real overlap: how many U-mode tasks are live at once.
    let live = AP_LIVE.fetch_add(1, Ordering::AcqRel) + 1;
    AP_LIVE_MAX.fetch_max(live, Ordering::AcqRel);

    let satp = AP_SLOT_SATP[slot].load(Ordering::Relaxed);
    asm!("sfence.vma");
    asm!("csrw satp, {}", in(reg) satp);
    asm!("sfence.vma");
    asm!("csrw stvec, {}", in(reg) utrap_ap as *const () as usize);
    asm!("csrs sstatus, {}", in(reg) 1usize << 18); // SUM: S-mode may read the task's U pages

    // Arm this hart's own timer for the duration of the task. Before this the
    // secondary ran U-mode with `sie.STIE` clear, so nothing could take the CPU
    // back from a task that did not exit - the boot hart's timer is a different
    // hart's timer and cannot preempt this one. `sstatus.SIE` is left alone:
    // U-mode traps are taken regardless of it, and setting it would also expose
    // the hart's kernel-mode stretches here, which is not what this step claims.
    sbi_set_timer(rdtime() + QUANTUM);
    asm!("csrs sie, {}", in(reg) STIE);

    let fp = core::ptr::addr_of!(ctx.frame) as *const usize;
    let kp = core::ptr::addr_of!(ctx.kctx) as *const usize;
    ap_run(fp, kp); // returns (via ap_return) when the task exits or faults

    // Disarm before leaving U-mode behind: the hart's compute and queue rounds
    // run in S-mode with no trap vector installed, so a timer arriving there
    // would have nowhere to go.
    asm!("csrc sie, {}", in(reg) STIE);

    // Back to bare mode so the hart's compute/queue rounds are unaffected.
    asm!("csrw stvec, {}", in(reg) 0usize);
    asm!("sfence.vma");
    asm!("csrw satp, {}", in(reg) 0usize);
    asm!("sfence.vma");

    AP_LIVE.fetch_sub(1, Ordering::AcqRel);
    AP_TASKS_DONE.fetch_add(1, Ordering::Release);
}

/// Pull tasks off the shared queue and run them until it is empty. Every secondary
/// hart runs this concurrently — this is the symmetric dispatch loop.
unsafe fn ap_schedule(hartid: usize) {
    while let Some(slot) = ap_q_pop() {
        ap_execute(hartid, slot as usize);
    }
}

/// Secondary hart body. Never prints (only the boot hart owns the UART) and never
/// traps (no stvec installed here): it checks in, then serves parallel rounds the
/// boot hart opens by bumping `SMP_GEN`.
#[no_mangle]
extern "C" fn hart_main(hartid: usize) -> ! {
    if hartid < MAX_HARTS {
        HART_RAN[hartid].store(true, Ordering::Release);
    }
    HARTS_ONLINE.fetch_add(1, Ordering::Release);

    let mut served = SMP_GEN.load(Ordering::Acquire);
    loop {
        // Wait for the boot hart to open a new round, and pick up any U-mode
        // task posted here while waiting.
        //
        // This used to spin unconditionally, on the reasoning that a TCG
        // round-robin host keeps making progress that way and the hart wakes
        // promptly. Both halves are true and the cost was not being counted: on
        // TCG every vCPU shares one host budget, so harts spinning here take it
        // from the hart running the console — which is the hart draining the
        // UART. Pasting a 64-character line lands 8/10 intact at `-smp 1`, 2/8
        // at `-smp 4`, and 0/3 at `-smp 8`, where most lines never arrive at
        // all. That is issue #19, and it was never a race: it is starvation.
        while SMP_GEN.load(Ordering::Acquire) == served {
            if AP_SCHED_ON.load(Ordering::Acquire) {
                // A U-mode scheduling window is open, so the boot hart may post
                // work at any moment and the demos measure how much overlap it
                // gets. Stay hot here; the window is short and bounded.
                unsafe { ap_schedule(hartid) };
                core::hint::spin_loop();
                continue;
            }
            // Nothing to do. Sleep instead of burning the shared budget.
            //
            // `sie.STIE` is set so the timer is *locally* enabled, which is what
            // makes `wfi` resume; `sstatus.SIE` stays clear so no trap is taken
            // when it does. That distinction is load-bearing: this loop runs in
            // S-mode with `stvec` at zero, so an actual trap here would jump to
            // address zero. The spec grants exactly this - `wfi` resumes for a
            // locally enabled interrupt regardless of the global enable.
            unsafe {
                sbi_set_timer(rdtime() + IDLE_TICK);
                asm!("csrs sie, {}", in(reg) STIE);
                asm!("wfi");
                asm!("csrc sie, {}", in(reg) STIE);
            }
        }
        served = SMP_GEN.load(Ordering::Acquire);

        // (0) Drain the shared run queue: proves several harts pop one queue
        // concurrently with each item running exactly once. Done first so all
        // harts contend on the queue at the same time.
        drain_runq(hartid);

        // (1) Atomic work: proves coherent shared memory.
        let mut n = 0;
        while n < SMP_WORK {
            SMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            n += 1;
        }
        // (2) Locked work: proves mutual exclusion. The increment is a plain
        // read-modify-write on a non-atomic; without the lock, concurrent harts
        // would clobber each other and the total would come up short.
        let mut m = 0;
        while m < SMP_LOCK_WORK {
            {
                let _held = SMP_LOCK.lock();
                unsafe {
                    let p = core::ptr::addr_of_mut!(SMP_GUARDED);
                    let v = read_volatile(p);
                    write_volatile(p, v + 1);
                }
            }
            m += 1;
        }
        // Release: the boot hart's Acquire-load of this establishes that our
        // counter increments happen-before it reads the total.
        if hartid < MAX_HARTS {
            HART_ROUNDS[hartid].fetch_add(1, Ordering::Release);
        }
    }
}

/// Boot hart: start every other hart via SBI HSM and wait for them to check in.
pub(crate) fn smp_bringup(boot: usize) {
    BOOT_HART.store(boot, Ordering::Relaxed);
    let mut started = 0u64;
    for hid in 0..MAX_HARTS {
        if hid == boot {
            continue;
        }
        // An absent hart returns a nonzero error and is simply skipped, so the
        // same build works at -smp 1 and -smp 8 with no configuration.
        if sbi_hart_start(hid, _hart_start as *const () as usize, 0) == 0 {
            started += 1;
        }
    }
    SMP_STARTED.store(started, Ordering::Relaxed);

    let mut spins = 0u64;
    while HARTS_ONLINE.load(Ordering::Acquire) < started && spins < SMP_SPIN_LIMIT {
        core::hint::spin_loop();
        spins += 1;
    }
}

/// The result of one parallel round.
pub(crate) struct SmpRound {
    /// Secondary harts that participated.
    pub(crate) parts: u64,
    /// Atomic shared counter total (proves coherent memory).
    pub(crate) counter: u64,
    /// Lock-guarded non-atomic counter total (proves mutual exclusion).
    pub(crate) guarded: u64,
    /// Contributors to `guarded`: the secondaries plus the boot hart.
    pub(crate) guarded_contributors: u64,
    /// Bitmask of participating secondary hart ids.
    pub(crate) mask: u64,
    /// Run-queue jobs drained in total (must equal NJOBS).
    pub(crate) jobs_done: u64,
    /// True iff every job ran exactly once (the run-queue correctness property).
    pub(crate) jobs_each_once: bool,
    /// How many distinct harts pulled at least one job (shows the spread).
    pub(crate) job_harts: u64,
}

/// Drive one parallel round: the secondaries do atomic work and lock-guarded
/// work, and the boot hart joins the SAME lock so the contention is real (it is a
/// hart too). Returns the tallies.
pub(crate) fn smp_round() -> SmpRound {
    let started = SMP_STARTED.load(Ordering::Relaxed);
    if started == 0 {
        return SmpRound {
            parts: 0,
            counter: 0,
            guarded: 0,
            guarded_contributors: 0,
            mask: 0,
            jobs_done: 0,
            jobs_each_once: false,
            job_harts: 0,
        };
    }
    SMP_COUNTER.store(0, Ordering::Relaxed);
    unsafe { write_volatile(core::ptr::addr_of_mut!(SMP_GUARDED), 0) };
    // Reset and refill the shared run queue before opening the round.
    JOBS_DONE.store(0, Ordering::Relaxed);
    for i in 0..NJOBS {
        JOB_RUNS[i].store(0, Ordering::Relaxed);
        JOB_HART[i].store(u32::MAX, Ordering::Relaxed);
    }
    for id in 0..NJOBS {
        runq_push(id as u32);
    }
    // Release: orders both resets above before the secondaries observe the new
    // generation and start their work.
    let round_gen = SMP_GEN.fetch_add(1, Ordering::Release) + 1;

    // The boot hart joins the drain (it is a hart too), then the lock contention.
    drain_runq(BOOT_HART.load(Ordering::Relaxed));

    // The boot hart contends on the lock alongside the secondaries.
    let mut m = 0;
    while m < SMP_LOCK_WORK {
        {
            let _held = SMP_LOCK.lock();
            unsafe {
                let p = core::ptr::addr_of_mut!(SMP_GUARDED);
                let v = read_volatile(p);
                write_volatile(p, v + 1);
            }
        }
        m += 1;
    }

    let mut spins = 0u64;
    loop {
        let mut done = 0u64;
        for rounds in HART_ROUNDS.iter() {
            if rounds.load(Ordering::Acquire) >= round_gen {
                done += 1;
            }
        }
        if done >= started || spins >= SMP_SPIN_LIMIT {
            break;
        }
        core::hint::spin_loop();
        spins += 1;
    }

    let counter = SMP_COUNTER.load(Ordering::Relaxed);
    let guarded = unsafe { read_volatile(core::ptr::addr_of!(SMP_GUARDED)) };
    let mut parts = 0u64;
    let mut mask = 0u64;
    for (hid, rounds) in HART_ROUNDS.iter().enumerate() {
        if rounds.load(Ordering::Acquire) >= round_gen {
            parts += 1;
            mask |= 1 << hid;
        }
    }

    // Run-queue verdict: every job ran exactly once, and count the distinct harts
    // that pulled work.
    let jobs_done = JOBS_DONE.load(Ordering::Acquire);
    let mut jobs_each_once = true;
    let mut hart_seen = 0u64;
    for i in 0..NJOBS {
        if JOB_RUNS[i].load(Ordering::Relaxed) != 1 {
            jobs_each_once = false;
        }
        let h = JOB_HART[i].load(Ordering::Relaxed);
        if (h as usize) < MAX_HARTS {
            hart_seen |= 1 << h;
        }
    }
    let job_harts = hart_seen.count_ones() as u64;

    SmpRound {
        parts,
        counter,
        guarded,
        guarded_contributors: parts + 1, // secondaries + this boot hart
        mask,
        jobs_done,
        jobs_each_once,
        job_harts,
    }
}

/// One-line boot-time SMP proof (asserted in CI).
pub(crate) fn smp_report_boot() {
    let started = SMP_STARTED.load(Ordering::Relaxed);
    if started == 0 {
        kprintln!("[dezh-boot] smp: 1 hart (launch with -smp N for parallelism); SBI HSM bringup path present");
        return;
    }
    let r = smp_round();
    let expected = r.parts * SMP_WORK;
    let guarded_expected = r.guarded_contributors * SMP_LOCK_WORK;
    kprintln!(
        "[dezh-boot] smp: {} secondary harts online via SBI HSM; boot hart = {}",
        started,
        BOOT_HART.load(Ordering::Relaxed)
    );
    kprintln!(
        "[dezh-boot] smp: parallel round shared-counter = {} (expected {}) -> {}",
        r.counter,
        expected,
        if r.counter == expected {
            "COHERENT"
        } else {
            "MISMATCH"
        }
    );
    kprintln!(
        "[dezh-boot] smp: lock-guarded counter = {} (expected {}) -> {}",
        r.guarded,
        guarded_expected,
        if r.guarded == guarded_expected {
            "MUTEX-OK"
        } else {
            "RACE"
        }
    );
    kprintln!(
        "[dezh-boot] smp: run-queue {} jobs drained by {} harts, each exactly once -> {}",
        r.jobs_done,
        r.job_harts,
        if r.jobs_done == NJOBS as u64 && r.jobs_each_once {
            "QUEUE-OK"
        } else {
            "QUEUE-BROKEN"
        }
    );
}

/// Prepare one task slot: build its private address space and reset its tallies.
/// Called only from the boot hart, so the frame allocator is not contended.
pub(crate) fn ap_prepare_slot(slot: usize, entry: usize, arg: usize) -> bool {
    let satp = build_ap_slot_space(slot);
    if satp == 0 {
        return false;
    }
    AP_SLOT_ENTRY[slot].store(entry, Ordering::Relaxed);
    AP_SLOT_ARG[slot].store(arg, Ordering::Relaxed);
    AP_SLOT_SATP[slot].store(satp, Ordering::Relaxed);
    AP_SLOT_RUNS[slot].store(0, Ordering::Relaxed);
    AP_SLOT_HART[slot].store(u32::MAX, Ordering::Relaxed);
    AP_SLOT_EXIT[slot].store(u64::MAX, Ordering::Relaxed);
    AP_SLOT_FAULT[slot].store(false, Ordering::Relaxed);
    true
}

/// Hand `n` prepared slots to the secondaries and wait for all of them to finish.
/// Returns false on timeout.
pub(crate) fn ap_run_batch(n: usize) -> bool {
    AP_TASKS_DONE.store(0, Ordering::Release);
    AP_LIVE.store(0, Ordering::Relaxed);
    AP_LIVE_MAX.store(0, Ordering::Relaxed);
    for s in 0..n {
        ap_q_push(s as u32);
    }
    AP_SCHED_ON.store(true, Ordering::Release);
    let mut spins = 0u64;
    while AP_TASKS_DONE.load(Ordering::Acquire) < n as u64 && spins < SMP_SPIN_LIMIT {
        core::hint::spin_loop();
        spins += 1;
    }
    AP_SCHED_ON.store(false, Ordering::Release);
    AP_TASKS_DONE.load(Ordering::Acquire) >= n as u64
}

/// Release a slot's page-table frames.
pub(crate) fn ap_free_slot(slot: usize) {
    let r = AP_SLOT_ROOT[slot].swap(0, Ordering::Relaxed);
    let l = AP_SLOT_L1[slot].swap(0, Ordering::Relaxed);
    if r != 0 {
        frame_free(r);
    }
    if l != 0 {
        frame_free(l);
    }
    AP_SLOT_SATP[slot].store(0, Ordering::Relaxed);
}

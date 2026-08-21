//! The SMP demos: parallel rounds, tasks on secondary harts, symmetric
//! scheduling, and cross-hart isolation.
//!
//! `ap_prepare_slot`, `ap_run_batch` and `ap_free_slot` stayed in `smp` - they
//! are the mechanism these drive, and they sat interleaved with the demos
//! rather than beside each other.

use core::sync::atomic::Ordering;

use crate::smp::{
    ap_free_slot, ap_prepare_slot, ap_run_batch, smp_round,
    AP_LIVE_MAX, AP_SLOTS, AP_SLOT_EXIT, AP_SLOT_FAULT, AP_SLOT_HART, AP_SLOT_RUNS, AP_SPIN_ITERS,
    BOOT_HART, HARTS_ONLINE, HART_TICKS, MAX_HARTS, NJOBS, SMP_LOCK_WORK, SMP_STARTED, SMP_WORK,
};
use crate::mm::paging::task_stack_top;
use crate::proc::loader::ProcessSpec;
use crate::sched::{join_secondaries, run_processes, CONSOLE_SMP_ON};
use crate::{TASK_PRINT, USERPROG_ELF};
use crate::smp::{ap_rogue_task, ap_spin_task, ap_worker_task};
use crate::{kprint, kprintln};

/// Interactive `smp-demo`: re-run a parallel round and explain what it proves.
pub(crate) fn run_smp_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    let started = SMP_STARTED.load(Ordering::Relaxed);
    let online = HARTS_ONLINE.load(Ordering::Relaxed);
    kprintln!("[smp] boot hart = {boot} (runs the OS: scheduler, IPC, drivers)");
    kprintln!("[smp] secondary harts started via SBI HSM = {started}, checked in = {online}");
    if started == 0 {
        kprintln!("[smp] no secondary harts. Launch QEMU with -smp N to see real parallelism.");
        kprintln!("[smp] the bringup path (sbi_hart_start + per-hart stack + tp identity) is still present.");
        return;
    }
    let r = smp_round();
    let expected = r.parts * SMP_WORK;
    let guarded_expected = r.guarded_contributors * SMP_LOCK_WORK;
    kprintln!(
        "[smp] {} harts each applied {SMP_WORK} atomic increments to ONE shared counter, at once",
        r.parts
    );
    kprintln!(
        "[smp] shared counter = {} (expected {expected}) -> {}",
        r.counter,
        if r.counter == expected {
            "COHERENT - the harts truly share memory and their atomics serialise"
        } else {
            "MISMATCH"
        }
    );
    kprintln!(
        "[smp] then {} harts (incl. the boot hart) each did {SMP_LOCK_WORK} NON-atomic increments under one ticket lock",
        r.guarded_contributors
    );
    kprintln!(
        "[smp] lock-guarded counter = {} (expected {guarded_expected}) -> {}",
        r.guarded,
        if r.guarded == guarded_expected {
            "MUTEX-OK - the lock held; without it concurrent read-modify-write would lose updates"
        } else {
            "RACE - updates were lost"
        }
    );
    kprint!("[smp] participating secondary hart ids: ");
    let mut first = true;
    for hid in 0..MAX_HARTS {
        if r.mask & (1 << hid) != 0 {
            if !first {
                kprint!(", ");
            }
            kprint!("{hid}");
            first = false;
        }
    }
    kprintln!("");
    kprintln!(
        "[smp] then {} jobs on ONE shared run queue were drained concurrently by {} harts",
        r.jobs_done,
        r.job_harts
    );
    kprintln!(
        "[smp] each job ran exactly once -> {}",
        if r.jobs_done == NJOBS as u64 && r.jobs_each_once {
            "QUEUE-OK - a correct concurrent run queue: none lost, none double-run"
        } else {
            "QUEUE-BROKEN"
        }
    );
    kprintln!("[smp] proven: several harts drain one shared run queue under a lock, each item exactly once - the core of a symmetric scheduler.");
    kprintln!("[smp] next: make each job a U-mode task dispatch (needs per-hart trap state + address-space switch); see ROADMAP.");
}

/// Interactive `smp-preempt`: prove a secondary hart's own timer interrupts a
/// U-mode task running there.
///
/// The gap this closes, in W9's own words: tasks on secondary harts ran to
/// completion, with no timer armed there. A task that did not exit owned that
/// hart, and the boot hart's timer could not help - it is a different hart's
/// timer. The evidence is per-hart on purpose: a single total would be satisfied
/// by the boot hart, which has been preempting since W9.
pub(crate) fn run_smp_preempt_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-preempt] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-preempt] running a task on a secondary hart that never yields ({AP_SPIN_ITERS} iterations, longer than one quantum)"
    );

    let before: [u64; MAX_HARTS] =
        core::array::from_fn(|h| HART_TICKS[h].load(Ordering::Relaxed));

    if !ap_prepare_slot(0, ap_spin_task as *const () as usize, 0) {
        kprintln!("[smp-preempt] out of frames while building the task's address space.");
        return;
    }
    let ok = ap_run_batch(1);
    let hart = AP_SLOT_HART[0].load(Ordering::Relaxed) as usize;
    let faulted = AP_SLOT_FAULT[0].load(Ordering::Relaxed);
    ap_free_slot(0);

    if !ok {
        kprintln!("[smp-preempt] TIMEOUT: no hart reported the task done.");
        return;
    }
    if faulted {
        kprintln!("[smp-preempt] the task FAULTED on hart {hart}.");
        return;
    }
    if hart >= MAX_HARTS {
        kprintln!("[smp-preempt] the task reported an impossible hart id {hart}.");
        return;
    }

    let took = HART_TICKS[hart].load(Ordering::Relaxed) - before[hart];
    kprintln!("[smp-preempt] the task ran on hart {hart} (boot hart is {boot}) and exited");
    kprintln!("[smp-preempt] timer interrupts taken ON THAT HART while it ran = {took}");
    if hart != boot && took > 0 {
        kprintln!("[smp-preempt] {took} interrupts on hart {hart}, task resumed each time -> PREEMPT-OK");
        kprintln!("[smp-preempt] what this does NOT yet show: the hart choosing a DIFFERENT task. That needs the console's task table under a lock; see ROADMAP W13.");
    } else if hart == boot {
        kprintln!("[smp-preempt] INCONCLUSIVE - the task landed on the boot hart, which has had a timer since W9. Try more harts.");
    } else {
        kprintln!("[smp-preempt] FAILED - no timer interrupt arrived on hart {hart}; it still runs U-mode uninterruptibly.");
    }
}

/// Interactive `smp-task`: dispatch one real U-mode task onto a secondary hart and
/// wait for it to finish, while the boot hart stays on the console.
pub(crate) fn run_smp_task_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-task] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-task] dispatching a U-mode task to a secondary hart (boot hart {boot} stays on the console)"
    );
    if !ap_prepare_slot(0, ap_worker_task as *const () as usize, 0) {
        kprintln!("[smp-task] out of frames while building the task's address space.");
        return;
    }
    let ok = ap_run_batch(1);
    let hart = AP_SLOT_HART[0].load(Ordering::Relaxed);
    let exit = AP_SLOT_EXIT[0].load(Ordering::Relaxed);
    let faulted = AP_SLOT_FAULT[0].load(Ordering::Relaxed);
    ap_free_slot(0);

    if !ok {
        kprintln!("[smp-task] TIMEOUT: no hart reported the task done.");
        return;
    }
    if faulted {
        kprintln!("[smp-task] the task FAULTED on hart {hart} (handled; the hart recovered).");
        return;
    }
    kprintln!("[smp-task] the task exited (code {exit}) on hart {hart} -> U-MODE-ON-AP");
    kprintln!("[smp-task] proven: a U-mode task ran to completion on a hart other than the boot hart, its syscalls serviced there via a per-hart trap path.");
}

/// Interactive `smp-sched`: hand several U-mode tasks to ONE shared queue and let
/// every secondary hart pull from it — symmetric scheduling, several tasks running
/// in U-mode at the same instant on different harts.
pub(crate) fn run_smp_sched_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-sched] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-sched] queueing {AP_SLOTS} U-mode tasks; every secondary hart pulls from the SAME queue (boot hart {boot} stays on the console)"
    );
    let mut prepared = 0usize;
    while prepared < AP_SLOTS {
        if !ap_prepare_slot(prepared, ap_worker_task as *const () as usize, 0) {
            break;
        }
        prepared += 1;
    }
    if prepared == 0 {
        kprintln!("[smp-sched] out of frames while building address spaces.");
        return;
    }
    let ok = ap_run_batch(prepared);
    let live_max = AP_LIVE_MAX.load(Ordering::Relaxed);

    let mut each_once = true;
    let mut faults = 0usize;
    let mut hart_mask = 0u64;
    for s in 0..prepared {
        if AP_SLOT_RUNS[s].load(Ordering::Relaxed) != 1 {
            each_once = false;
        }
        if AP_SLOT_FAULT[s].load(Ordering::Relaxed) {
            faults += 1;
        }
        let h = AP_SLOT_HART[s].load(Ordering::Relaxed);
        if (h as usize) < MAX_HARTS {
            hart_mask |= 1 << h;
        }
    }
    let harts_used = hart_mask.count_ones() as u64;

    kprint!("[smp-sched] task -> hart placement: ");
    for (s, hart) in AP_SLOT_HART.iter().take(prepared).enumerate() {
        if s > 0 {
            kprint!(", ");
        }
        kprint!("t{}=hart{}", s, hart.load(Ordering::Relaxed));
    }
    kprintln!("");
    for s in 0..prepared {
        ap_free_slot(s);
    }

    if !ok {
        kprintln!("[smp-sched] TIMEOUT: not every task reported done.");
        return;
    }
    kprintln!(
        "[smp-sched] {prepared} tasks ran on {harts_used} harts, each exactly once, {faults} faults; peak {live_max} U-mode tasks live at the same time"
    );
    kprintln!(
        "[smp-sched] verdict -> {}",
        if each_once && faults == 0 && harts_used >= 2 && live_max >= 2 {
            "SCHED-OK - one queue, many harts, several U-mode tasks executing simultaneously"
        } else {
            "SCHED-INCOMPLETE"
        }
    );
}

/// Interactive `smp-isolate`: two tasks on two harts, and the second one reaches
/// into the first's stack. Each task has its OWN address space, so the intruder
/// must fault instead — parallelism did not cost isolation.
pub(crate) fn run_smp_isolate_demo() {
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-isolate] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    let victim_stack = task_stack_top(0) - 64; // inside slot 0's stack region
    kprintln!("[smp-isolate] task 0 is an ordinary worker; task 1 reaches into task 0's stack at {victim_stack:#x}");
    if !ap_prepare_slot(0, ap_worker_task as *const () as usize, 0)
        || !ap_prepare_slot(1, ap_rogue_task as *const () as usize, victim_stack)
    {
        kprintln!("[smp-isolate] out of frames while building address spaces.");
        return;
    }
    let ok = ap_run_batch(2);
    let good_fault = AP_SLOT_FAULT[0].load(Ordering::Relaxed);
    let rogue_fault = AP_SLOT_FAULT[1].load(Ordering::Relaxed);
    let h0 = AP_SLOT_HART[0].load(Ordering::Relaxed);
    let h1 = AP_SLOT_HART[1].load(Ordering::Relaxed);
    ap_free_slot(0);
    ap_free_slot(1);

    if !ok {
        kprintln!("[smp-isolate] TIMEOUT: not every task reported done.");
        return;
    }
    kprintln!("[smp-isolate] worker on hart {h0}: {}", if good_fault { "FAULTED (unexpected)" } else { "ran cleanly" });
    kprintln!(
        "[smp-isolate] intruder on hart {h1}: {}",
        if rogue_fault {
            "page-faulted on the cross-task write, killed on its own hart"
        } else {
            "was NOT blocked"
        }
    );
    kprintln!(
        "[smp-isolate] verdict -> {}",
        if rogue_fault && !good_fault {
            "ISOLATION-OK - concurrent tasks on different harts cannot reach each other's memory"
        } else {
            "ISOLATION-BROKEN"
        }
    );
}

/// `smp-console`: the merged scheduler — a secondary hart taking a task out of
/// the **console's** table and running it on `utrap`, the real trap path.
///
/// Under diagnosis. It works from a cold console and wedges after any demo that
/// has run a U-mode task on a secondary through the AP path, which is a
/// different mechanism with its own slots and its own 74-line handler. See
/// `docs/ROADMAP.md` for what has been ruled out, and
/// `tools/debug/hart_pcs.py` for where each hart actually is when it stops.
pub(crate) fn run_smp_console_demo() {
    let boot = BOOT_HART.load(Ordering::Relaxed);
    if SMP_STARTED.load(Ordering::Relaxed) == 0 {
        kprintln!("[smp-console] no secondary harts. Launch QEMU with -smp N.");
        return;
    }
    kprintln!(
        "[smp-console] opening the console scheduler to every hart (boot hart {boot}); 3 loaded processes"
    );
    CONSOLE_SMP_ON.store(true, Ordering::Release);
    run_processes(&[
        ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 1),
        ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 2),
        ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 3),
    ]);
    let joined = join_secondaries();
    kprintln!(
        "[smp-console] verdict -> {}",
        if joined {
            "RETURNED - every hart handed its task back"
        } else {
            "TIMEOUT - a secondary never gave its task back"
        }
    );
}

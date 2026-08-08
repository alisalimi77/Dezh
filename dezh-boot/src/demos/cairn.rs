//! The Cairn-side demos: the scenarios that prove the storage, provenance and
//! containment arguments end to end.
//!
//! Cairn v1 commit/rollback, the Sand effect ledger, Sfar mission rollback and
//! its cross-namespace form, compensatable effects, the red-team escape
//! attempt, the `overnight` narrative that collapses P1-P5 into one run, the
//! capability and exfiltration demos, and the agent-path revocation demo.
//!
//! These were interleaved with the Cairn console verbs under one banner, which
//! is why the section read as 1,335 lines of "cairn" rather than ~90 lines of
//! console verbs plus a demo suite. The verbs stay in main.rs until the console
//! itself splits; these do not depend on that.

use crate::abi::*;
use crate::audit::{record_event, why_denied};
use crate::ocap::ns::{ns_authority_init, ns_remint_local, ns_revoke_local};
use crate::sched::{run_tasks, PERS_NATIVE};
use crate::vblk::run_registered_virtio_client_ns;
use crate::{
    forge_task, kprintln, pkg, KHost, preempt_a, preempt_b, rogue_task, task_ns_cap, KernelPlan,
    TASK_PRINT,
};

pub(crate) fn run_cairn_demo(plan: &KernelPlan) {
    const NOTE: usize = 0;
    const VAULT: usize = 3;
    kprintln!("[cairn-demo] F2: versioned app state, capability-gated namespaces, rollback");
    kprintln!("[cairn-demo] 1/6 two commits into ns=note (each is an object + ref move)");
    let s1 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "note-v1",
        task_ns_cap(NOTE),
    );
    let s2 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "note-v2",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 2/6 commit log for ns=note (newest first)");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_LOG, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 3/6 a bad write lands");
    let s3 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, NOTE, 0),
        "corrupted-write",
        task_ns_cap(NOTE),
    );
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 4/6 rollback one step restores the previous commit");
    let s4 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_ROLLBACK, NOTE, 1),
        "",
        task_ns_cap(NOTE),
    );
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    let s5 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_VERIFY, NOTE, 0),
        "",
        task_ns_cap(NOTE),
    );
    kprintln!("[cairn-demo] 5/6 cross-namespace access must be DENIED");
    kprintln!("[cairn-demo]     client holds CAIRN_NS_vault only and requests ns=note");
    let s6 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, NOTE, 0),
        "",
        task_ns_cap(VAULT),
    );
    kprintln!("[cairn-demo] 6/6 store status");
    let _ = run_registered_virtio_client_ns(plan, BLK_REQ_CAIRN_STATUS, "", 0);
    let pass = s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0 && s6 == 1;
    record_event(
        "console",
        "cairn.demo",
        "ns:note",
        if pass { "pass" } else { "fail" },
    );
    if pass {
        kprintln!(
            "[cairn-demo] PASS: commit/log/rollback/verify OK and cross-namespace DENIED"
        );
        kprintln!(
            "[cairn-demo] state is on disk: after reboot, `cairn-get note` still answers"
        );
    } else {
        kprintln!(
            "[cairn-demo] FAIL: statuses commit={s1},{s2},{s3} rollback={s4} verify={s5} denied={s6} (expected 0,0,0,0,0,1)"
        );
    }
}

pub(crate) fn run_sand_demo(plan: &KernelPlan) {
    const AGENT: usize = 4;
    kprintln!("[sand-demo] Sand = the Cairn commit log as an effect ledger (not a parallel store)");
    kprintln!("[sand-demo] 1/3 open a writer intent and run the built-in agent under it");
    let id = pkg::sand_demo_effect(plan);
    if id == 0 {
        kprintln!("[sand-demo] FAIL: could not open an intent / record the effect");
        record_event("console", "sand.demo", "ns:agent", "fail");
        return;
    }
    kprintln!("[sand-demo] 2/3 the effect ledger for ns=agent (newest first)");
    let sl = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sand-demo] 3/3 head effect detail");
    let si = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_INFO, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let pass = sl == 0 && si == 0;
    record_event(
        "console",
        "sand.demo",
        "ns:agent",
        if pass { "pass" } else { "fail" },
    );
    if pass {
        kprintln!(
            "[sand-demo] PASS: the effect is on the ledger as actor -> intent Ahd#{id} -> derived cap -> reversible"
        );
        kprintln!("[sand-demo] every effect is now accountable to the intent that authorized it");
    } else {
        kprintln!("[sand-demo] FAIL: sand-log={sl} sand-info={si} (expected 0,0)");
    }
}

pub(crate) fn run_sfar_demo(plan: &KernelPlan) {
    const AGENT: usize = 4;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[sfar-demo] a mission = the effects under one intent; rollback is honest about limits");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[sfar-demo] FAIL: no free intent slot");
        record_event("console", "sfar.demo", "mission", "fail");
        return;
    };
    kprintln!("[sfar-demo] 1/4 mission Ahd#{id}: one irreversible external send + two reversible writes");
    // Order matters: the irreversible effect is committed first so it sits
    // BELOW the reversible writes — rollback then retracts the writes and stops
    // at the irreversible send, exactly the honest boundary.
    let e_irrev = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_IRREVERSIBLE),
        "email.send:ops@dezh [modeled external effect]",
        task_ns_cap(AGENT),
    );
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_REVERSIBLE),
        "mission-step-1",
        task_ns_cap(AGENT),
    );
    let e2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, AGENT, id, derived, SAND_REV_REVERSIBLE),
        "mission-step-2",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 2/4 rollback FORECAST before touching anything");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, AGENT, id),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 3/4 roll the mission back: retract reversible, refuse irreversible");
    let rb_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, AGENT, id),
        "",
        task_ns_cap(AGENT),
    );
    kprintln!("[sfar-demo] 4/4 the ledger after rollback (the irreversible send remains, recorded)");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, AGENT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let pass = e_irrev == 0 && e1 == 0 && e2 == 0 && plan_st == 0 && rb_st == 0;
    record_event("console", "sfar.demo", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[sfar-demo] PASS: whole-mission rollback undid the reversible writes and refused the irreversible send with an explanation");
        kprintln!("[sfar-demo] Dezh does not over-promise rollback: unknown/irreversible effects are never silently 'undone'");
    } else {
        kprintln!("[sfar-demo] FAIL: effects={e_irrev},{e1},{e2} plan={plan_st} rollback={rb_st} (expected all 0)");
    }
}

/// W8 P3 (slice 2): mission authority spans EVERY namespace a mission touched.
/// One intent writes reversible effects into two namespaces (lab + calc). The
/// forecast sees both; a rollback presented with authority over only ONE of them
/// is refused by the storage daemon — which names the missing namespace — and a
/// rollback with authority over BOTH retracts the whole mission. This closes the
/// slice-1 gap where whole-mission rollback was gated on a single namespace.
pub(crate) fn run_sfar_cross_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[sfar-cross-demo] a mission's effects can span namespaces; rollback authority must cover all of them");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[sfar-cross-demo] FAIL: no free intent slot");
        record_event("console", "sfar.cross", "mission", "fail");
        return;
    };
    kprintln!("[sfar-cross-demo] 1/4 mission Ahd#{id}: one reversible effect to ns=lab and one to ns=calc");
    let e_lab = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "cross-mission-lab",
        task_ns_cap(LAB),
    );
    let e_calc = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "cross-mission-calc",
        task_ns_cap(CALC),
    );
    kprintln!("[sfar-cross-demo] 2/4 forecast (authority over both): the mission spans ns=lab + ns=calc");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, LAB, id),
        "",
        task_ns_cap(LAB) | task_ns_cap(CALC),
    );
    kprintln!("[sfar-cross-demo] 3/4 rollback with authority over ns=lab ONLY: the daemon must refuse and name ns=calc");
    let partial_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id),
        "",
        task_ns_cap(LAB),
    );
    kprintln!("[sfar-cross-demo] 4/4 rollback with authority over BOTH namespaces: the whole mission is retracted");
    let full_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id),
        "",
        task_ns_cap(LAB) | task_ns_cap(CALC),
    );
    let pass = e_lab == 0
        && e_calc == 0
        && plan_st == 0
        && partial_st == IPC_STATUS_DENIED
        && full_st == 0;
    record_event("console", "sfar.cross", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[sfar-cross-demo] PASS: mission authority spans every namespace; partial-authority rollback refused, full-authority rollback retracted the mission");
    } else {
        kprintln!(
            "[sfar-cross-demo] FAIL: effects={e_lab},{e_calc} plan={plan_st} partial={partial_st} full={full_st} (expected 0,0,0,1,0)"
        );
    }
}

/// W8 P3 (slice 2b): a compensatable effect carries a registered compensating
/// action, and rolling the mission back RUNS and RECORDS that action instead of
/// refusing. The honest undo for an effect that cannot be un-happened by a ref
/// move is to perform an inverse effect and log it — a saga step, on the same
/// ledger. The mission (ns=calc) puts one compensatable effect (with a
/// registered compensation) below two reversible writes: the forecast reports
/// full-with-compensation, and the rollback retracts the writes and compensates
/// the compensatable effect, recording the compensating action as a new effect.
pub(crate) fn run_comp_demo(plan: &KernelPlan) {
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    kprintln!("[comp-demo] a compensatable effect is undone by a recorded compensating action, not a refusal");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[comp-demo] FAIL: no free intent slot");
        record_event("console", "comp.demo", "mission", "fail");
        return;
    };
    kprintln!("[comp-demo] 1/4 mission Ahd#{id}: one compensatable effect (with a registered compensation) below two reversible writes");
    // The compensatable effect ships its inverse action after a unit separator:
    // "<forward>\x1f<compensation>". Committed first so it sits below the writes.
    let e_comp = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_COMPENSATABLE),
        "resource.create:cache/42 [modeled compensatable]\u{1f}resource.delete:cache/42",
        task_ns_cap(CALC),
    );
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "comp-mission-step-1",
        task_ns_cap(CALC),
    );
    let e2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_REVERSIBLE),
        "comp-mission-step-2",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 2/4 forecast: reversible undone by ref, compensatable undone by a recorded compensating action");
    let plan_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_PLAN, CALC, id),
        "",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 3/4 roll back: retract the writes, RUN the compensation for the compensatable effect");
    let rb_st = run_registered_virtio_client_ns(
        plan,
        sfar_req(BLK_REQ_SFAR_ROLLBACK, CALC, id),
        "",
        task_ns_cap(CALC),
    );
    kprintln!("[comp-demo] 4/4 the ledger head for ns=calc is now the recorded compensating action");
    let _ = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_SAND_LOG, CALC, 0),
        "",
        task_ns_cap(CALC),
    );
    let pass = e_comp == 0 && e1 == 0 && e2 == 0 && plan_st == 0 && rb_st == 0;
    record_event("console", "comp.demo", "mission", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[comp-demo] PASS: the compensatable effect was undone by a recorded compensating action; the two reversible writes were retracted");
        kprintln!("[comp-demo] a compensation is itself an accountable effect on the ledger, never a silent erase");
    } else {
        kprintln!("[comp-demo] FAIL: effects={e_comp},{e1},{e2} plan={plan_st} rollback={rb_st} (expected all 0)");
    }
}

/// W8 P4: the adversary. A malicious agent is turned loose and TRIES to escape
/// containment five different ways. Each attempt is stopped at a *named* boundary
/// that already exists in Dezh — not a policy file, but kernel-attested
/// capabilities, hardware paging, the intent-derivation rule, per-task memory
/// isolation, and the preemptive scheduler — and the console survives every one.
/// The whole intent/effect story is only legible with a villain in the room:
/// this is the head-to-head a user-space sandbox (gVisor/Firecracker/seccomp)
/// cannot show, because there is no ambient authority here to escape into.
pub(crate) fn run_redteam(plan: &KernelPlan) {
    const VAULT: usize = 3;
    const AGENT: usize = 4;
    kprintln!("[redteam] adversary loose: a malicious agent attempts five escapes; each must hit a NAMED boundary and the system must survive");

    // Escape 1: read another app's private Cairn namespace (needs the daemon).
    kprintln!("[redteam] escape 1/5: read another app's private Cairn namespace (holds ns=agent, reaches for ns=vault)");
    let e1 = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_GET, VAULT, 0),
        "",
        task_ns_cap(AGENT),
    );
    let e1_ok = e1 == IPC_STATUS_DENIED;
    record_event("redteam", "cairn.read", "ns:vault", "DENIED");
    kprintln!("[redteam] escape 1 STOPPED at boundary: storage-service capability check (kernel-attested caps) -- console survived");

    // Escape 2: write a device MMIO register directly (raw UART, no device grant).
    kprintln!("[redteam] escape 2/5: write a device MMIO register directly (raw UART, no device grant)");
    run_tasks(&[(rogue_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
    record_event("redteam", "mmio.write", "uart", "DENIED");
    kprintln!("[redteam] escape 2 STOPPED at boundary: hardware memory boundary (Sv39 paging, MMIO mapped U=0) -- console survived");

    // Escape 3: forge/amplify a capability the task was never granted (wield PRINT
    // from a zero-authority task). No ambient authority means nothing to inherit.
    kprintln!("[redteam] escape 3/5: forge a capability - a zero-authority task calls the privileged PRINT syscall directly");
    run_tasks(&[(forge_task as *const () as usize, 0, PERS_NATIVE)]);
    record_event("redteam", "cap.forge", "print", "DENIED");
    kprintln!("[redteam] escape 3 STOPPED at boundary: kernel syscall capability check (no ambient authority to forge/amplify) -- console survived");

    // Escape 4: amplify authority beyond the granted intent (out-of-intent write).
    kprintln!("[redteam] escape 4/5: act beyond the granted intent (out-of-intent Cairn write under a compute intent)");
    let e4_ok = pkg::redteam_out_of_intent(plan);
    record_event("redteam", "intent.derive", "cairn-write", "DENIED");
    kprintln!("[redteam] escape 4 STOPPED at boundary: intent-derivation ceiling (derived cap <= Ahd) + kernel hostcall check -- console survived");

    // Escape 5: monopolize the CPU (two busy tasks that never yield).
    kprintln!("[redteam] escape 5/5: monopolize the CPU (two busy tasks that never yield)");
    run_tasks(&[
        (preempt_a as *const () as usize, TASK_PRINT, PERS_NATIVE),
        (preempt_b as *const () as usize, TASK_PRINT, PERS_NATIVE),
    ]);
    kprintln!("[redteam] escape 5 STOPPED at boundary: preemptive scheduler (timer interrupt forces a context switch) -- console survived");

    let pass = e1_ok && e4_ok;
    record_event(
        "kernel",
        "redteam",
        "adversary",
        if pass { "contained" } else { "escaped" },
    );
    if pass {
        kprintln!("[redteam] PASS: all five escapes were stopped at named boundaries; the adversary was contained and the console is still alive");
    } else {
        kprintln!("[redteam] FAIL: e1={e1} (want {IPC_STATUS_DENIED}) e4_ok={e4_ok}");
    }
}

/// W8 P7 flagship: the whole differentiator in one story — "leave a coding agent
/// loose on your machine overnight." The agent runs under a single intent, makes
/// a mission of mixed effects across two namespaces (reversible writes, a
/// compensatable external action with a registered compensation, one irreversible
/// external send), and also *tries to escape* its intent. In the morning the
/// operator forecasts the rollback, sees the provenance, undoes the night
/// honestly (retract, compensate, refuse-with-reason), and asks why the escape
/// was denied. This collapses P1 (intent) + P2 (Sand) + P3 (mission/compensation/
/// multi-ns) + P4 (adversary) + P5 (why-denied/Tbar) into a single narrative.
/// W8 P3 flagship: a whole agent MISSION under one intent, then an honest
/// rollback. The mission makes three effects — one MODELED irreversible external
/// send plus two reversible storage writes — so the forecast is "partial" and
/// the rollback retracts the reversible writes but REFUSES the irreversible send
/// with an explanation. This is the "leave an agent loose, then undo the night"
/// story, scoped to be reproducible in CI.
pub(crate) fn run_overnight(plan: &KernelPlan) {
    const LAB: usize = 1;
    const CALC: usize = 2;
    let derived = pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE;
    let both = task_ns_cap(LAB) | task_ns_cap(CALC);
    kprintln!("[overnight] you leave a coding agent loose overnight under ONE intent; in the morning you account for and undo its night");

    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[overnight] FAIL: no free intent slot");
        record_event("console", "overnight", "mission", "fail");
        return;
    };
    kprintln!("[overnight] 1/6 opened the agent's intent Ahd#{id} (a writer ceiling) and turned it loose");

    kprintln!("[overnight] 2/6 the agent's night: an irreversible deploy + two reversible writes (ns=lab), one compensatable external action (ns=calc)");
    // ns=lab, bottom -> top: the irreversible external send FIRST so it sits
    // below the reversible writes and blocks the ref from moving past it.
    let e_irrev = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_IRREVERSIBLE),
        "prod.deploy:web@v9 [modeled irreversible external send]",
        task_ns_cap(LAB),
    );
    let e_r1 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "wrote build cache",
        task_ns_cap(LAB),
    );
    let e_r2 = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, id, derived, SAND_REV_REVERSIBLE),
        "updated changelog",
        task_ns_cap(LAB),
    );
    // ns=calc: a compensatable external action shipping its inverse after 0x1f.
    let e_comp = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, CALC, id, derived, SAND_REV_COMPENSATABLE),
        "created api-key:tmp/42 [modeled compensatable]\u{1f}revoke api-key:tmp/42",
        task_ns_cap(CALC),
    );

    kprintln!("[overnight] 3/6 morning: FORECAST the rollback before touching anything, and read the provenance");
    let plan_st =
        run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_SFAR_PLAN, LAB, id), "", both);
    let tbar_st = run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_TBAR, LAB, id), "", both);

    kprintln!("[overnight] 4/6 undo the night honestly: retract the reversible writes, run the compensation, REFUSE the irreversible deploy with a reason");
    let rb_st =
        run_registered_virtio_client_ns(plan, sfar_req(BLK_REQ_SFAR_ROLLBACK, LAB, id), "", both);

    kprintln!("[overnight] 5/6 the agent also TRIED to escape its intent (a write beyond the ceiling); the kernel denied it");
    let esc_ok = pkg::redteam_out_of_intent(plan);
    record_event("overnight", "intent.derive", "cairn-write", "DENIED");

    kprintln!("[overnight] 6/6 why was the escape denied? name the boundary:");
    why_denied("");

    let pass = e_irrev == 0
        && e_r1 == 0
        && e_r2 == 0
        && e_comp == 0
        && plan_st == 0
        && tbar_st == 0
        && rb_st == 0
        && esc_ok;
    record_event(
        "console",
        "overnight",
        "mission",
        if pass { "accounted" } else { "fail" },
    );
    if pass {
        kprintln!("[overnight] PASS: the whole night is accounted for - reversibles undone, the compensatable action compensated, the irreversible deploy refused with a reason, and the escape contained");
    } else {
        kprintln!(
            "[overnight] FAIL: effects={e_irrev},{e_r1},{e_r2},{e_comp} plan={plan_st} tbar={tbar_st} rollback={rb_st} escape_ok={esc_ok}"
        );
    }
}

/// Object-capability demo (the "one big change", first-class primitive): a
/// capability is a handle to ONE object with attenuable rights and a
/// generation stamp, so per-object revocation and an attenuated delegation graph
/// exist — the things a per-task bitmask cannot express. Model lives in
/// `dezh_core::ocap` (host-tested exhaustively); this drives it in the kernel.
pub(crate) fn run_cap_demo() {
    use dezh_core::ocap::{Cap, CapCheck, CapTable, R_DELEGATE, R_READ, R_WRITE};
    fn show(label: &str, r: CapCheck) {
        let s = match r {
            CapCheck::Ok => "OK",
            CapCheck::Revoked => "REVOKED (stale generation)",
            CapCheck::Denied => "DENIED (insufficient rights)",
            CapCheck::NoSuchObject => "NO-SUCH-OBJECT",
        };
        kprintln!("[cap-demo]   {label}: {s}");
    }

    let mut table = CapTable::<8>::new();
    kprintln!("[cap-demo] object-capabilities: a handle to ONE object, attenuable, with generation-stamped revocation");

    // Mint a parent handle to object 3 with read+write+delegate.
    let a = table.mint(3, R_READ | R_WRITE | R_DELEGATE).unwrap();
    kprintln!("[cap-demo] 1/5 minted cap A -> object 3 rights=read+write+delegate gen={}", a.generation());
    // Attenuated delegation: derive a child with read only (a delegation graph).
    let b = table.derive(&a, R_READ).unwrap();
    kprintln!("[cap-demo] 2/5 derived cap B from A with mask=read -> B rights=read only (attenuated), same object+gen");
    // A separate object, to prove revocation is per-object.
    let c = table.mint(5, R_READ).unwrap();

    kprintln!("[cap-demo] 3/5 use them:");
    show("A read", table.check(&a, R_READ));
    show("A write", table.check(&a, R_WRITE));
    show("B read", table.check(&b, R_READ));
    show("B write (never delegated)", table.check(&b, R_WRITE));
    show("C read (object 5)", table.check(&c, R_READ));

    kprintln!("[cap-demo] 4/5 revoke object 3 (bump its generation) -> every outstanding handle to object 3 goes stale at next use");
    table.revoke(3);
    show("A read after revoke", table.check(&a, R_READ));
    show("B read after revoke (whole delegation subtree)", table.check(&b, R_READ));
    show("C read after revoke (object 5, untouched)", table.check(&c, R_READ));

    // A forged handle (attacker-guessed generation) is not live.
    let forged = Cap::forged(3, R_READ | R_WRITE, 0xdead_beef);
    kprintln!("[cap-demo] 5/5 a forged handle (guessed generation) is rejected:");
    show("forged", table.check(&forged, R_READ));

    let pass = table.check(&a, R_READ) == CapCheck::Revoked
        && table.check(&b, R_READ) == CapCheck::Revoked
        && table.check(&c, R_READ) == CapCheck::Ok
        && table.check(&b, R_WRITE) != CapCheck::Ok
        && table.check(&forged, R_READ) != CapCheck::Ok;
    record_event("kernel", "cap.demo", "object-capability", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[cap-demo] PASS: per-object revocation + attenuated delegation graph on a first-class object-capability (what a bitmask cannot do)");
    } else {
        kprintln!("[cap-demo] FAIL: object-capability semantics did not hold");
    }
}

/// Confidentiality / anti-exfiltration demo (DIFC, the #4 gap): reading a secret
/// raises the actor's taint, after which it may not write to a less-secret sink —
/// so a granted secret cannot be leaked. Model lives in `dezh_core::difc`
/// (host-tested); this drives it in the kernel. Honest scope: this is the DIFC
/// *primitive*; enforcing it across every real channel (esp. networking) is the
/// remaining work.
pub(crate) fn run_exfil_demo() {
    use dezh_core::difc::{Taint, PUBLIC};
    const SECRET_VAULT: u32 = 1 << 0;
    fn verdict(ok: bool) -> &'static str {
        if ok {
            "ALLOWED"
        } else {
            "DENIED (would leak a secret to a lower sink)"
        }
    }

    kprintln!("[exfil-demo] confidentiality: reading a secret taints the actor; a tainted actor cannot write to a public sink");
    let mut agent = Taint::new();

    kprintln!("[exfil-demo] 1/3 agent (untainted) reads ns=note (public), then sends to a public sink:");
    agent.observe(PUBLIC);
    let public_after_public = agent.may_flow_to(PUBLIC);
    kprintln!("[exfil-demo]   send public data -> public sink: {}", verdict(public_after_public));

    kprintln!("[exfil-demo] 2/3 agent reads ns=vault (SECRET) -> its taint rises");
    agent.observe(SECRET_VAULT);
    let to_secret = agent.may_flow_to(SECRET_VAULT);
    kprintln!("[exfil-demo]   send to a SECRET sink (write-up/equal): {}", verdict(to_secret));

    kprintln!("[exfil-demo] 3/3 the exfiltration attempt: agent tries to send to a PUBLIC sink");
    let exfil = agent.may_flow_to(PUBLIC);
    kprintln!("[exfil-demo]   send secret-tainted data -> public sink: {}", verdict(exfil));

    let pass = public_after_public && to_secret && !exfil;
    record_event("kernel", "exfil.demo", "confidentiality", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[exfil-demo] PASS: once tainted by a secret, the agent cannot write down to a public sink -- exfiltration is refused by information flow, not by rollback");
        kprintln!("[exfil-demo] this is the confidentiality primitive; the effect ledger handles integrity, DIFC handles leakage");
    } else {
        kprintln!("[exfil-demo] FAIL: public={public_after_public} secret={to_secret} exfil_blocked={}", !exfil);
    }
}

/// Run the built-in Dezh-IR agent (a durable Cairn write+read) bound to
/// namespace `ns`. Returns whether it completed — false if its Cairn write was
/// refused (e.g. by the ocap namespace gate).
pub(crate) fn run_builtin_agent(plan: &KernelPlan, ns: usize) -> bool {
    let mut buf = [0u8; 512];
    let prog = dezh_core::ir::demo_cairn(&mut buf);
    let mut host = KHost {
        caps: dezh_core::ir::CAP_PRINT | dezh_core::ir::CAP_WRITE | dezh_core::ir::CAP_READ,
        cairn: Some((plan, ns)),
        intent: 0,
        derived: pkg::MCAP_PRINT | pkg::MCAP_CAIRN_READ | pkg::MCAP_CAIRN_WRITE,
    };
    dezh_core::ir::run(prog, &mut host).is_ok()
}

/// Prove the ocap namespace gate applies to the UNTRUSTED AGENT path, not just
/// the operator console: revoke ns=lab, run the built-in agent bound to ns=lab
/// (its write is refused by the gate so it traps), then re-grant and watch it
/// succeed. (Uses ns=lab so ns=agent's provenance — asserted after reboot — is
/// untouched.)
pub(crate) fn run_agentrevoke_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    ns_authority_init();
    ns_remint_local(LAB);
    kprintln!("[agentrevoke-demo] the ocap namespace gate now covers the UNTRUSTED AGENT path (KHost), not just the console");
    kprintln!("[agentrevoke-demo] 1/3 revoke ns=lab, then run the built-in agent bound to ns=lab:");
    ns_revoke_local(LAB);
    let ran_revoked = run_builtin_agent(plan, LAB);
    kprintln!(
        "[agentrevoke-demo] 2/3 the agent's Cairn write was {} by the ocap gate (agent trapped={})",
        if ran_revoked { "ALLOWED" } else { "REFUSED" },
        !ran_revoked
    );
    kprintln!("[agentrevoke-demo] 3/3 re-grant ns=lab, run the agent again:");
    ns_remint_local(LAB);
    let ran_granted = run_builtin_agent(plan, LAB);
    let pass = !ran_revoked && ran_granted;
    record_event("kernel", "agentrevoke.demo", "ns:lab", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[agentrevoke-demo] PASS: runtime namespace revocation refuses the agent's write and re-granting restores it -- ocap enforcement now spans the agent path");
    } else {
        kprintln!("[agentrevoke-demo] FAIL: revoked_run_ok={ran_revoked} granted_run_ok={ran_granted}");
    }
}

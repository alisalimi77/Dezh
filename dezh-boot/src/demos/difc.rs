//! Demos for information-flow control, both directions.
//!
//! `taintflow` is the secrecy half - reading a secret taints the operator and
//! blocks the write-down until an explicit declassification. `ingress` is the
//! integrity half - bytes off the wire are not secret, they are unvalidated,
//! and they cannot become trusted state without an explicit endorsement.

use crate::cairn::console::{cairn_cmd_commit, cairn_cmd_simple};
use crate::audit::record_event;
use crate::difc::{declassify, endorse, ns_label, ns_requires, OP_TAINT};
use crate::net::marz::run_marz_ping;
use crate::{kprintln, KernelPlan, BLK_REQ_CAIRN_GET};

/// Prove DIFC enforcement on the real storage path: read a secret namespace,
/// then be refused when writing it down to a public one, until an explicit
/// declassification.
pub(crate) fn run_taintflow_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    declassify();
    kprintln!("[taintflow-demo] read a secret, then be refused writing it down to a public namespace (enforced on the storage path)");
    kprintln!("[taintflow-demo] 1/4 read ns=vault (secret) -> the operator is tainted:");
    cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, "vault");
    kprintln!("[taintflow-demo] 2/4 try to commit to ns=lab (public) -> exfiltration REFUSED:");
    cairn_cmd_commit(plan, "lab leaked-secret");
    let blocked = !unsafe { (*OP_TAINT.get()).may_flow_to(ns_label(LAB)) };
    kprintln!("[taintflow-demo] 3/4 declassify (privileged), then commit to ns=lab:");
    declassify();
    cairn_cmd_commit(plan, "lab after-declassify");
    let allowed = unsafe { (*OP_TAINT.get()).may_flow_to(ns_label(LAB)) };
    let pass = blocked && allowed;
    record_event(
        "kernel",
        "taintflow.demo",
        "confidentiality",
        if pass { "OK" } else { "fail" },
    );
    if pass {
        kprintln!("[taintflow-demo] PASS: a secret read taints the operator and blocks the write-down; declassification is the explicit, privileged escape -- confidentiality enforced on real data flow");
    } else {
        kprintln!("[taintflow-demo] FAIL: blocked={blocked} allowed={allowed}");
    }
}

/// The ingress half of information-flow control: data that came off the wire is
/// not secret, it is **unvalidated**, and it must not silently become trusted
/// state. Read from the network, be refused writing into a namespace that demands
/// an endorsement, then endorse explicitly and be allowed.
pub(crate) fn run_ingress_demo(plan: &KernelPlan) {
    const NOTE: usize = 0; // trusted state: demands an endorsement
    const LAB: usize = 1; // scratch: demands nothing
    kprintln!("[ingress-demo] the network can be READ from; what arrives is attacker-chosen, so it starts unendorsed");
    // Start from a known state: fully endorsed, untainted.
    endorse();
    declassify();

    kprintln!("[ingress-demo] 1/4 talk to the network and consume what comes back:");
    run_marz_ping("ops");
    let lowered = unsafe { (*OP_TAINT.get()).integrity() } != dezh_core::difc::TRUSTED;

    kprintln!("[ingress-demo] 2/4 try to write it into ns=note (trusted state) -> REFUSED:");
    cairn_cmd_commit(plan, "note from-the-wire");
    let blocked = !unsafe { (*OP_TAINT.get()).may_endorse_to(ns_requires(NOTE)) };

    kprintln!("[ingress-demo] 3/4 the same data may still go to ns=lab, which demands no endorsement:");
    let scratch_ok = unsafe { (*OP_TAINT.get()).may_endorse_to(ns_requires(LAB)) };
    cairn_cmd_commit(plan, "lab from-the-wire");

    kprintln!("[ingress-demo] 4/4 endorse (privileged, recorded); the gate to ns=note reopens:");
    endorse();
    let allowed = unsafe { (*OP_TAINT.get()).may_endorse_to(ns_requires(NOTE)) };
    // Write to the scratch namespace rather than ns=note: the point is proven by
    // the gate, and clobbering trusted state would be a poor way to prove we
    // protect it.
    cairn_cmd_commit(plan, "lab after-endorsement");

    let pass = lowered && blocked && scratch_ok && allowed;
    record_event(
        "kernel",
        "ingress.demo",
        "integrity",
        if pass { "OK" } else { "fail" },
    );
    if pass {
        kprintln!("[ingress-demo] PASS: INGRESS-OK -- reading the network lowers integrity, unvalidated input cannot become trusted state, and endorsement is the explicit, privileged escape");
    } else {
        kprintln!("[ingress-demo] FAIL: lowered={lowered} blocked={blocked} scratch_ok={scratch_ok} allowed={allowed}");
    }
}

//! The Cairn v1 console verbs.
//!
//! `cairn-commit`, `cairn-get`/`cairn-log`/`cairn-status`, `cairn-rollback`,
//! and the Sand and Sfar front-ends. Each parses a namespace, checks the ocap
//! and DIFC gates, then hands the request to the storage daemon over IPC - the
//! console never touches the block device itself.
//!
//! This is what the "Cairn v1 console front-end" banner actually was, once
//! step 19 removed the eleven demos and this commit removed the benchmarks and
//! the app subsystem: about 130 lines.

use crate::abi::*;
use crate::audit::record_event;
use crate::difc::{difc_may_write, difc_observe};
use crate::ocap::ns::ns_authority_live;
use crate::vblk::run_registered_virtio_client_ns;
use crate::{kprintln, task_ns_cap, KernelPlan};
pub(crate) fn cairn_parse_ns(arg: &str) -> Option<(usize, &str)> {
    let (ns_name, rest) = match arg.split_once(' ') {
        Some((n, r)) => (n, r.trim()),
        None => (arg, ""),
    };
    match cairn_ns_id(ns_name) {
        Some(ns) => Some((ns, rest)),
        None => {
            kprintln!("unknown namespace '{ns_name}' (known: note lab calc vault agent)");
            None
        }
    }
}

pub(crate) fn cairn_cmd_commit(plan: &KernelPlan, arg: &str) {
    let Some((ns, text)) = cairn_parse_ns(arg) else {
        return;
    };
    if text.is_empty() {
        kprintln!("usage: cairn-commit <ns> <text>");
        return;
    }
    if !ns_authority_live(ns) {
        record_event("console", "cairn.commit", CAIRN_NS_NAMES[ns], "DENIED");
        return;
    }
    // Confidentiality: refuse a write that would leak secret-tainted data down
    // into a lower-secrecy namespace (no write-down).
    if !difc_may_write(ns) {
        record_event("console", "cairn.commit", CAIRN_NS_NAMES[ns], "DENIED");
        return;
    }
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_COMMIT, ns, 0),
        text,
        task_ns_cap(ns),
    );
    record_event(
        "console",
        "cairn.commit",
        CAIRN_NS_NAMES[ns],
        if st == 0 { "ok" } else { "fail" },
    );
}

pub(crate) fn cairn_cmd_simple(plan: &KernelPlan, base: usize, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    if !ns_authority_live(ns) {
        return;
    }
    let st = run_registered_virtio_client_ns(plan, cairn_req(base, ns, 0), "", task_ns_cap(ns));
    // A successful READ of a labelled namespace taints the operator (DIFC).
    if base == BLK_REQ_CAIRN_GET && st == 0 {
        difc_observe(ns);
    }
}

pub(crate) fn cairn_cmd_rollback(plan: &KernelPlan, arg: &str) {
    let Some((ns, rest)) = cairn_parse_ns(arg) else {
        return;
    };
    let steps = rest.parse::<usize>().unwrap_or(1).max(1);
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_CAIRN_ROLLBACK, ns, steps),
        "",
        task_ns_cap(ns),
    );
    record_event(
        "console",
        "cairn.rollback",
        CAIRN_NS_NAMES[ns],
        if st == 0 { "ok" } else { "fail" },
    );
}

/// F2 flagship flow: versioned commits, log, a bad write, rollback, integrity
/// verify, and a cross-namespace denial backed by kernel-attested caps.
/// Sand console front-end: sand-log / sand-info are provenance views over the
/// SAME Cairn commit log (they carry no write authority — read the ns bit only).
pub(crate) fn sand_cmd(plan: &KernelPlan, base: usize, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    let _ = run_registered_virtio_client_ns(plan, cairn_req(base, ns, 0), "", task_ns_cap(ns));
}

/// W8 P2 flagship: prove the effect ledger links an effect back to the intent
/// that authorized it. Open a `writer` Ahd, run the built-in agent under it so
/// its Cairn write is derived from that intent, then read the Sand ledger and
/// show the effect carries actor -> intent(Ahd) -> derived cap -> reversibility.
/// Sfar console front-end: `sfar-plan <ahd>` (rollback forecast) and
/// `sfar-rollback <ahd>` (whole-mission retraction). A mission may span several
/// namespaces, so the operator console presents authority for all of them; the
/// storage daemon still enforces the mission-authority check per touched ns.
pub(crate) fn sfar_cmd(plan: &KernelPlan, base: usize, arg: &str) {
    const AGENT: usize = 4;
    let (cmd_name, ev_action) = match base {
        BLK_REQ_SFAR_ROLLBACK => ("sfar-rollback", "sfar.rollback"),
        BLK_REQ_TBAR => ("tbar", "tbar.query"),
        _ => ("sfar-plan", "sfar.plan"),
    };
    let Ok(id) = arg.trim().parse::<u16>() else {
        kprintln!("usage: {cmd_name} <ahd-id> (see intent-list / sand-log for the mission's Ahd)");
        return;
    };
    if id == 0 {
        kprintln!("[sfar] Ahd #0 is 'direct' (no mission); open one with intent-open");
        return;
    }
    let st = run_registered_virtio_client_ns(plan, sfar_req(base, AGENT, id), "", all_cairn_ns_caps());
    record_event(
        "console",
        ev_action,
        "mission",
        if st == 0 { "ok" } else { "fail" },
    );
}

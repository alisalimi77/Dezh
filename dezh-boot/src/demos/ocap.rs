//! Demo for runtime revocation of a live namespace capability.
//!
//! Exercises `ocap::ns` on the real storage path: a commit is refused by the
//! ocap generation check before it ever reaches the daemon.

use crate::ocap::ns::{ns_authority_init, ns_authority_ok, NS_HANDLE, NS_TABLE};
use crate::{cairn_cmd_commit, kprintln, record_event, KernelPlan};

/// Prove the migration: a namespace capability revoked at runtime stops the live
/// storage path (a commit is refused by the ocap check before it reaches the
/// daemon), and re-granting restores it.
pub(crate) fn run_nsrevoke_demo(plan: &KernelPlan) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    const CALC: usize = 2;
    ns_authority_init();
    kprintln!("[nsrevoke-demo] runtime revocation of a LIVE namespace capability (ocap generation), enforced on the storage path");
    kprintln!("[nsrevoke-demo] 1/4 commit while the capability is live:");
    cairn_cmd_commit(plan, "calc nsrev-before");
    let live1 = ns_authority_ok(CALC);
    kprintln!("[nsrevoke-demo] 2/4 ns-revoke calc (bump the generation):");
    unsafe { (*NS_TABLE.get()).revoke(CALC) };
    kprintln!("[nsrevoke-demo] 3/4 commit after revoke -> the ocap check refuses it before it reaches the daemon:");
    cairn_cmd_commit(plan, "calc nsrev-blocked");
    let blocked = !ns_authority_ok(CALC);
    kprintln!("[nsrevoke-demo] 4/4 ns-grant calc (re-mint) then commit again:");
    unsafe { (*NS_HANDLE.get())[CALC] = (*NS_TABLE.get()).mint(CALC, R_READ | R_WRITE | R_DELEGATE) };
    cairn_cmd_commit(plan, "calc nsrev-after");
    let live2 = ns_authority_ok(CALC);
    let pass = live1 && blocked && live2;
    record_event("kernel", "nsrevoke.demo", "ns:calc", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[nsrevoke-demo] PASS: a live namespace capability was revoked at runtime (generation bump) and re-granted -- what the bitmask model could not do");
    } else {
        kprintln!("[nsrevoke-demo] FAIL: live1={live1} blocked={blocked} live2={live2}");
    }
}

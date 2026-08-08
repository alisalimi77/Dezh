//! Cairn namespace authority, as an object-capability.
//!
//! The namespace capability is the first live authority migrated onto
//! `dezh_core::ocap`: the console holds a generation-stamped handle per
//! namespace, and `ns-revoke` bumps that namespace's generation so the held
//! handle goes stale and further operations on the live storage path are
//! refused - real runtime revocation of a live capability, which the coarse
//! task-capability bitmask cannot express. (The bitmask still gates the U-mode
//! client -> daemon hop; migrating that hop too is the remaining work.)
//!
//! `ns_revoke` and `ns_grant` come along rather than staying behind, because
//! revocation is only half done in the kernel table: it is also persisted at
//! the object owner, so the two writes belong in one place. They reach back
//! into the Cairn IPC path for that second write - the one inbound edge this
//! module does not own.
//!
//! The table and the handle array are private. Anything that needs to move a
//! namespace's generation without also writing through to the store goes via
//! `ns_revoke_local` / `ns_remint_local`, which exist for exactly one caller -
//! the demo - and say in their names that they are the half-operation.
//!
//! Boot hart only: namespace authority is minted, checked and revoked from the
//! console and from the storage IPC path, both of which run on the boot hart.

use crate::mm::global::Global;
use crate::{
    cairn_parse_ns, cairn_req, kprintln, record_event, run_registered_virtio_client_ns,
    task_ns_cap, KernelPlan, BLK_REQ_NS_GRANT, BLK_REQ_NS_REVOKE, CAIRN_NS_NAMES,
};

// Boot hart only: namespace authority is minted, checked and revoked from the
// console and from the storage IPC path, both of which run on the boot hart.
static NS_TABLE: Global<dezh_core::ocap::CapTable<8>> =
    Global::new(dezh_core::ocap::CapTable::new());
static NS_HANDLE: Global<[Option<dezh_core::ocap::Cap>; 8]> = Global::new([None; 8]);
static NS_INIT: Global<bool> = Global::new(false);

pub(crate) fn ns_authority_init() {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    unsafe {
        if *NS_INIT.get() {
            return;
        }
        let mut i = 0usize;
        while i < CAIRN_NS_NAMES.len() {
            (*NS_HANDLE.get())[i] = (*NS_TABLE.get()).mint(i, R_READ | R_WRITE | R_DELEGATE);
            i += 1;
        }
        *NS_INIT.get() = true;
    }
}

/// Quiet check: does the operator still hold a live capability for namespace
/// `ns` (its handle's generation still matches the object's live generation)?
pub(crate) fn ns_authority_ok(ns: usize) -> bool {
    use dezh_core::ocap::{CapCheck, R_READ};
    ns_authority_init();
    unsafe {
        (*NS_HANDLE.get())[ns].map(|h| (*NS_TABLE.get()).check(&h, R_READ)) == Some(CapCheck::Ok)
    }
}

/// Console gate: like [`ns_authority_ok`] but prints an explainable denial when
/// the namespace capability has been revoked.
pub(crate) fn ns_authority_live(ns: usize) -> bool {
    if ns_authority_ok(ns) {
        return true;
    }
    let name = CAIRN_NS_NAMES.get(ns).copied().unwrap_or("?");
    kprintln!("[cap] DENIED: namespace '{name}' capability was REVOKED (ns-grant {name} to re-mint) -- ocap generation stale");
    false
}

/// Bump a namespace's generation in the kernel table ONLY, without the
/// write-through to the store that [`ns_revoke`] also performs. The demo wants
/// to show the ocap check refusing an operation, not to leave a revocation
/// persisted in the superblock after it finishes.
pub(crate) fn ns_revoke_local(ns: usize) {
    ns_authority_init();
    unsafe { (*NS_TABLE.get()).revoke(ns) };
}

/// The inverse of [`ns_revoke_local`]: re-mint the handle at the current
/// generation, again without touching the store.
pub(crate) fn ns_remint_local(ns: usize) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    ns_authority_init();
    unsafe { (*NS_HANDLE.get())[ns] = (*NS_TABLE.get()).mint(ns, R_READ | R_WRITE | R_DELEGATE) };
}

pub(crate) fn ns_revoke(plan: &KernelPlan, arg: &str) {
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    ns_authority_init();
    // In-memory kernel gate (fast, this boot).
    unsafe { (*NS_TABLE.get()).revoke(ns) };
    // Persist at the object owner (survives reboot): the daemon records the
    // revoked flag in the superblock and enforces it on every Cairn op.
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_NS_REVOKE, ns, 0),
        "",
        task_ns_cap(ns),
    );
    kprintln!(
        "[ns-revoke] namespace '{}' capability REVOKED (kernel handle stale + persisted at the store, status={st})",
        CAIRN_NS_NAMES[ns]
    );
    record_event("kernel", "ns.revoke", CAIRN_NS_NAMES[ns], "OK");
}

pub(crate) fn ns_grant(plan: &KernelPlan, arg: &str) {
    use dezh_core::ocap::{R_DELEGATE, R_READ, R_WRITE};
    let Some((ns, _)) = cairn_parse_ns(arg) else {
        return;
    };
    ns_authority_init();
    unsafe { (*NS_HANDLE.get())[ns] = (*NS_TABLE.get()).mint(ns, R_READ | R_WRITE | R_DELEGATE) };
    let st = run_registered_virtio_client_ns(
        plan,
        cairn_req(BLK_REQ_NS_GRANT, ns, 0),
        "",
        task_ns_cap(ns),
    );
    kprintln!(
        "[ns-grant] namespace '{}' capability re-minted + persisted grant cleared at the store (status={st})",
        CAIRN_NS_NAMES[ns]
    );
    record_event("kernel", "ns.grant", CAIRN_NS_NAMES[ns], "OK");
}

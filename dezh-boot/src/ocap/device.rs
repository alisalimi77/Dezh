//! Device authority as revocable object-capability handles.
//!
//! Namespaces already carry generation-stamped handles; devices do too. The
//! operator holds one handle per device, and revoking it makes every later
//! grant of that device refuse - a hardware kill-switch that sits ABOVE the
//! finer gates, so a revoked NIC stops all egress regardless of which
//! destination the sender was authorized for.
//!
//! Unlike `ocap::ns`, nothing outside reaches past the entry points: the table,
//! the handles and the name lookup are private to this module, and the demo
//! that exercises them goes through `dev_authority_set` like the console does.
//!
//! Boot hart only: device authority is minted, checked and revoked from the
//! console, which does not run on a secondary hart. No other hart reads it.

use crate::audit::record_event;
use crate::mm::global::Global;
use crate::{kprintln};

// Only the net object has a revocation path today, but the block object owns
// index 0 of DEV_NAMES; naming it keeps the enumeration and the name table
// legible as one thing.
#[allow(dead_code)]
const DEV_OBJ_BLOCK: usize = 0;
pub(crate) const DEV_OBJ_NET: usize = 1;
const DEV_NAMES: [&str; 2] = ["block", "net"];

// Boot hart only: device authority is minted, checked and revoked from the
// console, which does not run on a secondary hart. No other hart reads it.
static DEV_TABLE: Global<dezh_core::ocap::CapTable<4>> =
    Global::new(dezh_core::ocap::CapTable::new());
static DEV_HANDLE: Global<[Option<dezh_core::ocap::Cap>; 4]> = Global::new([None; 4]);
static DEV_INIT: Global<bool> = Global::new(false);

pub(crate) fn dev_authority_init() {
    use dezh_core::ocap::{R_READ, R_WRITE};
    unsafe {
        if *DEV_INIT.get() {
            return;
        }
        let mut i = 0usize;
        while i < DEV_NAMES.len() {
            (*DEV_HANDLE.get())[i] = (*DEV_TABLE.get()).mint(i, R_READ | R_WRITE);
            i += 1;
        }
        *DEV_INIT.get() = true;
    }
}

/// Does the operator still hold a live capability for this device?
pub(crate) fn dev_authority_ok(obj: usize) -> bool {
    use dezh_core::ocap::{CapCheck, R_READ};
    dev_authority_init();
    unsafe {
        (*DEV_HANDLE.get())[obj].map(|h| (*DEV_TABLE.get()).check(&h, R_READ)) == Some(CapCheck::Ok)
    }
}

pub(crate) fn dev_authority_live(obj: usize) -> bool {
    if dev_authority_ok(obj) {
        return true;
    }
    kprintln!(
        "[cap] DENIED: device '{}' capability was REVOKED (dev-grant {} to re-mint) -- ocap generation stale",
        DEV_NAMES[obj], DEV_NAMES[obj]
    );
    false
}

fn dev_name_id(name: &str) -> Option<usize> {
    DEV_NAMES.iter().position(|n| *n == name)
}

pub(crate) fn dev_authority_set(arg: &str, grant: bool) {
    use dezh_core::ocap::{R_READ, R_WRITE};
    let Some(obj) = dev_name_id(arg.trim()) else {
        kprintln!("[cap] unknown device (known: block net)");
        return;
    };
    dev_authority_init();
    unsafe {
        if grant {
            (*DEV_HANDLE.get())[obj] = (*DEV_TABLE.get()).mint(obj, R_READ | R_WRITE);
        } else {
            (*DEV_TABLE.get()).revoke(obj);
        }
    }
    kprintln!(
        "[cap] device '{}' capability {}",
        DEV_NAMES[obj],
        if grant { "re-minted at the current generation" } else { "REVOKED (generation bumped)" }
    );
    record_event(
        "kernel",
        if grant { "dev.grant" } else { "dev.revoke" },
        DEV_NAMES[obj],
        "OK",
    );
}

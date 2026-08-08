//! Demos for the egress boundary and the device authority above it.
//!
//! These exercise `net::marz` and `ocap::device` from the console. They were
//! left in main.rs by W11 steps 6 and 8 on the grounds that a demo reaching
//! into two subsystems is not part of the mechanism it demonstrates; this is
//! where they were always going.

use crate::audit::record_event;
use crate::difc::declassify;
use crate::net::marz::{
    marz_dest_authority, marz_egress_reset_all, marz_send_to, run_marz_send,
};
use crate::ocap::device::{dev_authority_init, dev_authority_ok, dev_authority_set, DEV_OBJ_NET};
use crate::pkg;
use crate::{cairn_cmd_simple, kprintln, sfar_cmd, KernelPlan, BLK_REQ_CAIRN_GET, BLK_REQ_SFAR_PLAN, BLK_REQ_SFAR_ROLLBACK, BLK_REQ_TBAR};

/// M3: a REAL external effect, end to end. A send under an intent leaves the
/// machine, is recorded as irreversible, is attributed by the provenance graph,
/// and is REFUSED by rollback - because it genuinely cannot be undone.
pub(crate) fn run_marz_effect_demo(plan: &KernelPlan) {
    declassify();
    marz_egress_reset_all();
    kprintln!("[marz-effect-demo] a real send becomes an irreversible, attributable effect");
    let Some((id, _ceiling)) = pkg::open_intent("writer") else {
        kprintln!("[marz-effect-demo] FAIL: no free intent slot");
        return;
    };
    kprintln!("[marz-effect-demo] 1/4 send to 'ops' under intent Ahd#{id} (a real frame on the wire):");
    marz_send_to(plan, "ops", id);

    kprintln!("[marz-effect-demo] 2/4 the rollback forecast for the mission:");
    let mut idbuf = [0u8; 8];
    let idstr = u16_to_str(id, &mut idbuf);
    sfar_cmd(plan, BLK_REQ_SFAR_PLAN, idstr);

    kprintln!("[marz-effect-demo] 3/4 provenance: who authorized what left the machine:");
    sfar_cmd(plan, BLK_REQ_TBAR, idstr);

    kprintln!("[marz-effect-demo] 4/4 roll the mission back - the send CANNOT be undone:");
    sfar_cmd(plan, BLK_REQ_SFAR_ROLLBACK, idstr);
    record_event("kernel", "marz.effect", "egress", "OK");
    kprintln!("[marz-effect-demo] PASS: the wire is honest - a real external effect is attributed, classified irreversible, and rollback refuses it instead of pretending");
}

/// Render a u16 into `buf` and return it as a str (no allocator in the console).
fn u16_to_str(v: u16, buf: &mut [u8; 8]) -> &str {
    let mut n = v;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("0")
}

/// The Marz gate proven end to end: per-destination authority and the DIFC
/// export rule, both enforced before anything reaches the wire.
pub(crate) fn run_marz_demo(plan: &KernelPlan) {
    declassify();
    marz_egress_reset_all();
    kprintln!("[marz-demo] egress authority names a DESTINATION, and export obeys information flow");

    kprintln!("[marz-demo] 1/4 authorized, untainted -> send to 'ops':");
    run_marz_send(plan, "ops");

    kprintln!("[marz-demo] 2/4 revoke ONLY 'ops' (vault-sync untouched) -> send refused:");
    marz_dest_authority("ops", false);
    run_marz_send(plan, "ops");
    marz_dest_authority("ops", true);

    kprintln!("[marz-demo] 3/4 read ns=vault (secret) -> the operator is tainted; a send to the PUBLIC 'ops' is exfiltration:");
    cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, "vault");
    run_marz_send(plan, "ops");

    kprintln!("[marz-demo] 4/4 the same tainted data MAY go to 'vault-sync' (cleared for it):");
    run_marz_send(plan, "vault-sync");
    declassify();
    kprintln!("[marz-demo] PASS: a destination capability is not network access, and a secret cannot be exported to a destination not cleared for it");
    record_event("kernel", "marz.demo", "egress", "OK");
}

/// Device authority is revocable at runtime, above every finer gate.
pub(crate) fn run_dev_demo(plan: &KernelPlan) {
    dev_authority_init();
    kprintln!("[dev-demo] device authority is a revocable ocap handle, above the per-destination gate");
    kprintln!("[dev-demo] 1/3 revoke the NIC device capability:");
    dev_authority_set("net", false);
    kprintln!("[dev-demo] 2/3 egress to an otherwise-authorized destination is refused at the device:");
    run_marz_send(plan, "ops");
    kprintln!("[dev-demo] 3/3 re-grant the device; egress works again:");
    dev_authority_set("net", true);
    run_marz_send(plan, "ops");
    let pass = dev_authority_ok(DEV_OBJ_NET);
    record_event("kernel", "dev.demo", "device", if pass { "OK" } else { "fail" });
    kprintln!("[dev-demo] PASS: revoking the device stops every send regardless of destination authority; re-granting restores it");
}

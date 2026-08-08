//! Marz: the egress boundary.
//!
//! Authority to send is NOT "network access": it is a capability for a specific
//! DESTINATION, and the destination carries a secrecy label so the DIFC rule
//! applies on export - the Flume lesson, that leaving the system is a
//! declassification.
//!
//! The three demos that prove this gate (M2 egress, M3 irreversible effect, and
//! the device kill-switch) stay in main.rs with the rest of `demos/`, and the
//! DMA window Marz is granted stays beside the other virtio windows rather than
//! moving here - it is a device resource the kernel hands out, not part of the
//! gate that decides whether a frame may leave.
//!
//! Boot hart only: destinations are checked from the console path.

use crate::proc::loader::ProcessSpec;
use crate::dev::virtio::{VIRTIO_DEVICE_ID_NET, find_virtio_mmio, marz_dma_pa};
use crate::sched::{TEXIT, run_foreground_processes};
use crate::audit::record_event;
use crate::mm::global::Global;
use crate::ocap::device::DEV_OBJ_NET;
use crate::pkg;
use crate::{cairn_req_intent, dev_authority_live, difc_ingress, kprintln, BLK_REQ_CAIRN_COMMIT, FIRST_FOREGROUND_TASK, run_registered_virtio_client_ns, task_ns_cap, KernelPlan, MARZ_ELF, NS_SECRET_VAULT, OP_TAINT, SAND_REV_IRREVERSIBLE, TASK_DEVICE_VIRTIO_NET, TASK_PRINT};

struct MarzDest {
    name: &'static str,
    ip: [u8; 4],
    port: u16,
    /// How secret this destination is cleared to receive.
    label: dezh_core::difc::Label,
    /// What the ledger records when a frame actually leaves for this destination.
    record: &'static str,
}

const MARZ_DESTS: [MarzDest; 2] = [
    // A public collector: cleared for nothing secret.
    MarzDest {
        name: "ops",
        ip: [10, 0, 2, 2],
        port: 8888,
        label: 0,
        record: "egress -> ops 10.0.2.2:8888 [REAL external send, on the wire]",
    },
    // A destination cleared to receive vault-class secrets.
    MarzDest {
        name: "vault-sync",
        ip: [10, 0, 2, 3],
        port: 9999,
        label: NS_SECRET_VAULT,
        record: "egress -> vault-sync 10.0.2.3:9999 [REAL external send, on the wire]",
    },
];

/// Egress capabilities live above the Cairn namespace bits.
const MARZ_DEST_BASE: usize = 16;
pub(crate) const fn marz_dest_cap(d: usize) -> usize {
    1 << (MARZ_DEST_BASE + d)
}

/// The operator's per-destination egress authority. Revoking one destination
/// leaves the others intact — the point of naming destinations in the capability.
/// Which destinations the operator may send to. Private: the only ways to move
/// it are `marz_dest_authority` (the console verb) and `marz_egress_reset_all`
/// (what a demo needs to start from a known state). It was a bare `static mut`
/// until the demos stopped writing it directly and the last external writer
/// went away - the same trade `mm::frames` made, for the same reason.
///
/// Boot hart only: egress authority is set and checked from the console path.
static OP_EGRESS: Global<usize> = Global::new(marz_dest_cap(0) | marz_dest_cap(1));

fn marz_dest_id(name: &str) -> Option<usize> {
    MARZ_DESTS.iter().position(|d| d.name == name)
}

fn marz_dest_packed(d: &MarzDest) -> usize {
    ((d.ip[0] as usize) << 24)
        | ((d.ip[1] as usize) << 16)
        | ((d.ip[2] as usize) << 8)
        | (d.ip[3] as usize)
        | ((d.port as usize) << 32)
}

/// The egress gate: a send needs (a) the capability for THAT destination, and
/// (b) an information flow the destination may legally receive. Returns true if
/// the send may proceed; prints a named reason otherwise.
fn marz_gate(d: usize) -> bool {
    let dest = &MARZ_DESTS[d];
    if unsafe { *OP_EGRESS.get() } & marz_dest_cap(d) == 0 {
        kprintln!(
            "[marz] DENIED: no capability for destination '{}' -- egress authority names a destination, it is not 'network access'",
            dest.name
        );
        return false;
    }
    if !unsafe { (*OP_TAINT.get()).may_flow_to(dest.label) } {
        kprintln!(
            "[marz] DENIED: sending to '{}' would export secret-tainted data to a destination cleared for {:#x} (taint={:#x}) -- declassify first",
            dest.name,
            dest.label,
            unsafe { (*OP_TAINT.get()).secrecy() }
        );
        return false;
    }
    true
}

/// Marz: launch the egress daemon for an authorized destination. It receives
/// exactly two grants — the one discovered NIC page and the DMA window — plus
/// PRINT. No block authority, no other device, and it never scans for hardware.
pub(crate) fn run_marz_send(plan: &KernelPlan, arg: &str) {
    marz_send_to(plan, arg, 0);
}

/// Send to `arg` under intent `ahd` (0 = direct). On success the send is
/// recorded as an IRREVERSIBLE effect on the ledger: it already happened in the
/// outside world, so rollback must refuse it.
pub(crate) fn marz_send_to(plan: &KernelPlan, arg: &str, ahd: u16) {
    const LAB: usize = 1;
    let name = arg.trim();
    let name = if name.is_empty() { "ops" } else { name };
    let Some(d) = marz_dest_id(name) else {
        kprintln!("[marz] unknown destination '{name}' (known: ops vault-sync)");
        return;
    };
    if find_virtio_mmio(VIRTIO_DEVICE_ID_NET).is_none() {
        kprintln!("[marz] no virtio-net device; nothing to send (see net-probe)");
        record_event("kernel", "marz.send", "virtio-net", "absent");
        return;
    }
    // Device authority first: a revoked NIC stops every send, whatever
    // destination authority the caller holds.
    if !dev_authority_live(DEV_OBJ_NET) {
        record_event("kernel", "marz.send", "device", "DENIED");
        return;
    }
    if !marz_gate(d) {
        record_event("kernel", "marz.send", MARZ_DESTS[d].name, "DENIED");
        return;
    }
    let dest = &MARZ_DESTS[d];
    kprintln!(
        "[marz] authorized egress to '{}' ({}.{}.{}.{}:{}); launching the daemon with ONLY the NIC page + DMA",
        dest.name, dest.ip[0], dest.ip[1], dest.ip[2], dest.ip[3], dest.port
    );
    run_foreground_processes(&[ProcessSpec::new(
        MARZ_ELF,
        TASK_PRINT | TASK_DEVICE_VIRTIO_NET,
        0,
    )
    .args(marz_dma_pa(), marz_dest_packed(dest), 0)
    .virtio_net()]);
    let st = unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] };
    record_event(
        "kernel",
        "marz.send",
        dest.name,
        if st == 0 { "OK" } else { "fail" },
    );
    if st != 0 {
        kprintln!("[marz] egress failed (status={st})");
        return;
    }
    kprintln!("[marz] egress complete: a real frame left the machine for '{}'", dest.name);
    // The wire is not reversible. Record it as an irreversible effect so the
    // ledger attributes it and rollback refuses it honestly.
    let derived = pkg::MCAP_PRINT;
    let led = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, ahd, derived, SAND_REV_IRREVERSIBLE),
        dest.record,
        task_ns_cap(LAB),
    );
    kprintln!(
        "[marz] recorded on the ledger as IRREVERSIBLE (ns=lab, intent={}, status={led})",
        if ahd == 0 { 0 } else { ahd }
    );
}

/// `marz-ping <dest>`: the same authority as a send (the wire is the wire), but it
/// exercises the RECEIVE path — ARP resolution and a real ICMP echo whose reply the
/// daemon has to parse. Ingress is not yet a ledgered effect or DIFC-labelled; the
/// packet is dropped after matching, which is why this reads and reports only.
pub(crate) fn run_marz_ping(arg: &str) {
    let name = arg.trim();
    let name = if name.is_empty() { "ops" } else { name };
    let Some(d) = marz_dest_id(name) else {
        kprintln!("[marz] unknown destination '{name}' (known: ops vault-sync)");
        return;
    };
    if find_virtio_mmio(VIRTIO_DEVICE_ID_NET).is_none() {
        kprintln!("[marz] no virtio-net device (see net-probe)");
        return;
    }
    if !dev_authority_live(DEV_OBJ_NET) {
        record_event("kernel", "marz.ping", "device", "DENIED");
        return;
    }
    if !marz_gate(d) {
        record_event("kernel", "marz.ping", MARZ_DESTS[d].name, "DENIED");
        return;
    }
    let dest = &MARZ_DESTS[d];
    kprintln!(
        "[marz] authorized probe of '{}' ({}.{}.{}.{}); the daemon must RECEIVE to succeed",
        dest.name, dest.ip[0], dest.ip[1], dest.ip[2], dest.ip[3]
    );
    run_foreground_processes(&[ProcessSpec::new(
        MARZ_ELF,
        TASK_PRINT | TASK_DEVICE_VIRTIO_NET,
        1, // op = ping
    )
    .args(marz_dma_pa(), marz_dest_packed(dest), 0)
    .virtio_net()]);
    let st = unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] };
    record_event(
        "kernel",
        "marz.ping",
        dest.name,
        if st == 0 { "OK" } else { "fail" },
    );
    if st == 0 {
        kprintln!("[marz] NET-RX-OK: the host answered and Dezh parsed the reply (ARP + ICMP echo)");
        // Ingress is an information flow INTO the system. What came back was
        // chosen by whoever is on the other end, so consuming it lowers the
        // operator's integrity until someone validates it.
        difc_ingress("the network");
    } else {
        kprintln!("[marz] probe failed (status={st})");
    }
}

/// Restore egress authority for every known destination. A demo that is about
/// to prove a *different* gate (the device kill-switch, or the DIFC export
/// rule) needs the destination gate out of the way first, and saying so by
/// name beats each demo assembling the mask itself.
pub(crate) fn marz_egress_reset_all() {
    unsafe { *OP_EGRESS.get() = marz_dest_cap(0) | marz_dest_cap(1) };
}

pub(crate) fn marz_dest_authority(arg: &str, grant: bool) {
    let Some(d) = marz_dest_id(arg.trim()) else {
        kprintln!("[marz] unknown destination (known: ops vault-sync)");
        return;
    };
    unsafe {
        if grant {
            *OP_EGRESS.get() |= marz_dest_cap(d);
        } else {
            *OP_EGRESS.get() &= !marz_dest_cap(d);
        }
    }
    kprintln!(
        "[marz] destination '{}' egress capability {}",
        MARZ_DESTS[d].name,
        if grant { "granted" } else { "REVOKED" }
    );
    record_event(
        "kernel",
        if grant { "marz.grant" } else { "marz.revoke" },
        MARZ_DESTS[d].name,
        "OK",
    );
}

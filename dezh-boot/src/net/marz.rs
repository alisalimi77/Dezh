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
use crate::dev::virtio::{MARZ_DMA, VIRTIO_DEVICE_ID_NET, find_virtio_mmio, marz_dma_pa};
use crate::sched::{TEXIT, run_foreground_processes};
use crate::audit::record_event;
use crate::mm::global::Global;
use crate::ocap::device::DEV_OBJ_NET;
use crate::pkg;
use crate::{cairn_req_intent, dev_authority_live, difc_ingress, kprintln, BLK_REQ_CAIRN_COMMIT, FIRST_FOREGROUND_TASK, run_registered_virtio_client_ns, task_ns_cap, KernelPlan, MARZ_ELF, NS_SECRET_VAULT, OP_TAINT, SAND_REV_COMPENSATABLE, SAND_REV_IRREVERSIBLE, TASK_DEVICE_VIRTIO_NET, TASK_PRINT};

struct MarzDest {
    name: &'static str,
    ip: [u8; 4],
    port: u16,
    /// How secret this destination is cleared to receive.
    label: dezh_core::difc::Label,
    /// What the ledger records when a frame actually leaves for this destination.
    record: &'static str,
    /// What it records for a gateway EFFECT, which is a different class: an
    /// effect that can be compensated is not the same event as a frame that
    /// cannot be unsent, and the ledger should not blur them.
    record_effect: &'static str,
}

const MARZ_DESTS: [MarzDest; 2] = [
    // A public collector: cleared for nothing secret.
    MarzDest {
        name: "ops",
        ip: [10, 0, 2, 2],
        port: 8888,
        label: 0,
        record: "egress -> ops 10.0.2.2:8888 [REAL external send, on the wire]",
        record_effect: "effect -> ops 10.0.2.2:8888 [REAL external effect via gateway; compensatable, gateway outside the TCB]",
    },
    // A destination cleared to receive vault-class secrets.
    MarzDest {
        name: "vault-sync",
        ip: [10, 0, 2, 3],
        port: 9999,
        label: NS_SECRET_VAULT,
        record: "egress -> vault-sync 10.0.2.3:9999 [REAL external send, on the wire]",
        record_effect: "effect -> vault-sync 10.0.2.3:9999 [REAL external effect via gateway; compensatable, gateway outside the TCB]",
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
/// Where the request/response payload is staged in the granted DMA window. Must
/// match `REQ_OFF`/`REQ_MAX` in the Marz daemon - the window is the contract
/// between them, and there is no other channel.
const MARZ_REQ_OFF: usize = 0x3120;
const MARZ_REQ_MAX: usize = 0xE0;
/// Wire protocol version, shared with `tools/gateway/dezh_gateway.py`.
const FX_MAGIC: &str = "DEZHFX1";

/// Stage an effect request into the DMA window for the daemon to send.
/// Returns its length, or None if it will not fit - a truncated request would
/// reach the gateway as a malformed one, which is a worse failure than not
/// sending it.
fn marz_stage_request(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    if b.len() > MARZ_REQ_MAX {
        return None;
    }
    unsafe {
        let base = (MARZ_DMA.get() as *mut u8).add(MARZ_REQ_OFF);
        let mut i = 0;
        while i < b.len() {
            core::ptr::write_volatile(base.add(i), b[i]);
            i += 1;
        }
    }
    Some(b.len())
}

/// Read the gateway's reply back out of the window, as the daemon left it.
/// NUL-terminated by the daemon; anything non-ASCII is rejected rather than
/// printed, because this text came from outside the machine.
fn marz_read_reply(buf: &mut [u8; MARZ_REQ_MAX]) -> Option<usize> {
    unsafe {
        let base = (MARZ_DMA.get() as *const u8).add(MARZ_REQ_OFF);
        let mut i = 0;
        while i < MARZ_REQ_MAX {
            let c = core::ptr::read_volatile(base.add(i));
            if c == 0 {
                return Some(i);
            }
            if !(0x20..0x7f).contains(&c) {
                return None;
            }
            buf[i] = c;
            i += 1;
        }
    }
    None
}

/// Split a gateway reply into (ok, token). `DEZHFX1 OK <token> <detail>`.
fn fx_parse(reply: &str) -> Option<(bool, &str)> {
    let mut it = reply.split(' ');
    if it.next()? != FX_MAGIC {
        return None;
    }
    match it.next()? {
        "OK" => Some((true, it.next().unwrap_or("-"))),
        "ERR" => Some((false, it.next().unwrap_or("-"))),
        _ => None,
    }
}

/// Compose `"<forward>\x1f<compensating action>"`, the encoding the storage
/// daemon reads to persist a registered compensation alongside the effect.
fn fx_compose_record(out: &mut [u8; MARZ_REQ_MAX], forward: &str, dest: &str, token: &str) -> usize {
    fn put(out: &mut [u8; MARZ_REQ_MAX], n: &mut usize, s: &str) {
        for &b in s.as_bytes() {
            if *n < MARZ_REQ_MAX {
                out[*n] = b;
                *n += 1;
            }
        }
    }
    let mut n = 0usize;
    put(out, &mut n, forward);
    if n < MARZ_REQ_MAX {
        out[n] = 0x1f; // SAND_COMP_SEP
        n += 1;
    }
    put(out, &mut n, "marz-effect ");
    put(out, &mut n, dest);
    put(out, &mut n, " git.revert ");
    put(out, &mut n, token);
    n
}

/// `marz-effect <dest> <verb> <arg>`: an effect on a real external system.
///
/// The whole W12 argument in one path. Everything before the send is Dezh's to
/// prove and it proves it: the NIC device capability is live, the operator holds
/// egress authority for this *named destination*, and the DIFC export rule
/// allows it. Then the request leaves on the wire, the gateway performs a real
/// git commit, and the answer comes back.
///
/// It is recorded COMPENSATABLE rather than irreversible, which is the honest
/// class and a different one from `marz-send`. A one-way frame cannot be
/// unsent. This effect can be undone, by a specific registered action against a
/// specific token - and `sfar-rollback` runs it for real.
///
/// What Dezh does not prove, and the record must not imply: that the gateway
/// did what it said. It is outside the TCB.
pub(crate) fn marz_effect(plan: &KernelPlan, arg: &str, ahd: u16) {
    const LAB: usize = 1;
    // An effect with no mission is attributable but not forecastable: `sfar-plan`
    // works over the effects under one intent, and Ahd#0 means "direct". Opening
    // one here rather than requiring the operator to remember is what makes the
    // console verb produce a record `sfar-plan` can actually reason about.
    let ahd = if ahd != 0 {
        ahd
    } else {
        pkg::open_intent("writer").map(|(id, _)| id).unwrap_or(0)
    };
    let (dname, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
    let (verb, slug) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    let dname = if dname.is_empty() { "ops" } else { dname };
    let verb = if verb.is_empty() { "git.commit" } else { verb };
    if slug.is_empty() {
        kprintln!("usage: marz-effect <dest> <verb> <arg>   (e.g. marz-effect ops git.commit nightly)");
        return;
    }

    let Some(d) = marz_dest_id(dname) else {
        kprintln!("[marz] unknown destination '{dname}' (known: ops vault-sync)");
        return;
    };
    if find_virtio_mmio(VIRTIO_DEVICE_ID_NET).is_none() {
        kprintln!("[marz] no virtio-net device; no effect attempted (see net-probe)");
        record_event("kernel", "marz.effect", "virtio-net", "absent");
        return;
    }
    if !dev_authority_live(DEV_OBJ_NET) {
        record_event("kernel", "marz.effect", "device", "DENIED");
        return;
    }
    if !marz_gate(d) {
        record_event("kernel", "marz.effect", MARZ_DESTS[d].name, "DENIED");
        return;
    }

    let Some(len) = marz_stage_fx(ahd, verb, slug) else {
        kprintln!("[marz] request does not fit the granted DMA window; nothing sent");
        record_event("kernel", "marz.effect", MARZ_DESTS[d].name, "too-long");
        return;
    };

    let dest = &MARZ_DESTS[d];
    kprintln!(
        "[marz-effect] authorized effect '{}' -> '{}' ({}.{}.{}.{}:{})",
        verb, dest.name, dest.ip[0], dest.ip[1], dest.ip[2], dest.ip[3], dest.port
    );
    run_foreground_processes(&[ProcessSpec::new(
        MARZ_ELF,
        TASK_PRINT | TASK_DEVICE_VIRTIO_NET,
        2, // OP_EFFECT
    )
    .args(marz_dma_pa(), marz_dest_packed(dest), len)
    .virtio_net()]);
    let st = unsafe { (*TEXIT.get())[FIRST_FOREGROUND_TASK] };
    if st != 0 {
        kprintln!("[marz-effect] no outcome observed (daemon status={st}); NOT recording an effect");
        record_event("kernel", "marz.effect", dest.name, "no-reply");
        return;
    }

    let mut buf = [0u8; MARZ_REQ_MAX];
    let Some(n) = marz_read_reply(&mut buf) else {
        kprintln!("[marz-effect] the reply was not printable ASCII; discarding it unread");
        record_event("kernel", "marz.effect", dest.name, "bad-reply");
        return;
    };
    let reply = core::str::from_utf8(&buf[..n]).unwrap_or("");
    kprintln!("[marz-effect] gateway says: {reply}");
    // Bytes off the wire are attacker-chosen, not secret. Lower integrity so
    // this cannot silently become trusted state without an explicit endorse.
    difc_ingress("marz-effect gateway reply");

    let Some((ok, token)) = fx_parse(reply) else {
        record_event("kernel", "marz.effect", dest.name, "malformed");
        return;
    };
    if !ok {
        kprintln!("[marz-effect] the gateway REFUSED the effect ({token}); nothing changed out there");
        record_event("kernel", "marz.effect", dest.name, "refused");
        return;
    }

    // Register the compensation WITH the effect, in the ledger value the daemon
    // already understands: "forward\x1fcompensation". A compensatable effect
    // with no registered compensation is only a claim; this is what makes
    // `sfar-plan` able to forecast an undo rather than guess at one.
    let mut val = [0u8; MARZ_REQ_MAX];
    let vlen = fx_compose_record(&mut val, dest.record_effect, dest.name, token);
    let value = core::str::from_utf8(&val[..vlen]).unwrap_or(dest.record_effect);
    let derived = pkg::MCAP_PRINT;
    let led = run_registered_virtio_client_ns(
        plan,
        cairn_req_intent(BLK_REQ_CAIRN_COMMIT, LAB, ahd, derived, SAND_REV_COMPENSATABLE),
        value,
        task_ns_cap(LAB),
    );
    record_event("kernel", "marz.effect", dest.name, "OK");
    kprintln!(
        "[marz-effect] recorded COMPENSATABLE (ns=lab, intent={ahd}, token={token}, status={led})"
    );
    kprintln!(
        "[marz-effect] the compensating action is registered: marz-effect {} git.revert {token}",
        dest.name
    );
}

/// Build and stage `DEZHFX1 <intent> <verb> <arg>` without an allocator.
fn marz_stage_fx(ahd: u16, verb: &str, arg: &str) -> Option<usize> {
    let mut line = [0u8; MARZ_REQ_MAX];
    let mut n = 0usize;
    let mut put = |s: &str, n: &mut usize| -> bool {
        let b = s.as_bytes();
        if *n + b.len() > MARZ_REQ_MAX {
            return false;
        }
        let mut i = 0;
        while i < b.len() {
            line[*n] = b[i];
            *n += 1;
            i += 1;
        }
        true
    };
    let mut num = [0u8; 6];
    let mut v = ahd as usize;
    let mut k = num.len();
    loop {
        k -= 1;
        num[k] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    let intent = core::str::from_utf8(&num[k..]).ok()?;
    if !(put(FX_MAGIC, &mut n)
        && put(" ", &mut n)
        && put(intent, &mut n)
        && put(" ", &mut n)
        && put(verb, &mut n)
        && put(" ", &mut n)
        && put(arg, &mut n))
    {
        return None;
    }
    let text = core::str::from_utf8(&line[..n]).ok()?;
    marz_stage_request(text)
}

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

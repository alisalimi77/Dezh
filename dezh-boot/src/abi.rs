//! The wire vocabulary between the kernel console and the user-space daemons.
//!
//! Block-daemon opcodes and request numbers, the Cairn namespace table and the
//! request packing helpers, Sand reversibility classes, and the typed IPC
//! envelope's protocol and status codes. Everything here is a constant or a
//! pure function over constants - there is no state in this module and nothing
//! in it can fail.
//!
//! It was under the "cooperative multitasking scheduler" banner, which is the
//! second thing W11 found there that is not a scheduler. An ABI is exactly the
//! kind of thing that should be readable on its own: this is the definition
//! both sides of the IPC boundary are agreeing to.

pub(crate) const BLK_OP_NO_GRANT_PROBE: usize = 7;
pub(crate) const BLK_OP_DAEMON: usize = 8;
pub(crate) const BLK_OP_CLIENT_DEMO: usize = 9;
pub(crate) const BLK_OP_CLIENT_REQ: usize = 10;
pub(crate) const BLK_REQ_PROBE: usize = 1;
pub(crate) const BLK_REQ_BWRITE: usize = 2;
pub(crate) const BLK_REQ_BREAD: usize = 3;
pub(crate) const BLK_REQ_PSET: usize = 4;
pub(crate) const BLK_REQ_PGET: usize = 5;
pub(crate) const BLK_REQ_PROLLBACK: usize = 6;
pub(crate) const BLK_REQ_STOP: usize = 7;
pub(crate) const BLK_REQ_INSTALL_CHECK: usize = 8;
pub(crate) const BLK_REQ_INSTALL_INIT: usize = 9;
pub(crate) const BLK_REQ_ROOT_STATUS: usize = 10;
pub(crate) const BLK_REQ_APP_AVAILABLE: usize = 11;
pub(crate) const BLK_REQ_APP_INSTALLED: usize = 12;
pub(crate) const BLK_REQ_APP_INFO: usize = 13;
pub(crate) const BLK_REQ_APP_INSTALL_NOTE: usize = 14;
pub(crate) const BLK_REQ_APP_REQUIRE_NOTE: usize = 15;
pub(crate) const BLK_REQ_APP_REMOVE_NOTE: usize = 16;
pub(crate) const BLK_REQ_NOTE_SET: usize = 17;
pub(crate) const BLK_REQ_NOTE_GET: usize = 18;
pub(crate) const BLK_REQ_APP_INSTALL_LAB: usize = 19;
pub(crate) const BLK_REQ_APP_REQUIRE_LAB: usize = 20;
pub(crate) const BLK_REQ_APP_REMOVE_LAB: usize = 21;
pub(crate) const BLK_REQ_LAB_SET: usize = 22;
pub(crate) const BLK_REQ_LAB_GET: usize = 23;
pub(crate) const BLK_REQ_FAULT_DEMO: usize = 24;
pub(crate) const BLK_REQ_APP_INSTALL_CALC: usize = 25;
pub(crate) const BLK_REQ_APP_REQUIRE_CALC: usize = 26;
pub(crate) const BLK_REQ_APP_REMOVE_CALC: usize = 27;
pub(crate) const BLK_REQ_CALC_SET: usize = 28;
pub(crate) const BLK_REQ_CALC_GET: usize = 29;
pub(crate) const BLK_REQ_APP_INSTALL_VAULT: usize = 30;
pub(crate) const BLK_REQ_APP_REQUIRE_VAULT: usize = 31;
pub(crate) const BLK_REQ_APP_REMOVE_VAULT: usize = 32;
pub(crate) const BLK_REQ_VAULT_SET: usize = 33;
pub(crate) const BLK_REQ_VAULT_GET: usize = 34;
pub(crate) const BLK_REQ_PKG_STORE_INIT: usize = 35;
pub(crate) const BLK_REQ_PKG_REGISTRY_READ: usize = 36;
pub(crate) const BLK_REQ_PKG_REGISTRY_WRITE: usize = 37;
pub(crate) const BLK_REQ_PKG_BLOB_READ: usize = 38;
pub(crate) const BLK_REQ_PKG_BLOB_WRITE: usize = 39;
pub(crate) const BLK_REQ_PKG_JOURNAL_READ: usize = 40;
pub(crate) const BLK_REQ_PKG_JOURNAL_WRITE: usize = 41;
// 42 (CAIRN_INIT) is daemon-internal: the store lazy-formats on first use.
pub(crate) const BLK_REQ_CAIRN_COMMIT: usize = 43;
pub(crate) const BLK_REQ_CAIRN_GET: usize = 44;
pub(crate) const BLK_REQ_CAIRN_LOG: usize = 45;
pub(crate) const BLK_REQ_CAIRN_ROLLBACK: usize = 46;
pub(crate) const BLK_REQ_CAIRN_VERIFY: usize = 47;
pub(crate) const BLK_REQ_CAIRN_STATUS: usize = 48;
// Sand (W8 P2): effect-ledger view over the same enriched Cairn commit log.
pub(crate) const BLK_REQ_SAND_LOG: usize = 49;
pub(crate) const BLK_REQ_SAND_INFO: usize = 50;
// Sfar (W8 P3): mission rollback forecast + whole-mission rollback.
pub(crate) const BLK_REQ_SFAR_PLAN: usize = 51;
pub(crate) const BLK_REQ_SFAR_ROLLBACK: usize = 52;
// Tbar (W8 P5): actor -> intent -> effect provenance graph for one intent.
pub(crate) const BLK_REQ_TBAR: usize = 53;
// Persisted namespace revocation (ocap migration): the daemon records a per-ns
// revoked flag in the superblock so revocation survives reboot.
pub(crate) const BLK_REQ_NS_REVOKE: usize = 54;
pub(crate) const BLK_REQ_NS_GRANT: usize = 55;
// Task-capability bits 8..15 gate Cairn v1 namespaces 0..7 (kernel-attested on
// every IPC recv; the storage daemon checks the requested namespace's bit).
pub(crate) const TASK_CAIRN_NS_BASE: usize = 8;
pub(crate) const CAIRN_NS_NAMES: [&str; 5] = ["note", "lab", "calc", "vault", "agent"];

pub(crate) fn cairn_ns_id(name: &str) -> Option<usize> {
    CAIRN_NS_NAMES.iter().position(|n| *n == name)
}

pub(crate) const fn task_ns_cap(ns: usize) -> usize {
    1 << (TASK_CAIRN_NS_BASE + ns)
}

/// The full Cairn v1 namespace-capability set (bits 8..12). The console acts as
/// the operator/mission owner: a Sfar plan/rollback it drives may touch any
/// namespace, so it presents authority for all of them and the storage daemon
/// still enforces the mission-authority check per touched namespace.
pub(crate) const fn all_cairn_ns_caps() -> usize {
    let mut caps = 0usize;
    let mut ns = 0usize;
    while ns < CAIRN_NS_NAMES.len() {
        caps |= task_ns_cap(ns);
        ns += 1;
    }
    caps
}

/// Pack a Cairn request for the virtio-blk client: base op | ns << 8 | steps << 12.
pub(crate) fn cairn_req(base: usize, ns: usize, steps: usize) -> usize {
    base | (ns << 8) | (steps.min(0xfff) << 12)
}

/// Pack a Sand-carrying commit request: the base packing plus the intent(Ahd)
/// id in bits 24..39 and a status byte in bits 40..47 that holds the derived cap
/// (bits 0..4) and the effect's reversibility class (bits 5..6). The client
/// courier unpacks these into the commit IPC so the daemon records provenance.
/// A direct (no-intent) reversible commit uses `cairn_req` with `ahd == 0`.
pub(crate) fn cairn_req_intent(base: usize, ns: usize, ahd: u16, derived: u32, rev_class: u8) -> usize {
    let status_byte = ((derived & 0x1f) | (((rev_class & 0x3) as u32) << 5)) as usize;
    cairn_req(base, ns, 0) | ((ahd as usize) << 24) | (status_byte << 40)
}

/// Pack a Sfar (mission) request: base op | ns << 8 | ahd << 24. The mission's
/// Ahd id rides the request-id field over the commit IPC to the daemon.
pub(crate) fn sfar_req(base: usize, ns: usize, ahd: u16) -> usize {
    cairn_req(base, ns, 0) | ((ahd as usize) << 24)
}

// Reversibility classes, mirrored from the storage daemon: an effect never
// silently claims to be reversible (unknown is its own class).
pub(crate) const SAND_REV_REVERSIBLE: u8 = 0;
#[allow(dead_code)]
pub(crate) const SAND_REV_COMPENSATABLE: u8 = 1;
pub(crate) const SAND_REV_IRREVERSIBLE: u8 = 2;
#[allow(dead_code)]
pub(crate) const SAND_REV_UNKNOWN: u8 = 3;
pub(crate) const IPC_PROTO_V1: usize = 0xd1;
pub(crate) const IPC_SERVICE_SYSTEM: usize = 0;
pub(crate) const IPC_STATUS_OK: usize = 0;
pub(crate) const IPC_STATUS_DENIED: usize = 1;
pub(crate) const IPC_STATUS_UNAVAILABLE: usize = 2;
pub(crate) const IPC_STATUS_TIMEOUT: usize = 3;
pub(crate) const IPC_STATUS_BAD_REQUEST: usize = 4;
pub(crate) const IPC_STATUS_IO_FAILURE: usize = 5;
pub(crate) const IPC_STATUS_FAULTED: usize = 6;
pub(crate) const IPC_STATUS_BUSY: usize = 7;
pub(crate) const IPC_OP_PING: usize = 1;
pub(crate) const IPC_OP_TIMEOUT: usize = 2;
pub(crate) const IPC_OP_BADREQ: usize = 255;
pub(crate) const VIRTIO_SERVICE_TASK: usize = 0;
pub(crate) const FIRST_FOREGROUND_TASK: usize = 1;
pub(crate) const BENCH_ROLE_SYSCALL: usize = 1;
pub(crate) const BENCH_ROLE_IPC_SERVICE: usize = 2;
pub(crate) const BENCH_ROLE_IPC_CLIENT: usize = 3;
pub(crate) const BENCH_ROLE_CAPS: usize = 4;
pub(crate) const BENCH_SYSCALL_ITERS: usize = 200_000;
pub(crate) const BENCH_IPC_ITERS: usize = 32;
pub(crate) const NOTE_ROLE_RUN: usize = 1;
pub(crate) const NOTE_ROLE_DENY_MMIO: usize = 2;
pub(crate) const NOTE_ROLE_DENY_BLOCK: usize = 3;
pub(crate) const LAB_ROLE_UI: usize = 1;
pub(crate) const LAB_ROLE_WORKER: usize = 2;
pub(crate) const LAB_ROLE_DENY_BLOCK: usize = 3;
pub(crate) const LAB_ROLE_DENY_MMIO: usize = 4;
pub(crate) const CALC_ROLE_RUN: usize = 1;
pub(crate) const CALC_ROLE_EVAL: usize = 2;
pub(crate) const CALC_OP_ADD: usize = 1;
pub(crate) const CALC_OP_SUB: usize = 2;
pub(crate) const CALC_OP_MUL: usize = 3;
pub(crate) const CALC_OP_DIV: usize = 4;
pub(crate) const VAULT_ROLE_RUN: usize = 1;
pub(crate) const VAULT_ROLE_DENY_BLOCK: usize = 2;
pub(crate) const VAULT_ROLE_DENY_MMIO: usize = 3;

pub(crate) fn typed_word(service: usize, op: usize, request_id: usize, status: usize, arg: usize) -> usize {
    (IPC_PROTO_V1 << 56)
        | ((service & 0xff) << 48)
        | ((op & 0xff) << 40)
        | ((request_id & 0xffff) << 24)
        | ((status & 0xff) << 16)
        | (arg & 0xffff)
}

pub(crate) fn ipc_status_name(status: usize) -> &'static str {
    match status {
        IPC_STATUS_OK => "OK",
        IPC_STATUS_DENIED => "DENIED",
        IPC_STATUS_UNAVAILABLE => "UNAVAILABLE",
        IPC_STATUS_TIMEOUT => "TIMEOUT",
        IPC_STATUS_BAD_REQUEST => "BAD_REQUEST",
        IPC_STATUS_IO_FAILURE => "IO_FAILURE",
        IPC_STATUS_FAULTED => "FAULTED",
        IPC_STATUS_BUSY => "BUSY",
        _ => "UNKNOWN",
    }
}

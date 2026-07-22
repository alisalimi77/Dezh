//! `.dzp` package ingestion + transactional persistent package registry.
//!
//! Package persistence is deliberately service-mediated: this module owns
//! package metadata, verification, transaction recovery, and grant accounting,
//! but every sector read/write goes through the registered user-space
//! `virtio-block` daemon.

use core::fmt::Write;

use crate::{kprint, kprintln};
use dezh_core::{b64, dzp, ir, sig};
use dezh_kernel::KernelPlan;

// Build-time signed demo package + its publisher public key (see build.rs). The
// kernel only ever VERIFIES; no private key lives here.
include!(concat!(env!("OUT_DIR"), "/signed_demo.rs"));

// --- Manifest capability vocabulary -------------------------------------------

pub(crate) const MCAP_PRINT: u32 = 1 << 0;
pub(crate) const MCAP_IPC: u32 = 1 << 1;
pub(crate) const MCAP_UPTIME: u32 = 1 << 2;
pub(crate) const MCAP_CAIRN_READ: u32 = 1 << 3;
pub(crate) const MCAP_CAIRN_WRITE: u32 = 1 << 4;

const MCAP_TABLE: &[(&str, u32)] = &[
    ("print", MCAP_PRINT),
    ("ipc", MCAP_IPC),
    ("uptime", MCAP_UPTIME),
    ("cairn-read", MCAP_CAIRN_READ),
    ("cairn-write", MCAP_CAIRN_WRITE),
];

pub(crate) fn mcap_names(set: u32, out: &mut dyn core::fmt::Write) {
    let mut first = true;
    for &(name, bit) in MCAP_TABLE {
        if set & bit != 0 {
            if !first {
                let _ = out.write_str(" ");
            }
            let _ = out.write_str(name);
            first = false;
        }
    }
    if first {
        let _ = out.write_str("(none)");
    }
}

// --- Package store layout ------------------------------------------------------

const SECTOR_SIZE: usize = 512;
const STAGE_SIZE: usize = 64 * 1024;
static mut STAGE: [u8; STAGE_SIZE] = [0; STAGE_SIZE];

const ARENA_SIZE: usize = 128 * 1024;
static mut ARENA: [u8; ARENA_SIZE] = [0; ARENA_SIZE];
static mut ARENA_USED: usize = 0;

const MAX_PKGS: usize = 8;
const NAME_MAX: usize = 24;
const VER_MAX: usize = 12;
const ENTRY_SIZE: usize = 128;
const REGISTRY_SIZE: usize = 2 * SECTOR_SIZE;
const JOURNAL_SECTORS: usize = 8;
const JOURNAL_SIZE: usize = JOURNAL_SECTORS * SECTOR_SIZE;

const STATE_EMPTY: u8 = 0;
const STATE_ACTIVE: u8 = 1;
const STATE_REMOVED: u8 = 2;
const STATE_CORRUPT: u8 = 3;
const STATE_PENDING_INSTALL: u8 = 4;
const STATE_PENDING_REMOVE: u8 = 5;
const STATE_QUARANTINED: u8 = 6;

const PKG_STORE_MARKER_SECTOR: usize = 24;
const PKG_REGISTRY_SECTOR: usize = 25;
const PKG_REGISTRY_SECTORS_USED: usize = 2;
const PKG_REGISTRY_RESERVED_END: usize = 31;
const PKG_JOURNAL_SECTOR: usize = 32;
const PKG_JOURNAL_RESERVED_END: usize = 39;
const PKG_BLOB_FIRST_SECTOR: usize = 64;
const PKG_PREVIOUS_FIRST_SECTOR: usize = 576;
const PKG_STAGE_FIRST_SECTOR: usize = 1088;
const PKG_SLOT_SECTORS: usize = 64;
const PKG_MAX_RAW_BYTES: usize = PKG_SLOT_SECTORS * SECTOR_SIZE;
const PKG_BLOB_RESERVED_END: usize = PKG_STAGE_FIRST_SECTOR + MAX_PKGS * PKG_SLOT_SECTORS - 1;

const REG_MAGIC: &[u8; 4] = b"DPKG";
const JOURNAL_MAGIC: &[u8; 4] = b"DPJ0";
const JOURNAL_VERSION: u8 = 1;
const JOURNAL_OP_INSTALL: u8 = 1;
const JOURNAL_OP_REMOVE: u8 = 2;
const JOURNAL_OP_REPLACE: u8 = 3;
const JOURNAL_OP_ROLLBACK: u8 = 4;
const JOURNAL_PHASE_STARTED: u8 = 1;
const JOURNAL_PHASE_BLOB_WRITTEN: u8 = 2;
const JOURNAL_PHASE_REGISTRY_PENDING: u8 = 3;

const ENTRY_FLAG_PINNED: u32 = 1 << 0;
const ENTRY_FLAG_PREVIOUS_VALID: u32 = 1 << 1;

#[derive(Clone, Copy)]
pub(crate) struct PkgEntry {
    used: bool,
    slot: u8,
    name: [u8; NAME_MAX],
    name_len: u8,
    version: [u8; VER_MAX],
    version_len: u8,
    kind: u16,
    mcaps: u32,
    raw_len: u32,
    raw_crc: u32,
    off: u32,
    len: u32,
}

const EMPTY_PKG: PkgEntry = PkgEntry {
    used: false,
    slot: 0,
    name: [0; NAME_MAX],
    name_len: 0,
    version: [0; VER_MAX],
    version_len: 0,
    kind: 0,
    mcaps: 0,
    raw_len: 0,
    raw_crc: 0,
    off: 0,
    len: 0,
};

#[derive(Clone, Copy)]
struct JournalRecord {
    op: u8,
    phase: u8,
    slot: usize,
    old_state: u8,
    new_state: u8,
    mcaps: u32,
    raw_len: usize,
    raw_crc: u32,
    blob_start: usize,
    blob_count: usize,
    registry_before_crc: u32,
    registry_after_crc: u32,
    name: [u8; NAME_MAX],
    name_len: u8,
    version: [u8; VER_MAX],
    version_len: u8,
}

enum JournalState {
    Empty,
    Valid(JournalRecord),
    Corrupt(&'static str),
}

static mut PKGS: [PkgEntry; MAX_PKGS] = [EMPTY_PKG; MAX_PKGS];
static mut STORE_LOADED: bool = false;
static mut STORE_DEGRADED: bool = false;
static mut REGISTRY: [u8; REGISTRY_SIZE] = [0; REGISTRY_SIZE];

impl PkgEntry {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("<bad>")
    }
    fn version(&self) -> &str {
        core::str::from_utf8(&self.version[..self.version_len as usize]).unwrap_or("<bad>")
    }
    fn payload(&self) -> &'static [u8] {
        unsafe {
            let base = core::ptr::addr_of!(ARENA) as *const u8;
            core::slice::from_raw_parts(base.add(self.off as usize), self.len as usize)
        }
    }
}

impl JournalRecord {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("<bad>")
    }
    fn version(&self) -> &str {
        core::str::from_utf8(&self.version[..self.version_len as usize]).unwrap_or("<bad>")
    }
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn get_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn checksum(buf: &[u8]) -> u32 {
    dzp::crc32(&[buf])
}

fn slot_blob_sector(slot: usize) -> usize {
    PKG_BLOB_FIRST_SECTOR + slot * PKG_SLOT_SECTORS
}

fn previous_blob_sector(slot: usize) -> usize {
    PKG_PREVIOUS_FIRST_SECTOR + slot * PKG_SLOT_SECTORS
}

fn stage_blob_sector(slot: usize) -> usize {
    PKG_STAGE_FIRST_SECTOR + slot * PKG_SLOT_SECTORS
}

fn entry_range(slot: usize) -> core::ops::Range<usize> {
    let start = slot * ENTRY_SIZE;
    start..start + ENTRY_SIZE
}

fn entry_state(reg: &[u8], slot: usize) -> u8 {
    reg[slot * ENTRY_SIZE]
}

fn state_name(state: u8) -> &'static str {
    match state {
        STATE_EMPTY => "Empty",
        STATE_ACTIVE => "Active",
        STATE_REMOVED => "Removed",
        STATE_CORRUPT => "Corrupt",
        STATE_PENDING_INSTALL => "PendingInstall",
        STATE_PENDING_REMOVE => "PendingRemove",
        STATE_QUARANTINED => "Quarantined",
        _ => "Unknown",
    }
}

fn op_name(op: u8) -> &'static str {
    match op {
        JOURNAL_OP_INSTALL => "Install",
        JOURNAL_OP_REMOVE => "Remove",
        JOURNAL_OP_REPLACE => "Replace",
        JOURNAL_OP_ROLLBACK => "Rollback",
        _ => "Unknown",
    }
}

fn phase_name(phase: u8) -> &'static str {
    match phase {
        JOURNAL_PHASE_STARTED => "Started",
        JOURNAL_PHASE_BLOB_WRITTEN => "BlobWritten",
        JOURNAL_PHASE_REGISTRY_PENDING => "RegistryPending",
        _ => "Unknown",
    }
}

fn entry_name(reg: &[u8], slot: usize) -> &str {
    let e = slot * ENTRY_SIZE;
    let len = reg[e + 32] as usize;
    if len == 0 || len > NAME_MAX {
        return "";
    }
    core::str::from_utf8(&reg[e + 40..e + 40 + len]).unwrap_or("")
}

fn entry_version(reg: &[u8], slot: usize) -> &str {
    let e = slot * ENTRY_SIZE;
    let len = reg[e + 33] as usize;
    if len == 0 || len > VER_MAX {
        return "";
    }
    core::str::from_utf8(&reg[e + 40 + NAME_MAX..e + 40 + NAME_MAX + len]).unwrap_or("")
}

fn entry_flags(reg: &[u8], slot: usize) -> u32 {
    get_u32(reg, slot * ENTRY_SIZE + 28)
}

fn set_entry_flags(slot: usize, flags: u32) {
    let mut reg = unsafe { REGISTRY };
    put_u32(&mut reg, slot * ENTRY_SIZE + 28, flags);
    unsafe { REGISTRY = reg };
}

fn entry_is_pinned(reg: &[u8], slot: usize) -> bool {
    entry_flags(reg, slot) & ENTRY_FLAG_PINNED != 0
}

fn entry_previous_valid(reg: &[u8], slot: usize) -> bool {
    entry_flags(reg, slot) & ENTRY_FLAG_PREVIOUS_VALID != 0
}

fn entry_previous_version(reg: &[u8], slot: usize) -> &str {
    let e = slot * ENTRY_SIZE;
    let len = reg[e + 80] as usize;
    if len == 0 || len > VER_MAX {
        return "";
    }
    core::str::from_utf8(&reg[e + 104..e + 104 + len]).unwrap_or("")
}

fn clear_runtime_registry() {
    unsafe {
        PKGS = [EMPTY_PKG; MAX_PKGS];
        ARENA_USED = 0;
    }
}

fn invalidate_loaded() {
    unsafe {
        STORE_LOADED = false;
    }
}

fn set_degraded(v: bool) {
    unsafe {
        STORE_DEGRADED = v;
        if v {
            STORE_LOADED = false;
            clear_runtime_registry();
        }
    }
}

fn read_sector(plan: &KernelPlan, req: usize, sector: usize, out: &mut [u8]) -> bool {
    let st = crate::run_registered_virtio_sector_status(plan, req, sector, None);
    if st != crate::IPC_STATUS_OK {
        return false;
    }
    crate::read_virtio_output_sector(out);
    true
}

fn write_sector(plan: &KernelPlan, req: usize, sector: usize, data: &[u8]) -> bool {
    crate::run_registered_virtio_sector_status(plan, req, sector, Some(data))
        == crate::IPC_STATUS_OK
}

fn read_registry(plan: &KernelPlan) -> bool {
    let mut tmp = [0u8; REGISTRY_SIZE];
    let mut sector = 0usize;
    while sector < PKG_REGISTRY_SECTORS_USED {
        if !read_sector(
            plan,
            crate::BLK_REQ_PKG_REGISTRY_READ,
            PKG_REGISTRY_SECTOR + sector,
            &mut tmp[sector * SECTOR_SIZE..][..SECTOR_SIZE],
        ) {
            return false;
        }
        sector += 1;
    }
    unsafe { REGISTRY = tmp };
    true
}

fn write_registry(plan: &KernelPlan) -> bool {
    let reg = unsafe { REGISTRY };
    let mut sector = 0usize;
    while sector < PKG_REGISTRY_SECTORS_USED {
        let start = sector * SECTOR_SIZE;
        if !write_sector(
            plan,
            crate::BLK_REQ_PKG_REGISTRY_WRITE,
            PKG_REGISTRY_SECTOR + sector,
            &reg[start..start + SECTOR_SIZE],
        ) {
            return false;
        }
        sector += 1;
    }
    true
}

fn init_store_marker(plan: &KernelPlan) -> bool {
    crate::run_registered_virtio_sector_status(
        plan,
        crate::BLK_REQ_PKG_STORE_INIT,
        PKG_STORE_MARKER_SECTOR,
        None,
    ) == crate::IPC_STATUS_OK
}

fn read_journal_raw(plan: &KernelPlan, out: &mut [u8; JOURNAL_SIZE]) -> bool {
    let mut sector = 0usize;
    while sector < JOURNAL_SECTORS {
        if !read_sector(
            plan,
            crate::BLK_REQ_PKG_JOURNAL_READ,
            PKG_JOURNAL_SECTOR + sector,
            &mut out[sector * SECTOR_SIZE..][..SECTOR_SIZE],
        ) {
            return false;
        }
        sector += 1;
    }
    true
}

fn write_journal_raw(plan: &KernelPlan, raw: &[u8; JOURNAL_SIZE]) -> bool {
    let mut sector = 0usize;
    while sector < JOURNAL_SECTORS {
        let start = sector * SECTOR_SIZE;
        if !write_sector(
            plan,
            crate::BLK_REQ_PKG_JOURNAL_WRITE,
            PKG_JOURNAL_SECTOR + sector,
            &raw[start..start + SECTOR_SIZE],
        ) {
            return false;
        }
        sector += 1;
    }
    true
}

fn clear_journal(plan: &KernelPlan) -> bool {
    write_journal_raw(plan, &[0u8; JOURNAL_SIZE])
}

fn decode_journal(raw: &[u8; JOURNAL_SIZE]) -> JournalState {
    if raw.iter().all(|b| *b == 0) {
        return JournalState::Empty;
    }
    if &raw[0..4] != JOURNAL_MAGIC {
        return JournalState::Corrupt("bad magic");
    }
    if raw[4] != JOURNAL_VERSION {
        return JournalState::Corrupt("bad version");
    }
    let stored = get_u32(raw, 52);
    let mut tmp = *raw;
    put_u32(&mut tmp, 52, 0);
    if checksum(&tmp) != stored {
        return JournalState::Corrupt("checksum mismatch");
    }
    let slot = get_u32(raw, 12) as usize;
    let name_len = raw[56] as usize;
    let version_len = raw[57] as usize;
    if slot >= MAX_PKGS || name_len > NAME_MAX || version_len > VER_MAX {
        return JournalState::Corrupt("field out of range");
    }
    let mut name = [0u8; NAME_MAX];
    let mut version = [0u8; VER_MAX];
    name[..name_len].copy_from_slice(&raw[64..64 + name_len]);
    version[..version_len].copy_from_slice(&raw[88..88 + version_len]);
    JournalState::Valid(JournalRecord {
        op: raw[5],
        phase: raw[6],
        slot,
        old_state: raw[16],
        new_state: raw[17],
        mcaps: get_u32(raw, 24),
        raw_len: get_u32(raw, 28) as usize,
        raw_crc: get_u32(raw, 32),
        blob_start: get_u32(raw, 36) as usize,
        blob_count: get_u32(raw, 40) as usize,
        registry_before_crc: get_u32(raw, 44),
        registry_after_crc: get_u32(raw, 48),
        name,
        name_len: name_len as u8,
        version,
        version_len: version_len as u8,
    })
}

fn write_journal(plan: &KernelPlan, rec: JournalRecord) -> bool {
    let mut raw = [0u8; JOURNAL_SIZE];
    raw[0..4].copy_from_slice(JOURNAL_MAGIC);
    raw[4] = JOURNAL_VERSION;
    raw[5] = rec.op;
    raw[6] = rec.phase;
    put_u32(&mut raw, 8, 1);
    put_u32(&mut raw, 12, rec.slot as u32);
    raw[16] = rec.old_state;
    raw[17] = rec.new_state;
    put_u32(&mut raw, 24, rec.mcaps);
    put_u32(&mut raw, 28, rec.raw_len as u32);
    put_u32(&mut raw, 32, rec.raw_crc);
    put_u32(&mut raw, 36, rec.blob_start as u32);
    put_u32(&mut raw, 40, rec.blob_count as u32);
    put_u32(&mut raw, 44, rec.registry_before_crc);
    put_u32(&mut raw, 48, rec.registry_after_crc);
    raw[56] = rec.name_len;
    raw[57] = rec.version_len;
    raw[64..64 + rec.name_len as usize].copy_from_slice(&rec.name[..rec.name_len as usize]);
    raw[88..88 + rec.version_len as usize]
        .copy_from_slice(&rec.version[..rec.version_len as usize]);
    let crc = checksum(&raw);
    put_u32(&mut raw, 52, crc);
    write_journal_raw(plan, &raw)
}

fn journal_record(
    op: u8,
    phase: u8,
    slot: usize,
    old_state: u8,
    new_state: u8,
    name: &str,
    version: &str,
    mcaps: u32,
    raw_len: usize,
    raw_crc: u32,
    registry_before_crc: u32,
    registry_after_crc: u32,
) -> JournalRecord {
    let mut n = [0u8; NAME_MAX];
    let mut v = [0u8; VER_MAX];
    n[..name.len()].copy_from_slice(name.as_bytes());
    v[..version.len()].copy_from_slice(version.as_bytes());
    JournalRecord {
        op,
        phase,
        slot,
        old_state,
        new_state,
        mcaps,
        raw_len,
        raw_crc,
        blob_start: slot_blob_sector(slot),
        blob_count: raw_len.div_ceil(SECTOR_SIZE),
        registry_before_crc,
        registry_after_crc,
        name: n,
        name_len: name.len() as u8,
        version: v,
        version_len: version.len() as u8,
    }
}

fn print_journal_record(prefix: &str, rec: JournalRecord) {
    kprintln!(
        "{prefix} op={} phase={} slot={} package={} {} old={} new={} raw={} crc={:#x}",
        op_name(rec.op),
        phase_name(rec.phase),
        rec.slot,
        rec.name(),
        rec.version(),
        state_name(rec.old_state),
        state_name(rec.new_state),
        rec.raw_len,
        rec.raw_crc
    );
}

fn write_blob_at(plan: &KernelPlan, base_sector: usize, raw: &[u8]) -> bool {
    let sectors = raw.len().div_ceil(SECTOR_SIZE);
    let mut sector = 0usize;
    while sector < sectors {
        let start = sector * SECTOR_SIZE;
        let end = (start + SECTOR_SIZE).min(raw.len());
        if !write_sector(
            plan,
            crate::BLK_REQ_PKG_BLOB_WRITE,
            base_sector + sector,
            &raw[start..end],
        ) {
            return false;
        }
        sector += 1;
    }
    true
}

fn write_blob(plan: &KernelPlan, slot: usize, raw: &[u8]) -> bool {
    write_blob_at(plan, slot_blob_sector(slot), raw)
}

fn write_stage_blob(plan: &KernelPlan, slot: usize, raw: &[u8]) -> bool {
    write_blob_at(plan, stage_blob_sector(slot), raw)
}

fn zero_blob_area(plan: &KernelPlan, base_sector: usize) -> bool {
    let zero = [0u8; SECTOR_SIZE];
    let mut sector = 0usize;
    while sector < PKG_SLOT_SECTORS {
        if !write_sector(
            plan,
            crate::BLK_REQ_PKG_BLOB_WRITE,
            base_sector + sector,
            &zero,
        ) {
            return false;
        }
        sector += 1;
    }
    true
}

fn zero_blob_slot(plan: &KernelPlan, slot: usize) -> bool {
    zero_blob_area(plan, slot_blob_sector(slot))
}

fn read_blob_area_into_stage(plan: &KernelPlan, base_sector: usize, raw_len: usize) -> bool {
    if raw_len > STAGE_SIZE || raw_len > PKG_MAX_RAW_BYTES {
        return false;
    }
    let sectors = raw_len.div_ceil(SECTOR_SIZE);
    let mut tmp = [0u8; SECTOR_SIZE];
    let mut sector = 0usize;
    while sector < sectors {
        if !read_sector(
            plan,
            crate::BLK_REQ_PKG_BLOB_READ,
            base_sector + sector,
            &mut tmp,
        ) {
            return false;
        }
        let start = sector * SECTOR_SIZE;
        let end = (start + SECTOR_SIZE).min(raw_len);
        unsafe { STAGE[start..end].copy_from_slice(&tmp[..end - start]) };
        sector += 1;
    }
    true
}

fn read_blob_into_stage(plan: &KernelPlan, slot: usize, raw_len: usize) -> bool {
    read_blob_area_into_stage(plan, slot_blob_sector(slot), raw_len)
}

fn read_previous_blob_into_stage(plan: &KernelPlan, slot: usize, raw_len: usize) -> bool {
    read_blob_area_into_stage(plan, previous_blob_sector(slot), raw_len)
}

fn parse_mcaps(manifest: &str) -> Result<u32, &str> {
    let mut set = 0u32;
    for cap in dzp::manifest_list(manifest, "caps") {
        match MCAP_TABLE.iter().find(|(n, _)| *n == cap) {
            Some((_, bit)) => set |= bit,
            None => return Err("unknown capability in manifest"),
        }
    }
    Ok(set)
}

fn verify_payload(pkg: &dzp::Package<'_>) -> Result<(), &'static str> {
    match pkg.kind {
        dzp::KIND_DEZH_IR => ir::verify(pkg.payload).map_err(|t| t.msg()),
        dzp::KIND_ELF_RISCV64 => {
            let p = pkg.payload;
            if p.len() > 20 && &p[0..4] == b"\x7fELF" && get_u16(p, 18) == 243 {
                Ok(())
            } else {
                Err("payload is not a riscv64 ELF")
            }
        }
        _ => Err("unknown payload kind"),
    }
}

fn install_runtime_entry(
    slot: usize,
    name: &str,
    version: &str,
    kind: u16,
    mcaps: u32,
    raw_len: usize,
    raw_crc: u32,
    payload: &[u8],
) -> bool {
    let (arena_used, fits) = unsafe { (ARENA_USED, ARENA_USED + payload.len() <= ARENA_SIZE) };
    if !fits {
        return false;
    }
    unsafe {
        let base = core::ptr::addr_of_mut!(ARENA) as *mut u8;
        core::ptr::copy_nonoverlapping(payload.as_ptr(), base.add(arena_used), payload.len());
        let mut e = EMPTY_PKG;
        e.used = true;
        e.slot = slot as u8;
        e.name[..name.len()].copy_from_slice(name.as_bytes());
        e.name_len = name.len() as u8;
        e.version[..version.len()].copy_from_slice(version.as_bytes());
        e.version_len = version.len() as u8;
        e.kind = kind;
        e.mcaps = mcaps;
        e.raw_len = raw_len as u32;
        e.raw_crc = raw_crc;
        e.off = arena_used as u32;
        e.len = payload.len() as u32;
        PKGS[slot] = e;
        ARENA_USED = arena_used + payload.len();
    }
    true
}

fn encode_registry_entry(
    slot: usize,
    state: u8,
    name: &str,
    version: &str,
    kind: u16,
    mcaps: u32,
    raw_len: usize,
    raw_crc: u32,
) {
    let mut reg = unsafe { REGISTRY };
    let range = entry_range(slot);
    reg[range.clone()].fill(0);
    let e = range.start;
    reg[e] = state;
    reg[e + 1..e + 5].copy_from_slice(REG_MAGIC);
    reg[e + 5] = slot as u8;
    put_u16(&mut reg, e + 6, kind);
    put_u32(&mut reg, e + 8, mcaps);
    put_u32(&mut reg, e + 12, raw_len as u32);
    put_u32(&mut reg, e + 16, raw_crc);
    put_u32(&mut reg, e + 20, slot_blob_sector(slot) as u32);
    put_u32(&mut reg, e + 24, PKG_SLOT_SECTORS as u32);
    reg[e + 32] = name.len() as u8;
    reg[e + 33] = version.len() as u8;
    reg[e + 40..e + 40 + name.len()].copy_from_slice(name.as_bytes());
    reg[e + 40 + NAME_MAX..e + 40 + NAME_MAX + version.len()].copy_from_slice(version.as_bytes());
    unsafe { REGISTRY = reg };
}

fn encode_previous_metadata(
    slot: usize,
    version: &str,
    kind: u16,
    mcaps: u32,
    raw_len: usize,
    raw_crc: u32,
) {
    let mut reg = unsafe { REGISTRY };
    let e = slot * ENTRY_SIZE;
    let mut flags = get_u32(&reg, e + 28);
    flags |= ENTRY_FLAG_PREVIOUS_VALID;
    put_u32(&mut reg, e + 28, flags);
    reg[e + 80] = version.len() as u8;
    put_u16(&mut reg, e + 82, kind);
    put_u32(&mut reg, e + 84, mcaps);
    put_u32(&mut reg, e + 88, raw_len as u32);
    put_u32(&mut reg, e + 92, raw_crc);
    put_u32(&mut reg, e + 96, previous_blob_sector(slot) as u32);
    put_u32(&mut reg, e + 100, PKG_SLOT_SECTORS as u32);
    reg[e + 104..e + 104 + VER_MAX].fill(0);
    reg[e + 104..e + 104 + version.len()].copy_from_slice(version.as_bytes());
    unsafe { REGISTRY = reg };
}

fn cap_delta(old_caps: u32, new_caps: u32) -> (u32, u32, u32) {
    (
        new_caps & !old_caps,
        old_caps & !new_caps,
        old_caps & new_caps,
    )
}

fn set_entry_state(slot: usize, state: u8) {
    unsafe {
        REGISTRY[slot * ENTRY_SIZE] = state;
    }
}

fn clear_registry_slot(slot: usize) {
    unsafe {
        let range = entry_range(slot);
        REGISTRY[range].fill(0);
    }
}

fn registry_slot_for(name: &str) -> Option<usize> {
    let reg = unsafe { REGISTRY };
    let mut first_removed = None;
    let mut first_empty = None;
    let mut slot = 0usize;
    while slot < MAX_PKGS {
        let state = entry_state(&reg, slot);
        if state != STATE_EMPTY && entry_name(&reg, slot) == name {
            return Some(slot);
        }
        if state == STATE_REMOVED && first_removed.is_none() {
            first_removed = Some(slot);
        }
        if state == STATE_EMPTY && first_empty.is_none() {
            first_empty = Some(slot);
        }
        slot += 1;
    }
    first_removed.or(first_empty)
}

fn find_runtime_pkg(name: &str) -> Option<usize> {
    unsafe { (0..MAX_PKGS).find(|&i| PKGS[i].used && PKGS[i].name() == name) }
}

fn find_registry_slot(name: &str) -> Option<usize> {
    let reg = unsafe { REGISTRY };
    (0..MAX_PKGS)
        .find(|&slot| entry_state(&reg, slot) != STATE_EMPTY && entry_name(&reg, slot) == name)
}

fn mark_corrupt(plan: &KernelPlan, slot: usize) {
    unsafe {
        let e = slot * ENTRY_SIZE;
        if REGISTRY[e] == STATE_ACTIVE {
            REGISTRY[e] = STATE_CORRUPT;
            let _ = write_registry(plan);
        }
    }
}

fn verify_slot_blob(plan: &KernelPlan, rec: JournalRecord) -> bool {
    verify_blob_record_at(plan, slot_blob_sector(rec.slot), rec)
}

fn verify_stage_blob(plan: &KernelPlan, rec: JournalRecord) -> bool {
    verify_blob_record_at(plan, stage_blob_sector(rec.slot), rec)
}

fn verify_blob_record_at(plan: &KernelPlan, base_sector: usize, rec: JournalRecord) -> bool {
    if rec.raw_len == 0
        || rec.raw_len > PKG_MAX_RAW_BYTES
        || rec.blob_count != rec.raw_len.div_ceil(SECTOR_SIZE)
    {
        return false;
    }
    if !read_blob_area_into_stage(plan, base_sector, rec.raw_len) {
        return false;
    }
    let bytes = unsafe { &STAGE[..rec.raw_len] };
    if checksum(bytes) != rec.raw_crc {
        return false;
    }
    let Ok(pkg) = dzp::parse(bytes) else {
        return false;
    };
    if verify_payload(&pkg).is_err() {
        return false;
    }
    let Some(name) = dzp::manifest_str(pkg.manifest, "name") else {
        return false;
    };
    let version = dzp::manifest_str(pkg.manifest, "version").unwrap_or("0.0.0");
    let Ok(mcaps) = parse_mcaps(pkg.manifest) else {
        return false;
    };
    name == rec.name() && version == rec.version() && mcaps == rec.mcaps
}

fn copy_blob_area(plan: &KernelPlan, from_sector: usize, to_sector: usize, raw_len: usize) -> bool {
    if raw_len == 0 || raw_len > PKG_MAX_RAW_BYTES {
        return false;
    }
    if !read_blob_area_into_stage(plan, from_sector, raw_len) {
        return false;
    }
    let bytes = unsafe { &STAGE[..raw_len] };
    write_blob_at(plan, to_sector, bytes)
}

fn rollback_slot_to_old_state(rec: JournalRecord) {
    match rec.old_state {
        STATE_EMPTY => clear_registry_slot(rec.slot),
        state => set_entry_state(rec.slot, state),
    }
}

fn quarantine_pending_slots() {
    let mut slot = 0usize;
    while slot < MAX_PKGS {
        let reg = unsafe { REGISTRY };
        let state = entry_state(&reg, slot);
        if state == STATE_PENDING_INSTALL || state == STATE_PENDING_REMOVE {
            set_entry_state(slot, STATE_QUARANTINED);
        }
        slot += 1;
    }
}

fn recover_from_journal(plan: &KernelPlan, manual: bool) -> bool {
    let mut raw = [0u8; JOURNAL_SIZE];
    if !read_journal_raw(plan, &mut raw) {
        kprintln!("[pkg-recover] service unavailable: cannot read journal");
        set_degraded(true);
        return false;
    }
    match decode_journal(&raw) {
        JournalState::Empty => {
            set_degraded(false);
            true
        }
        JournalState::Corrupt(reason) => {
            if !manual {
                kprintln!(
                    "[pkg-recover] journal corrupt ({reason}); store degraded, package run blocked"
                );
                set_degraded(true);
                crate::record_event("installer", "pkg.recover", "journal", "CORRUPT");
                return false;
            }
            kprintln!("[pkg-recover] journal corrupt ({reason}); quarantining pending slots and clearing journal");
            quarantine_pending_slots();
            if !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
                set_degraded(true);
                return false;
            }
            set_degraded(false);
            invalidate_loaded();
            crate::record_event("installer", "pkg.recover", "journal", "QUARANTINED");
            true
        }
        JournalState::Valid(rec) => {
            print_journal_record("[pkg-recover] found", rec);
            let mut ok = true;
            match rec.op {
                JOURNAL_OP_INSTALL | JOURNAL_OP_REPLACE => match rec.phase {
                    JOURNAL_PHASE_STARTED | JOURNAL_PHASE_BLOB_WRITTEN => {
                        rollback_slot_to_old_state(rec);
                        kprintln!(
                            "[pkg-recover] rolled back incomplete install for '{}'",
                            rec.name()
                        );
                        crate::record_event("installer", "pkg.recover", "package", "ROLLED_BACK");
                    }
                    JOURNAL_PHASE_REGISTRY_PENDING => {
                        let reg = unsafe { REGISTRY };
                        let state = entry_state(&reg, rec.slot);
                        if rec.op == JOURNAL_OP_REPLACE
                            && state == STATE_PENDING_INSTALL
                            && verify_stage_blob(plan, rec)
                            && copy_blob_area(
                                plan,
                                stage_blob_sector(rec.slot),
                                slot_blob_sector(rec.slot),
                                rec.raw_len,
                            )
                        {
                            set_entry_state(rec.slot, STATE_ACTIVE);
                            kprintln!(
                                "[pkg-recover] committed verified pending update '{}'",
                                rec.name()
                            );
                            crate::record_event("installer", "pkg.recover", "package", "COMMITTED");
                        } else if rec.op == JOURNAL_OP_INSTALL
                            && state == STATE_PENDING_INSTALL
                            && verify_slot_blob(plan, rec)
                        {
                            set_entry_state(rec.slot, STATE_ACTIVE);
                            kprintln!(
                                "[pkg-recover] committed verified pending install '{}'",
                                rec.name()
                            );
                            crate::record_event("installer", "pkg.recover", "package", "COMMITTED");
                        } else {
                            set_entry_state(rec.slot, STATE_QUARANTINED);
                            kprintln!(
                                "[pkg-recover] quarantined suspicious pending install '{}'",
                                rec.name()
                            );
                            crate::record_event(
                                "installer",
                                "pkg.recover",
                                "package",
                                "QUARANTINED",
                            );
                        }
                    }
                    _ => {
                        set_degraded(true);
                        ok = false;
                    }
                },
                JOURNAL_OP_ROLLBACK => {
                    if rec.phase == JOURNAL_PHASE_REGISTRY_PENDING
                        && read_previous_blob_into_stage(plan, rec.slot, rec.raw_len)
                        && checksum(unsafe { &STAGE[..rec.raw_len] }) == rec.raw_crc
                        && write_blob_at(plan, slot_blob_sector(rec.slot), unsafe {
                            &STAGE[..rec.raw_len]
                        })
                    {
                        set_entry_state(rec.slot, STATE_ACTIVE);
                        kprintln!(
                            "[pkg-recover] completed interrupted rollback for '{}'",
                            rec.name()
                        );
                        crate::record_event("installer", "pkg.recover", "package", "ROLLED_BACK");
                    } else {
                        set_degraded(true);
                        ok = false;
                    }
                }
                JOURNAL_OP_REMOVE => {
                    set_entry_state(rec.slot, STATE_REMOVED);
                    kprintln!(
                        "[pkg-recover] completed interrupted remove for '{}'",
                        rec.name()
                    );
                    crate::record_event("installer", "pkg.recover", "package", "REMOVED");
                }
                _ => {
                    set_degraded(true);
                    ok = false;
                }
            }
            if !ok || !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
                kprintln!("[pkg-recover] recovery failed while committing store metadata");
                set_degraded(true);
                return false;
            }
            set_degraded(false);
            invalidate_loaded();
            true
        }
    }
}

fn load_slot(plan: &KernelPlan, slot: usize) {
    let reg = unsafe { REGISTRY };
    let e = slot * ENTRY_SIZE;
    let state = reg[e];
    if state != STATE_ACTIVE {
        return;
    }
    if &reg[e + 1..e + 5] != REG_MAGIC {
        mark_corrupt(plan, slot);
        return;
    }
    let raw_len = get_u32(&reg, e + 12) as usize;
    let raw_crc = get_u32(&reg, e + 16);
    if raw_len == 0 || raw_len > PKG_MAX_RAW_BYTES {
        mark_corrupt(plan, slot);
        return;
    }
    if !read_blob_into_stage(plan, slot, raw_len) {
        mark_corrupt(plan, slot);
        return;
    }
    let bytes = unsafe { &STAGE[..raw_len] };
    if checksum(bytes) != raw_crc {
        mark_corrupt(plan, slot);
        return;
    }
    let Ok(pkg) = dzp::parse(bytes) else {
        mark_corrupt(plan, slot);
        return;
    };
    if verify_payload(&pkg).is_err() {
        mark_corrupt(plan, slot);
        return;
    }
    let Some(name) = dzp::manifest_str(pkg.manifest, "name") else {
        mark_corrupt(plan, slot);
        return;
    };
    let version = dzp::manifest_str(pkg.manifest, "version").unwrap_or("0.0.0");
    let Ok(mcaps) = parse_mcaps(pkg.manifest) else {
        mark_corrupt(plan, slot);
        return;
    };
    if name.len() > NAME_MAX || version.len() > VER_MAX {
        mark_corrupt(plan, slot);
        return;
    }
    if !install_runtime_entry(
        slot,
        name,
        version,
        pkg.kind,
        mcaps,
        raw_len,
        raw_crc,
        pkg.payload,
    ) {
        mark_corrupt(plan, slot);
    }
}

fn ensure_loaded(plan: &KernelPlan) -> bool {
    unsafe {
        if STORE_LOADED && !STORE_DEGRADED {
            return true;
        }
    }
    if !read_registry(plan) {
        kprintln!("[pkg-store] service unavailable; package registry not loaded");
        return false;
    }
    if !recover_from_journal(plan, false) {
        return false;
    }
    if !read_registry(plan) {
        kprintln!("[pkg-store] service unavailable; package registry not loaded after recovery");
        return false;
    }
    clear_runtime_registry();
    let mut slot = 0usize;
    while slot < MAX_PKGS {
        load_slot(plan, slot);
        slot += 1;
    }
    unsafe { STORE_LOADED = true };
    true
}

// --- Raw (no-echo) line reader for the upload protocol --------------------------

fn read_raw_line(buf: &mut [u8]) -> usize {
    use core::sync::atomic::Ordering;
    let mut len = 0usize;
    loop {
        let c = crate::Uart.getc();
        match c {
            b'\n' => {
                if crate::SKIP_LF_AFTER_CR.swap(false, Ordering::Relaxed) {
                    continue;
                }
                return len;
            }
            b'\r' => {
                crate::SKIP_LF_AFTER_CR.store(true, Ordering::Relaxed);
                return len;
            }
            c if c.is_ascii_graphic() && len < buf.len() => {
                crate::SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
                buf[len] = c;
                len += 1;
            }
            _ => {
                crate::SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
            }
        }
    }
}

fn receive_raw_into_stage(label: &str) -> Option<usize> {
    kprintln!("[{label}] ready: send base64 lines; end with '.', abort with '!'");
    let mut staged = 0usize;
    let mut line = [0u8; 120];
    loop {
        let n = read_raw_line(&mut line);
        let text = &line[..n];
        if text == b"." {
            break;
        }
        if text == b"!" {
            kprintln!("[{label}] aborted by sender");
            return None;
        }
        if text.is_empty() {
            continue;
        }
        let out = unsafe { &mut STAGE[staged..] };
        match b64::decode(text, out) {
            Some(k) => {
                staged += k;
                kprintln!("+ok {staged}");
            }
            None => {
                kprintln!("+err base64 decode failed or package exceeds {STAGE_SIZE} bytes");
                return None;
            }
        }
    }
    Some(staged)
}

// --- Install -------------------------------------------------------------------

pub(crate) fn pkg_recv(plan: &KernelPlan) {
    if !ensure_loaded(plan) {
        kprintln!("[pkg-recv] rejected: package store unavailable or degraded");
        return;
    }

    let Some(staged) = receive_raw_into_stage("pkg-recv") else {
        return;
    };

    if staged == 0 || staged > PKG_MAX_RAW_BYTES {
        kprintln!("[pkg-recv] rejected: package exceeds 32 KiB v0 slot limit");
        return;
    }

    let bytes = unsafe { &STAGE[..staged] };
    let pkg = match dzp::parse(bytes) {
        Ok(p) => p,
        Err(e) => {
            kprintln!("[pkg-recv] rejected: {}", e.msg());
            return;
        }
    };

    let Some(name) = dzp::manifest_str(pkg.manifest, "name") else {
        kprintln!("[pkg-recv] rejected: manifest has no name");
        return;
    };
    let version = dzp::manifest_str(pkg.manifest, "version").unwrap_or("0.0.0");
    if name.is_empty() || name.len() > NAME_MAX || version.len() > VER_MAX {
        kprintln!("[pkg-recv] rejected: name/version length out of range");
        return;
    }
    let mcaps = match parse_mcaps(pkg.manifest) {
        Ok(m) => m,
        Err(e) => {
            kprintln!("[pkg-recv] rejected: {e} (known: print ipc uptime cairn-read cairn-write)");
            return;
        }
    };
    if let Err(e) = verify_payload(&pkg) {
        kprintln!("[pkg-recv] rejected: {e}");
        return;
    }

    let raw_crc = checksum(bytes);
    let Some(slot) = registry_slot_for(name) else {
        kprintln!("[pkg-recv] rejected: registry full ({MAX_PKGS} packages)");
        return;
    };
    let reg = unsafe { REGISTRY };
    let old_state = entry_state(&reg, slot);
    if old_state == STATE_CORRUPT || old_state == STATE_QUARANTINED {
        kprintln!(
            "[pkg-recv] rejected: slot for '{name}' is {}; run pkg-verify/pkg-recover first",
            state_name(old_state)
        );
        return;
    }
    if old_state == STATE_ACTIVE
        && entry_version(&reg, slot) == version
        && get_u32(&reg, slot * ENTRY_SIZE + 16) == raw_crc
    {
        kprintln!("[pkg-recv] already installed '{name}' {version} with matching checksum");
        return;
    }
    if old_state == STATE_ACTIVE {
        kprintln!(
            "[pkg-recv] rejected: '{name}' is already Active; use pkg-update for explicit review"
        );
        return;
    }

    let before_crc = checksum(&reg);
    let op = JOURNAL_OP_INSTALL;
    let mut rec = journal_record(
        op,
        JOURNAL_PHASE_STARTED,
        slot,
        old_state,
        STATE_ACTIVE,
        name,
        version,
        mcaps,
        staged,
        raw_crc,
        before_crc,
        0,
    );
    if !write_journal(plan, rec) {
        kprintln!("[pkg-recv] rejected: journal write failed before install");
        return;
    }
    crate::record_event("installer", "pkg.tx.start", "package", "OK");

    if !write_blob(plan, slot, bytes) {
        kprintln!("[pkg-recv] rejected: package blob write failed");
        return;
    }
    rec.phase = JOURNAL_PHASE_BLOB_WRITTEN;
    if !write_journal(plan, rec) {
        kprintln!("[pkg-recv] rejected: journal write failed after blob");
        return;
    }
    if !verify_slot_blob(plan, rec) {
        kprintln!("[pkg-recv] rejected: package blob read-back verify failed");
        return;
    }
    crate::record_event("installer", "pkg.blob.verify", "package", "OK");

    encode_registry_entry(
        slot,
        STATE_PENDING_INSTALL,
        name,
        version,
        pkg.kind,
        mcaps,
        staged,
        raw_crc,
    );
    let pending_crc = {
        let reg = unsafe { REGISTRY };
        checksum(&reg)
    };
    rec.phase = JOURNAL_PHASE_REGISTRY_PENDING;
    rec.registry_after_crc = pending_crc;
    if !write_journal(plan, rec) || !write_registry(plan) {
        kprintln!("[pkg-recv] rejected: pending registry commit failed");
        return;
    }
    crate::record_event("installer", "pkg.registry.pending", "package", "OK");

    set_entry_state(slot, STATE_ACTIVE);
    if !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
        kprintln!("[pkg-recv] rejected: package registry commit failed");
        return;
    }
    crate::record_event("installer", "pkg.tx.commit", "package", "OK");

    unsafe {
        PKGS[slot] = EMPTY_PKG;
    }
    if !install_runtime_entry(
        slot,
        name,
        version,
        pkg.kind,
        mcaps,
        staged,
        raw_crc,
        pkg.payload,
    ) {
        invalidate_loaded();
        kprintln!("[pkg-recv] installed on disk, but runtime arena is full; reboot and retry");
        return;
    }
    invalidate_loaded();

    kprintln!(
        "[pkg] installed '{name}' {version} kind={} payload={} bytes persistent_slot={slot} state=Active",
        dzp::kind_name(pkg.kind),
        pkg.payload.len()
    );
    kprint!("[pkg] grants recorded at install time: ");
    mcap_names(mcaps, &mut crate::Uart);
    kprintln!(" (kernel-enforced at run time; persisted on disk)");
}

// --- Run -----------------------------------------------------------------------

fn ir_caps_from(mcaps: u32) -> u32 {
    let mut c = 0u32;
    if mcaps & MCAP_PRINT != 0 {
        c |= ir::CAP_PRINT;
    }
    if mcaps & MCAP_CAIRN_READ != 0 {
        c |= ir::CAP_READ;
    }
    if mcaps & MCAP_CAIRN_WRITE != 0 {
        c |= ir::CAP_WRITE;
    }
    c
}

fn task_caps_from(mcaps: u32, name: &str) -> usize {
    let mut c = 0usize;
    if mcaps & MCAP_PRINT != 0 {
        c |= crate::TASK_PRINT;
    }
    if mcaps & MCAP_IPC != 0 {
        c |= crate::TASK_IPC;
    }
    if mcaps & MCAP_UPTIME != 0 {
        c |= crate::TASK_TIME;
    }
    // A manifest cairn grant maps to the app's OWN namespace bit only — an app
    // can never name another app's namespace in its manifest.
    if mcaps & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) != 0 {
        if let Some(ns) = crate::cairn_ns_id(name) {
            c |= crate::task_ns_cap(ns);
        }
    }
    c
}

/// The Cairn v1 namespace an installed app may use: its own, by name.
fn app_cairn_ns(mcaps: u32, name: &str) -> Option<usize> {
    if mcaps & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) == 0 {
        return None;
    }
    let ns = crate::cairn_ns_id(name);
    if ns.is_none() {
        kprintln!(
            "[pkg-run] note: '{name}' requests cairn caps but has no v1 namespace (fixed table: note/lab/calc/vault/agent)"
        );
    }
    ns
}

pub(crate) fn pkg_run(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-run] package store unavailable or degraded");
        crate::record_event("kernel", "pkg.run", "package", "DENIED");
        return;
    }
    let Some(i) = find_runtime_pkg(name) else {
        if let Some(slot) = find_registry_slot(name) {
            let reg = unsafe { REGISTRY };
            let state = entry_state(&reg, slot);
            kprintln!(
                "[pkg-run] package '{name}' is {} and not runnable",
                state_name(state)
            );
        } else {
            kprintln!("[pkg-run] no installed package '{name}' (see pkg-list)");
        }
        crate::record_event("kernel", "pkg.run", "package", "DENIED");
        return;
    };
    run_loaded_entry(plan, i, unsafe { PKGS[i].mcaps }, 0, "pkg-run");
}

/// Run an already-loaded package slot with an EFFECTIVE capability set that may
/// be narrower than the package's installed grants. `pkg-run` passes the
/// installed grants unchanged; `intent-run` passes a set already reduced to an
/// Ahd ceiling (see `intent_run`). Everything downstream — the IR host caps,
/// the Cairn namespace binding, and the U-mode task caps — is derived from
/// `eff_mcaps`, so authority never exceeds what the caller supplies here.
fn run_loaded_entry(plan: &KernelPlan, i: usize, eff_mcaps: u32, ahd_id: u16, label: &str) {
    let entry = unsafe { PKGS[i] };
    kprint!(
        "[{label}] '{}' {} kind={} caps=",
        entry.name(),
        entry.version(),
        dzp::kind_name(entry.kind)
    );
    mcap_names(eff_mcaps, &mut crate::Uart);
    kprintln!();
    crate::record_event("installer", "pkg.run", "package", "start");
    match entry.kind {
        dzp::KIND_DEZH_IR => {
            // Effects this package makes carry its intent (Ahd) and derived cap
            // into the Sand ledger. `ahd_id == 0` (the pkg-run path) records a
            // direct effect; `intent-run` supplies the real Ahd id.
            let mut host = crate::KHost {
                caps: ir_caps_from(eff_mcaps),
                cairn: app_cairn_ns(eff_mcaps, entry.name()).map(|ns| (plan, ns)),
                intent: ahd_id,
                derived: eff_mcaps,
            };
            match ir::run(entry.payload(), &mut host) {
                Ok(()) => kprintln!("[{label}] '{}' finished", entry.name()),
                Err(t) => {
                    if t == ir::Trap::MissingCapability {
                        kprintln!(
                            "[{label}] DENIED by kernel: {} (grant it in app.toml caps=[...])",
                            t.msg()
                        );
                        crate::record_event("kernel", "pkg.run", "package", "DENIED");
                    } else {
                        kprintln!("[{label}] TRAP: {}", t.msg());
                        crate::record_event("kernel", "pkg.run", "package", "TRAP");
                    }
                    return;
                }
            }
        }
        dzp::KIND_ELF_RISCV64 => {
            kprintln!("[{label}] launching as U-mode process (own address space)");
            crate::run_foreground_processes(&[crate::ProcessSpec::new(
                entry.payload(),
                task_caps_from(eff_mcaps, entry.name()),
                0,
            )]);
            kprintln!("[{label}] '{}' exited; back in the console", entry.name());
        }
        _ => kprintln!("[{label}] unknown payload kind"),
    }
    crate::record_event("installer", "pkg.run", "package", "OK");
}

// --- Ahd: intent as the authority-derivation mechanism (W8, D020/D021) --------
//
// An `Ahd` is a declared capability CEILING. Running a package under an Ahd
// derives the effective capability set as `requested & ceiling`, so the derived
// authority is provably a SUBSET of the Ahd — this is structural (a bitwise
// AND), not a purpose string. Any capability the package requests beyond the
// Ahd is dropped and reported: authority only ever flows through a declared
// intent, and can never exceed it. Ahds are runtime authority sessions; they
// are intentionally not persisted (an intent is opened for a run).

const AHD_KINDS: &[(&str, u32)] = &[
    ("compute", MCAP_PRINT),
    ("reader", MCAP_PRINT | MCAP_CAIRN_READ),
    ("writer", MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE),
    ("ipc", MCAP_PRINT | MCAP_IPC),
    (
        "full",
        MCAP_PRINT | MCAP_IPC | MCAP_UPTIME | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE,
    ),
];

fn ahd_kind(kind: &str) -> Option<(&'static str, u32)> {
    AHD_KINDS
        .iter()
        .find(|(n, _)| *n == kind)
        .map(|(n, c)| (*n, *c))
}

fn print_ahd_kinds() {
    kprint!("  known intent kinds:");
    for (n, _) in AHD_KINDS {
        kprint!(" {n}");
    }
    kprintln!();
}

// Intent sessions are runtime-only (an Ahd is opened for a run, never persisted).
// The cap is generous so a long console session / flagship narrative that opens
// several intents in a row does not run out; ids keep incrementing regardless.
const MAX_AHD: usize = 16;

/// A lease value meaning "no use limit" (the default for demos that drive a
/// mission themselves). A finite lease bounds how many runs an intent authorizes
/// before it auto-revokes — coarse revocation for long-lived agents.
const AHD_UNLIMITED: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct AhdSlot {
    used: bool,
    id: u16,
    kind: &'static str,
    ceiling: u32,
    /// Remaining runs this intent authorizes (`AHD_UNLIMITED` = no limit). Each
    /// `intent-run` consumes one; reaching zero auto-revokes the intent.
    lease: u32,
    /// Once revoked (explicitly or by an exhausted lease) the intent authorizes
    /// nothing further. Effects it already produced keep their provenance —
    /// revoking authority is not erasing history.
    revoked: bool,
}

const EMPTY_AHD: AhdSlot = AhdSlot {
    used: false,
    id: 0,
    kind: "",
    ceiling: 0,
    lease: AHD_UNLIMITED,
    revoked: false,
};

static mut AHDS: [AhdSlot; MAX_AHD] = [EMPTY_AHD; MAX_AHD];
static mut AHD_NEXT_ID: u16 = 1;

/// Allocate an Ahd (intent) slot for `kind`, returning its id, canonical name,
/// and capability ceiling. Shared by the `intent-open` console command and the
/// self-contained `sand-demo` so both derive authority through the same path.
fn open_ahd(kind: &str, lease: u32) -> Option<(u16, &'static str, u32)> {
    let (kname, ceiling) = ahd_kind(kind)?;
    let slot = unsafe { (0..MAX_AHD).find(|&i| !AHDS[i].used) }?;
    let id = unsafe { AHD_NEXT_ID };
    unsafe {
        AHD_NEXT_ID = AHD_NEXT_ID.wrapping_add(1);
        AHDS[slot] = AhdSlot {
            used: true,
            id,
            kind: kname,
            ceiling,
            lease,
            revoked: false,
        };
    }
    Some((id, kname, ceiling))
}

/// Open an Ahd and return its id + ceiling, for callers that drive a whole
/// mission (e.g. the console `sfar-demo`). Thin wrapper over [`open_ahd`] with no
/// use limit (the caller runs the mission itself).
pub(crate) fn open_intent(kind: &str) -> Option<(u16, u32)> {
    open_ahd(kind, AHD_UNLIMITED).map(|(id, _, ceiling)| (id, ceiling))
}

pub(crate) fn intent_open(arg: &str) {
    // `intent-open <kind> [lease]` — an optional lease bounds how many runs the
    // intent authorizes before it auto-revokes (omit for no limit).
    let mut parts = arg.trim().split_whitespace();
    let Some(kind) = parts.next() else {
        kprintln!("usage: intent-open <kind> [lease]");
        print_ahd_kinds();
        return;
    };
    let lease = match parts.next() {
        Some(n) => match n.parse::<u32>() {
            Ok(v) => v,
            Err(_) => {
                kprintln!("[intent-open] bad lease '{n}' (a run count, or omit for unlimited)");
                return;
            }
        },
        None => AHD_UNLIMITED,
    };
    if ahd_kind(kind).is_none() {
        kprintln!("[intent-open] unknown intent kind '{kind}'");
        print_ahd_kinds();
        return;
    }
    let Some((id, kname, ceiling)) = open_ahd(kind, lease) else {
        kprintln!("[intent-open] no free Ahd slots (max {MAX_AHD})");
        return;
    };
    kprint!("[intent-open] opened Ahd #{id} kind={kname} ceiling=");
    mcap_names(ceiling, &mut crate::Uart);
    kprintln!();
    kprintln!("  authority derived under it is proven <= this ceiling");
    if lease == AHD_UNLIMITED {
        kprintln!("  lease=unlimited (revoke explicitly with intent-revoke {id})");
    } else {
        kprintln!("  lease={lease} run(s) then auto-revoked (or revoke now: intent-revoke {id})");
    }
    kprintln!("  run a package under it with: intent-run {id} <app>");
    crate::record_event("intent", "intent.open", "ahd", "OK");
}

pub(crate) fn intent_list() {
    kprintln!("open Ahds (intent tokens):");
    let mut any = false;
    for i in 0..MAX_AHD {
        let a = unsafe { AHDS[i] };
        if !a.used {
            continue;
        }
        any = true;
        kprint!("  Ahd #{} kind={} ceiling=", a.id, a.kind);
        mcap_names(a.ceiling, &mut crate::Uart);
        if a.revoked {
            kprint!(" lease=- status=REVOKED");
        } else if a.lease == AHD_UNLIMITED {
            kprint!(" lease=unlimited status=live");
        } else {
            kprint!(" lease={} status=live", a.lease);
        }
        kprintln!();
    }
    if !any {
        kprintln!("  (none open - open one with `intent-open <kind>`)");
        print_ahd_kinds();
    }
}

fn ahd_slot_index(id: u16) -> Option<usize> {
    (0..MAX_AHD).find(|&i| {
        let a = unsafe { AHDS[i] };
        a.used && a.id == id
    })
}

/// W8: revoke an intent. Any authority derived under it stops being grantable at
/// the next `intent-run`; effects it already produced keep their provenance on
/// the ledger (revoking authority is not erasing history — `tbar`/`sfar` still
/// resolve the mission).
pub(crate) fn intent_revoke(arg: &str) {
    let Ok(id) = arg.trim().parse::<u16>() else {
        kprintln!("usage: intent-revoke <ahd-id> (see intent-list)");
        return;
    };
    let Some(slot) = ahd_slot_index(id) else {
        kprintln!("[intent-revoke] no open Ahd #{id}");
        crate::record_event("intent", "intent.revoke", "ahd", "DENIED");
        return;
    };
    if unsafe { AHDS[slot].revoked } {
        kprintln!("[intent-revoke] Ahd #{id} is already revoked");
        return;
    }
    unsafe {
        AHDS[slot].revoked = true;
        AHDS[slot].lease = 0;
    }
    kprintln!("[intent-revoke] Ahd #{id} REVOKED; it authorizes nothing further");
    kprintln!("  past effects keep their provenance: tbar {id} / sfar-plan {id} still resolve");
    crate::record_event("intent", "intent.revoke", "ahd", "OK");
}

/// Outcome of trying to use an intent once, applying the revoke/lease gate.
enum LeaseUse {
    Authorized(u32),
    DeniedRevoked,
    DeniedExhausted,
}

/// Apply the same revoke/lease gate `intent-run` uses and consume one use. A
/// finite lease that reaches zero auto-revokes. Shared by the self-contained
/// `lease-demo` so it exercises the real gate, not a copy.
fn lease_use(id: u16) -> LeaseUse {
    let slot = ahd_slot_index(id).expect("lease_use on an open intent");
    let (revoked, lease) = unsafe { (AHDS[slot].revoked, AHDS[slot].lease) };
    if revoked {
        return LeaseUse::DeniedRevoked;
    }
    if lease == 0 {
        return LeaseUse::DeniedExhausted;
    }
    if lease != AHD_UNLIMITED {
        let left = lease - 1;
        unsafe {
            AHDS[slot].lease = left;
            if left == 0 {
                AHDS[slot].revoked = true;
            }
        }
        return LeaseUse::Authorized(left);
    }
    LeaseUse::Authorized(AHD_UNLIMITED)
}

/// W8: self-contained proof of leases + revocation. A lease of one authorizes
/// exactly one run and then auto-revokes; an explicitly revoked intent
/// authorizes nothing — while the effects either produced keep their provenance
/// (authority is bounded/withdrawn without rewriting history).
pub(crate) fn lease_demo() {
    kprintln!("[lease-demo] an intent can be LEASED (bounded runs) or REVOKED; authority is withdrawn, provenance is not");
    let Some((a, _k, _c)) = open_ahd("compute", 1) else {
        kprintln!("[lease-demo] FAIL: no free Ahd slot");
        crate::record_event("intent", "lease.demo", "ahd", "fail");
        return;
    };
    kprintln!("[lease-demo] 1/3 opened Ahd#{a} with lease=1 (authorizes exactly one run)");
    let use1 = lease_use(a);
    match use1 {
        LeaseUse::Authorized(left) => {
            kprintln!("[lease-demo]   use #1 -> AUTHORIZED (lease now {left}; auto-revoked at 0)")
        }
        _ => kprintln!("[lease-demo]   use #1 -> unexpectedly denied"),
    }
    let use2 = lease_use(a);
    match use2 {
        LeaseUse::DeniedExhausted | LeaseUse::DeniedRevoked => {
            kprintln!("[lease-demo]   use #2 -> DENIED (lease exhausted, intent auto-revoked)")
        }
        LeaseUse::Authorized(_) => kprintln!("[lease-demo]   use #2 -> unexpectedly authorized"),
    }
    let Some((b, _k, _c)) = open_ahd("compute", AHD_UNLIMITED) else {
        kprintln!("[lease-demo] FAIL: no free Ahd slot");
        return;
    };
    kprintln!("[lease-demo] 2/3 opened Ahd#{b} (unlimited) then revoke it explicitly");
    if let Some(slot) = ahd_slot_index(b) {
        unsafe {
            AHDS[slot].revoked = true;
            AHDS[slot].lease = 0;
        }
    }
    let use3 = lease_use(b);
    let use3_denied = matches!(use3, LeaseUse::DeniedRevoked | LeaseUse::DeniedExhausted);
    if use3_denied {
        kprintln!("[lease-demo]   use after revoke -> DENIED (revoked intents authorize nothing)");
    } else {
        kprintln!("[lease-demo]   use after revoke -> unexpectedly authorized");
    }
    let pass = matches!(use1, LeaseUse::Authorized(_))
        && matches!(use2, LeaseUse::DeniedExhausted | LeaseUse::DeniedRevoked)
        && use3_denied;
    kprintln!("[lease-demo] 3/3 note: effects made under a now-revoked intent still resolve on the ledger (tbar/sfar) - provenance outlives authority");
    crate::record_event("intent", "lease.demo", "ahd", if pass { "pass" } else { "fail" });
    if pass {
        kprintln!("[lease-demo] PASS: a lease bounds authority to N runs; revoke withdraws it immediately; neither erases provenance");
    } else {
        kprintln!("[lease-demo] FAIL: lease/revoke gate did not behave as expected");
    }
}

pub(crate) fn intent_run(plan: &KernelPlan, arg: &str) {
    let (id_str, name) = match arg.trim().split_once(' ') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => {
            kprintln!("usage: intent-run <ahd-id> <app>");
            return;
        }
    };
    let Ok(id) = id_str.parse::<u16>() else {
        kprintln!("[intent-run] bad Ahd id '{id_str}'");
        return;
    };
    let Some(slot) = ahd_slot_index(id) else {
        kprintln!("[intent-run] no open Ahd #{id} (see intent-list; open with intent-open)");
        crate::record_event("intent", "intent.run", "ahd", "DENIED");
        return;
    };
    let (ceiling, revoked, lease) =
        unsafe { (AHDS[slot].ceiling, AHDS[slot].revoked, AHDS[slot].lease) };
    if revoked {
        kprintln!("[intent-run] DENIED: Ahd #{id} is REVOKED; it authorizes nothing (re-open a fresh intent)");
        crate::record_event("intent", "intent.run", "ahd", "DENIED");
        return;
    }
    if lease == 0 {
        kprintln!("[intent-run] DENIED: Ahd #{id} lease is exhausted (auto-revoked)");
        crate::record_event("intent", "intent.run", "ahd", "DENIED");
        return;
    }
    if !ensure_loaded(plan) {
        kprintln!("[intent-run] package store unavailable or degraded");
        return;
    }
    let Some(i) = find_runtime_pkg(name) else {
        kprintln!("[intent-run] no installed package '{name}' (see pkg-list)");
        crate::record_event("intent", "intent.run", "package", "DENIED");
        return;
    };
    let requested = unsafe { PKGS[i].mcaps };
    let derived = requested & ceiling;
    let beyond = requested & !ceiling;

    kprint!("[intent-run] Ahd #{id} ceiling=");
    mcap_names(ceiling, &mut crate::Uart);
    kprint!(" | '{name}' requests=");
    mcap_names(requested, &mut crate::Uart);
    kprintln!();
    if beyond != 0 {
        kprint!("[intent-run] beyond-intent DENIED (dropped): ");
        mcap_names(beyond, &mut crate::Uart);
        kprintln!(" -- authority cannot exceed the Ahd");
        crate::record_event("intent", "intent.derive", "cap", "DENIED");
    }
    kprint!("[intent-run] derived (proven subset of Ahd) = ");
    mcap_names(derived, &mut crate::Uart);
    kprintln!();
    run_loaded_entry(plan, i, derived, id, "intent-run");
    // Consume one lease use; a finite lease that reaches zero auto-revokes, so a
    // leased intent authorizes a bounded number of runs before re-authorization.
    if lease != AHD_UNLIMITED {
        let left = lease - 1;
        unsafe { AHDS[slot].lease = left };
        if left == 0 {
            unsafe { AHDS[slot].revoked = true };
            kprintln!("[intent-run] Ahd #{id} lease exhausted -> auto-REVOKED; further runs are denied");
            crate::record_event("intent", "intent.lease", "ahd", "EXPIRED");
        } else {
            kprintln!("[intent-run] Ahd #{id} lease remaining={left}");
        }
    }
}

/// Self-contained proof that authority only flows through an Ahd, using the
/// built-in Dezh-IR agent (no package upload needed). The SAME agent bytecode
/// (write+read+print) is run under two Ahds: a `writer` Ahd that contains its
/// intent (durable Cairn write succeeds), and a `compute` Ahd where the write
/// is beyond intent (dropped, then the kernel denies the ungranted hostcall).
pub(crate) fn intent_demo(plan: &KernelPlan) {
    kprintln!("[intent-demo] intent (Ahd) is the ONLY path to authority; derived cap <= Ahd");
    kprintln!("[intent-demo] same agent bytecode, two different Ahds:");
    let agent_requests = MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;
    intent_demo_run(plan, "writer", MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE, agent_requests);
    intent_demo_run(plan, "compute", MCAP_PRINT, agent_requests);
    kprintln!("[intent-demo] PASS: under 'writer' the write is in-intent; under 'compute' it is beyond-intent and denied");
}

fn intent_demo_run(plan: &KernelPlan, kind: &str, ceiling: u32, requested: u32) {
    let derived = requested & ceiling;
    let beyond = requested & !ceiling;
    kprint!("[intent-demo] --- Ahd kind={kind} ceiling=");
    mcap_names(ceiling, &mut crate::Uart);
    kprintln!();
    if beyond != 0 {
        kprint!("[intent-demo] beyond-intent DENIED (dropped): ");
        mcap_names(beyond, &mut crate::Uart);
        kprintln!();
    }
    let mut buf = [0u8; 512];
    let prog = ir::demo_cairn(&mut buf);
    // The Cairn binding follows the DERIVED caps: no write/read cap -> no ns.
    let cairn = if derived & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) != 0 {
        crate::cairn_ns_id("agent").map(|ns| (plan, ns))
    } else {
        None
    };
    let mut host = crate::KHost {
        caps: ir_caps_from(derived),
        cairn,
        intent: 0,
        derived,
    };
    match ir::run(prog, &mut host) {
        Ok(()) => kprintln!("[intent-demo] agent finished within intent"),
        Err(t) => {
            if t == ir::Trap::MissingCapability {
                kprintln!(
                    "[intent-demo] kernel DENIED an out-of-intent hostcall: {}",
                    t.msg()
                );
            } else {
                kprintln!("[intent-demo] agent trapped: {}", t.msg());
            }
        }
    }
}

/// Open a real `writer` Ahd, run the built-in agent under it, and let its Cairn
/// write become a Sand effect stamped with that intent. Returns the Ahd id (0 on
/// failure) so the caller can read the effect back off the ledger. This is the
/// intent -> derived cap -> effect half of the W8 P2 `sand-demo`; the ledger
/// read-back half lives in the console.
pub(crate) fn sand_demo_effect(plan: &KernelPlan) -> u16 {
    let Some((id, kname, ceiling)) = open_ahd("writer", AHD_UNLIMITED) else {
        kprintln!("[sand-demo] no free Ahd slots to open a writer intent");
        return 0;
    };
    kprint!("[sand-demo] opened Ahd #{id} kind={kname} ceiling=");
    mcap_names(ceiling, &mut crate::Uart);
    kprintln!();
    // The built-in agent requests print + cairn read + cairn write; under a
    // writer Ahd all three are in-intent, so derived == requested.
    let requested = MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;
    let derived = requested & ceiling;
    kprint!("[sand-demo] derived under intent (proven <= Ahd) = ");
    mcap_names(derived, &mut crate::Uart);
    kprintln!();
    kprintln!("[sand-demo] running the built-in agent; its Cairn write becomes an accountable effect");
    let mut buf = [0u8; 512];
    let prog = ir::demo_cairn(&mut buf);
    let mut host = crate::KHost {
        caps: ir_caps_from(derived),
        cairn: crate::cairn_ns_id("agent").map(|ns| (plan, ns)),
        intent: id,
        derived,
    };
    match ir::run(prog, &mut host) {
        Ok(()) => {
            crate::record_event("intent", "sand.effect", "ns:agent", "OK");
            id
        }
        Err(t) => {
            kprintln!("[sand-demo] the agent trapped before recording an effect: {}", t.msg());
            crate::record_event("intent", "sand.effect", "ns:agent", "TRAP");
            0
        }
    }
}

// --- Package signing: trust store + verification (see docs/PACKAGE_SIGNING.md) -
//
// The trust store is root-anchored publisher keys, each with a capability
// CEILING and a revocation flag. Install verifies the signature, requires a
// trusted (non-revoked) signer, and attenuates the granted authority to the
// signer's ceiling — the W8 `derived ⊆ intent` rule at the supply-chain layer.

#[derive(Clone, Copy)]
struct TrustedKey {
    used: bool,
    pk: [u8; 32],
    ceiling: u32,
    revoked: bool,
    label: &'static str,
}

const EMPTY_KEY: TrustedKey = TrustedKey {
    used: false,
    pk: [0u8; 32],
    ceiling: 0,
    revoked: false,
    label: "",
};

const MAX_TRUSTED_KEYS: usize = 4;
static mut TRUST_STORE: [TrustedKey; MAX_TRUSTED_KEYS] = [EMPTY_KEY; MAX_TRUSTED_KEYS];
static mut TRUST_INIT: bool = false;

fn trust_init() {
    unsafe {
        if TRUST_INIT {
            return;
        }
        // The demo publisher is trusted for a WRITER ceiling (print + cairn),
        // NOT ipc — so the demo package's requested ipc is dropped at install by
        // publisher attenuation. A real deployment would load this store signed
        // by an offline root key.
        TRUST_STORE[0] = TrustedKey {
            used: true,
            pk: DEMO_SIGNER_PK,
            ceiling: MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE,
            revoked: false,
            label: "demo-publisher",
        };
        TRUST_INIT = true;
    }
}

fn trust_lookup(pk: &[u8; 32]) -> Option<usize> {
    unsafe { (0..MAX_TRUSTED_KEYS).find(|&i| TRUST_STORE[i].used && TRUST_STORE[i].pk == *pk) }
}

fn print_keyid(pk: &[u8; 32]) {
    for b in &pk[..8] {
        kprint!("{:02x}", b);
    }
}

/// Verify a signed envelope's Ed25519 signature over `inner || context`,
/// assembled in a scratch buffer. The signer's public key, the signature, and
/// the counter all come from the envelope; the trust decision is separate.
fn verify_envelope_sig(env: &[u8], e: &sig::SignedEnvelope) -> bool {
    if e.inner_offset + e.inner_len > env.len() {
        return false;
    }
    let inner = &env[e.inner_offset..e.inner_offset + e.inner_len];
    let mut buf = [0u8; 2048];
    if inner.len() + sig::SIG_CONTEXT_LEN > buf.len() {
        return false;
    }
    buf[..inner.len()].copy_from_slice(inner);
    let cn = match sig::signed_context(&mut buf[inner.len()..], e.counter) {
        Some(n) => n,
        None => return false,
    };
    sig::verify(&e.signer_pk, &e.signature, &buf[..inner.len() + cn])
}

/// W8 (signing): the whole capability-native signing story in one self-contained
/// proof — verify a build-time-signed package, require a trusted non-revoked
/// signer, attenuate the granted authority to the publisher's ceiling, record
/// the install as a ledgered effect, then show a tampered package and a revoked
/// key are both refused.
pub(crate) fn sig_demo(plan: &KernelPlan) {
    const LAB: usize = 1;
    trust_init();
    kprintln!("[sig-demo] a signed package: verify -> trusted signer -> publisher attenuation -> ledgered install");
    let env = DEMO_SIGNED_PKG;

    if !sig::is_signed(env) {
        kprintln!("[sig-demo] FAIL: demo package is not a signed envelope");
        crate::record_event("installer", "sig.demo", "package", "fail");
        return;
    }
    let e = match sig::parse_envelope(env) {
        Ok(e) => e,
        Err(er) => {
            kprintln!("[sig-demo] FAIL: {}", er.msg());
            crate::record_event("installer", "sig.demo", "package", "fail");
            return;
        }
    };
    kprint!("[sig-demo] 1/5 signer key id=");
    print_keyid(&e.signer_pk);
    kprintln!(" counter={}", e.counter);

    // Trust decision (root-anchored trust store) is separate from crypto.
    let Some(ki) = trust_lookup(&e.signer_pk) else {
        kprintln!("[sig-demo]   UNTRUSTED signer -> install REFUSED");
        crate::record_event("installer", "sig.demo", "package", "fail");
        return;
    };
    let (ceiling, label) = unsafe { (TRUST_STORE[ki].ceiling, TRUST_STORE[ki].label) };
    if unsafe { TRUST_STORE[ki].revoked } {
        kprintln!("[sig-demo]   signer key REVOKED -> install REFUSED");
        crate::record_event("installer", "sig.demo", "package", "fail");
        return;
    }
    kprintln!("[sig-demo] 2/5 trusted publisher '{label}' found in the trust store");

    // Verify the Ed25519 signature over the exact inner package + counter.
    let valid = verify_envelope_sig(env, &e);
    if !valid {
        kprintln!("[sig-demo]   signature INVALID -> install REFUSED");
        crate::record_event("installer", "sig.demo", "package", "fail");
        return;
    }
    kprintln!("[sig-demo] 3/5 signature VALID (Ed25519 over inner .dzp + counter)");

    // Publisher attenuation: granted = requested ∩ signer ceiling.
    let inner = &env[e.inner_offset..e.inner_offset + e.inner_len];
    let Ok(pkg) = dzp::parse(inner) else {
        kprintln!("[sig-demo] FAIL: inner .dzp did not parse");
        crate::record_event("installer", "sig.demo", "package", "fail");
        return;
    };
    let requested = parse_mcaps(pkg.manifest).unwrap_or(0);
    let granted = sig::attenuate(requested, ceiling);
    let beyond = sig::beyond_ceiling(requested, ceiling);
    kprint!("[sig-demo] 4/5 requested=");
    mcap_names(requested, &mut crate::Uart);
    kprint!(" | publisher ceiling=");
    mcap_names(ceiling, &mut crate::Uart);
    kprint!(" | GRANTED=");
    mcap_names(granted, &mut crate::Uart);
    kprintln!();
    if beyond != 0 {
        kprint!("[sig-demo]   dropped beyond publisher ceiling: ");
        mcap_names(beyond, &mut crate::Uart);
        kprintln!(" (a publisher cannot authorize authority above its own ceiling)");
    }

    // Install is a ledgered effect (the ledger is the transparency log).
    let ledger = crate::run_registered_virtio_client_ns(
        plan,
        crate::cairn_req(crate::BLK_REQ_CAIRN_COMMIT, LAB, 0),
        "signed-install: signed-demo v1.0.0 by demo-publisher; granted print,cairn (ipc dropped by ceiling)",
        crate::task_ns_cap(LAB),
    );
    kprintln!("[sig-demo] 5/5 install recorded on the ledger (ns=lab): status={ledger}");

    // A tampered package must be refused: flip one inner byte, re-verify.
    let mut tampered = [0u8; 2048];
    let refuse_tamper = if env.len() <= tampered.len() {
        tampered[..env.len()].copy_from_slice(env);
        let off = e.inner_offset; // first inner byte
        tampered[off] ^= 0xFF;
        match sig::parse_envelope(&tampered[..env.len()]) {
            Ok(te) => !verify_envelope_sig(&tampered[..env.len()], &te),
            Err(_) => true,
        }
    } else {
        false
    };
    kprintln!(
        "[sig-demo] tamper check: a flipped inner byte is {}",
        if refuse_tamper { "REJECTED" } else { "not rejected!" }
    );

    // A revoked signer key must be refused, even with a valid signature.
    unsafe { TRUST_STORE[ki].revoked = true };
    let refuse_revoked = unsafe { TRUST_STORE[ki].revoked }
        && trust_lookup(&e.signer_pk).map(|i| unsafe { TRUST_STORE[i].revoked }) == Some(true);
    unsafe { TRUST_STORE[ki].revoked = false };
    kprintln!(
        "[sig-demo] revocation check: a revoked signer key is {}",
        if refuse_revoked { "REFUSED" } else { "not refused!" }
    );

    let pass = valid && granted == (MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE)
        && beyond == MCAP_IPC
        && ledger == 0
        && refuse_tamper
        && refuse_revoked;
    crate::record_event("installer", "sig.demo", "package", if pass { "OK" } else { "fail" });
    if pass {
        kprintln!("[sig-demo] PASS: only a trusted signer's valid signature installs, attenuated to the publisher ceiling; tampered + revoked are refused");
        kprintln!("[sig-demo] and even a signed package is still capability-confined + effect-reversible (the xz lesson): signing is provenance, not safety");
    } else {
        kprintln!("[sig-demo] FAIL: valid={valid} granted/beyond/ledger/tamper/revoke check mismatch");
    }
}

/// redteam escape (W8 P4): a malicious agent under a `compute` intent tries to
/// AMPLIFY its authority by writing to Cairn — beyond what the intent grants.
/// The intent-derivation ceiling drops the write capability (derived cap proven
/// <= Ahd), and when the agent still attempts the hostcall the kernel denies it.
/// Returns true iff the out-of-intent write was correctly stopped.
pub(crate) fn redteam_out_of_intent(plan: &KernelPlan) -> bool {
    let Some((id, kname, ceiling)) = open_ahd("compute", AHD_UNLIMITED) else {
        kprintln!("[redteam] could not open a compute intent");
        return false;
    };
    let requested = MCAP_PRINT | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;
    let derived = requested & ceiling;
    let beyond = requested & !ceiling;
    kprint!("[redteam] agent under Ahd#{id} kind={kname} requests=");
    mcap_names(requested, &mut crate::Uart);
    kprintln!();
    if beyond != 0 {
        kprint!("[redteam] beyond-intent dropped by the derivation ceiling: ");
        mcap_names(beyond, &mut crate::Uart);
        kprintln!(" (derived cap proven <= Ahd)");
    }
    // The Cairn binding follows the DERIVED caps: with no write/read cap there
    // is no namespace to reach — authority never exceeds the intent.
    let cairn = if derived & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) != 0 {
        crate::cairn_ns_id("agent").map(|ns| (plan, ns))
    } else {
        None
    };
    let mut buf = [0u8; 512];
    let prog = ir::demo_cairn(&mut buf);
    let mut host = crate::KHost {
        caps: ir_caps_from(derived),
        cairn,
        intent: id,
        derived,
    };
    match ir::run(prog, &mut host) {
        Ok(()) => {
            kprintln!("[redteam] (BUG) the out-of-intent write was NOT stopped");
            false
        }
        Err(t) => {
            if t == ir::Trap::MissingCapability {
                kprintln!(
                    "[redteam] kernel DENIED the out-of-intent Cairn write: {}",
                    t.msg()
                );
            } else {
                kprintln!("[redteam] agent trapped before the write: {}", t.msg());
            }
            true
        }
    }
}

// --- Inspect / remove / recovery ----------------------------------------------

pub(crate) fn pkg_list(plan: &KernelPlan) {
    if !ensure_loaded(plan) {
        kprintln!("[pkg-list] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    kprintln!("packages (persistent package store):");
    let mut any = false;
    for slot in 0..MAX_PKGS {
        let state = entry_state(&reg, slot);
        if state == STATE_EMPTY {
            continue;
        }
        any = true;
        kprint!(
            "  [{}] {} {} slot={} raw={}B crc={:#x} caps=",
            state_name(state),
            entry_name(&reg, slot),
            entry_version(&reg, slot),
            slot,
            get_u32(&reg, slot * ENTRY_SIZE + 12),
            get_u32(&reg, slot * ENTRY_SIZE + 16)
        );
        mcap_names(get_u32(&reg, slot * ENTRY_SIZE + 8), &mut crate::Uart);
        if state == STATE_ACTIVE {
            kprintln!(" runnable=yes");
        } else {
            kprintln!(" runnable=no");
        }
    }
    if !any {
        kprintln!("  (none - install one with tools/sdk/install_pkg.py)");
    }
}

pub(crate) fn pkg_info(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-info] package store unavailable or degraded");
        return;
    }
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-info] no package slot named '{name}' (see pkg-list)");
        return;
    };
    let reg = unsafe { REGISTRY };
    let e = slot * ENTRY_SIZE;
    let state = entry_state(&reg, slot);
    kprintln!(
        "package: {} {}",
        entry_name(&reg, slot),
        entry_version(&reg, slot)
    );
    kprintln!(
        "  state    {} runnable={}",
        state_name(state),
        if state == STATE_ACTIVE { "yes" } else { "no" }
    );
    kprintln!("  kind     {}", dzp::kind_name(get_u16(&reg, e + 6)));
    kprintln!(
        "  raw      {} bytes crc={:#x}",
        get_u32(&reg, e + 12),
        get_u32(&reg, e + 16)
    );
    kprintln!(
        "  store    slot={} blob_sector={} sectors={}",
        slot,
        get_u32(&reg, e + 20),
        get_u32(&reg, e + 24)
    );
    kprint!("  GRANTED  ");
    mcap_names(get_u32(&reg, e + 8), &mut crate::Uart);
    kprintln!();
    kprint!("  DENIED   ");
    let all = MCAP_TABLE.iter().fold(0, |a, &(_, b)| a | b);
    mcap_names(all & !get_u32(&reg, e + 8), &mut crate::Uart);
    kprintln!(" + device/DMA/MMIO (never grantable from a manifest)");
    if state != STATE_ACTIVE {
        kprintln!(
            "  blocked  package state is {}, not Active",
            state_name(state)
        );
    }
    kprintln!(
        "  model    grants fixed by verified manifest; no inheritance from console/installer"
    );
}

pub(crate) fn pkg_remove(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-remove] package store unavailable or degraded");
        return;
    }
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-remove] no installed package '{name}'");
        return;
    };
    let reg = unsafe { REGISTRY };
    let state = entry_state(&reg, slot);
    if state != STATE_ACTIVE {
        kprintln!(
            "[pkg-remove] refused: '{name}' is {}, not Active",
            state_name(state)
        );
        return;
    }
    let before_crc = checksum(&reg);
    let rec = journal_record(
        JOURNAL_OP_REMOVE,
        JOURNAL_PHASE_STARTED,
        slot,
        STATE_ACTIVE,
        STATE_REMOVED,
        entry_name(&reg, slot),
        entry_version(&reg, slot),
        get_u32(&reg, slot * ENTRY_SIZE + 8),
        get_u32(&reg, slot * ENTRY_SIZE + 12) as usize,
        get_u32(&reg, slot * ENTRY_SIZE + 16),
        before_crc,
        0,
    );
    if !write_journal(plan, rec) {
        kprintln!("[pkg-remove] remove failed: journal write failed");
        return;
    }
    set_entry_state(slot, STATE_PENDING_REMOVE);
    unsafe {
        PKGS[slot].used = false;
    }
    if !write_registry(plan) {
        kprintln!("[pkg-remove] remove failed: pending registry write failed");
        return;
    }
    set_entry_state(slot, STATE_REMOVED);
    if !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
        kprintln!("[pkg-remove] remove failed: registry commit failed");
        return;
    }
    invalidate_loaded();
    kprintln!("[pkg-remove] removed '{name}' (logical remove; grants revoked)");
    crate::record_event("installer", "pkg.remove", "package", "OK");
}

pub(crate) fn pkg_store(plan: &KernelPlan) {
    if !read_registry(plan) {
        kprintln!("[pkg-store] unavailable: cannot read package registry");
        return;
    }
    let recovery_ok = recover_from_journal(plan, false);
    let _ = read_registry(plan);
    let mut jraw = [0u8; JOURNAL_SIZE];
    let journal = if read_journal_raw(plan, &mut jraw) {
        decode_journal(&jraw)
    } else {
        JournalState::Corrupt("unreadable")
    };
    let reg = unsafe { REGISTRY };
    let mut active = 0usize;
    let mut removed = 0usize;
    let mut corrupt = 0usize;
    let mut pending = 0usize;
    let mut quarantined = 0usize;
    let mut empty = 0usize;
    for slot in 0..MAX_PKGS {
        match entry_state(&reg, slot) {
            STATE_ACTIVE => active += 1,
            STATE_REMOVED => removed += 1,
            STATE_CORRUPT => corrupt += 1,
            STATE_PENDING_INSTALL | STATE_PENDING_REMOVE => pending += 1,
            STATE_QUARANTINED => quarantined += 1,
            _ => empty += 1,
        }
    }
    kprintln!("pkg-store:");
    kprintln!("  marker_sector={}", PKG_STORE_MARKER_SECTOR);
    kprintln!(
        "  registry_sectors={}..{} checksum={:#x}",
        PKG_REGISTRY_SECTOR,
        PKG_REGISTRY_RESERVED_END,
        checksum(&reg)
    );
    kprintln!(
        "  journal_sectors={}..{} status={}",
        PKG_JOURNAL_SECTOR,
        PKG_JOURNAL_RESERVED_END,
        match journal {
            JournalState::Empty => "empty",
            JournalState::Valid(_) => "active",
            JournalState::Corrupt(_) => "corrupt",
        }
    );
    kprintln!(
        "  slots active={} removed={} corrupt={} pending={} quarantined={} empty={}",
        active,
        removed,
        corrupt,
        pending,
        quarantined,
        empty
    );
    kprintln!(
        "  blob_range={}..{} active={}..{} previous={}..{} stage={}..{} slot_sectors={} max_package_bytes={}",
        PKG_BLOB_FIRST_SECTOR,
        PKG_BLOB_RESERVED_END,
        PKG_BLOB_FIRST_SECTOR,
        PKG_PREVIOUS_FIRST_SECTOR - 1,
        PKG_PREVIOUS_FIRST_SECTOR,
        PKG_STAGE_FIRST_SECTOR - 1,
        PKG_STAGE_FIRST_SECTOR,
        PKG_BLOB_RESERVED_END,
        PKG_SLOT_SECTORS,
        PKG_MAX_RAW_BYTES
    );
    kprintln!(
        "  degraded={} policy=no global registry, no ambient filesystem, manifest grants only",
        if recovery_ok { "no" } else { "yes" }
    );
}

pub(crate) fn pkg_journal(plan: &KernelPlan) {
    let mut raw = [0u8; JOURNAL_SIZE];
    if !read_journal_raw(plan, &mut raw) {
        kprintln!("[pkg-journal] unavailable: cannot read package journal");
        return;
    }
    match decode_journal(&raw) {
        JournalState::Empty => kprintln!("pkg-journal: empty"),
        JournalState::Valid(rec) => print_journal_record("pkg-journal:", rec),
        JournalState::Corrupt(reason) => kprintln!("pkg-journal: corrupt ({reason})"),
    }
}

pub(crate) fn pkg_recover(plan: &KernelPlan) {
    if !read_registry(plan) {
        kprintln!("[pkg-recover] unavailable: cannot read package registry");
        return;
    }
    if recover_from_journal(plan, true) {
        kprintln!("[pkg-recover] complete");
    } else {
        kprintln!("[pkg-recover] failed; package store remains degraded");
    }
}

pub(crate) fn pkg_verify(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-verify] package store unavailable or degraded");
        return;
    }
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-verify] no package slot named '{name}'");
        return;
    };
    let reg = unsafe { REGISTRY };
    let e = slot * ENTRY_SIZE;
    let raw_len = get_u32(&reg, e + 12) as usize;
    let raw_crc = get_u32(&reg, e + 16);
    if entry_state(&reg, slot) != STATE_ACTIVE {
        kprintln!(
            "[pkg-verify] {} is {}; not runnable",
            name,
            state_name(entry_state(&reg, slot))
        );
        return;
    }
    if !read_blob_into_stage(plan, slot, raw_len) {
        kprintln!("[pkg-verify] FAIL: cannot read blob");
        return;
    }
    let bytes = unsafe { &STAGE[..raw_len] };
    if checksum(bytes) != raw_crc {
        kprintln!("[pkg-verify] FAIL: blob CRC mismatch");
        return;
    }
    let Ok(pkg) = dzp::parse(bytes) else {
        kprintln!("[pkg-verify] FAIL: invalid dzp payload");
        return;
    };
    if verify_payload(&pkg).is_err() || parse_mcaps(pkg.manifest).is_err() {
        kprintln!("[pkg-verify] FAIL: manifest or payload rejected");
        return;
    }
    kprintln!("[pkg-verify] OK: '{name}' blob, manifest caps, and payload verify");
}

pub(crate) fn pkg_update(plan: &KernelPlan, arg: &str) {
    let mut parts = arg.split_whitespace();
    let Some(target) = parts.next() else {
        kprintln!("usage: pkg-update <name> [--allow-new-caps]");
        return;
    };
    let allow_new_caps = parts.any(|p| p == "--allow-new-caps");
    if !ensure_loaded(plan) {
        kprintln!("[pkg-update] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let Some(slot) = find_registry_slot(target) else {
        kprintln!("[pkg-update] no Active package named '{target}'");
        return;
    };
    if entry_state(&reg, slot) != STATE_ACTIVE {
        kprintln!(
            "[pkg-update] refused: '{target}' is {}, not Active",
            state_name(entry_state(&reg, slot))
        );
        return;
    }
    if entry_is_pinned(&reg, slot) {
        kprintln!("[pkg-update] refused: '{target}' is pinned; run pkg-unpin first");
        return;
    }

    let old_version_buf = {
        let mut v = [0u8; VER_MAX];
        let s = entry_version(&reg, slot).as_bytes();
        v[..s.len()].copy_from_slice(s);
        (v, s.len())
    };
    let old_kind = get_u16(&reg, slot * ENTRY_SIZE + 6);
    let old_caps = get_u32(&reg, slot * ENTRY_SIZE + 8);
    let old_raw_len = get_u32(&reg, slot * ENTRY_SIZE + 12) as usize;
    let old_raw_crc = get_u32(&reg, slot * ENTRY_SIZE + 16);
    let before_crc = checksum(&reg);

    let Some(staged) = receive_raw_into_stage("pkg-update") else {
        return;
    };
    if staged == 0 || staged > PKG_MAX_RAW_BYTES {
        kprintln!("[pkg-update] rejected: package exceeds 32 KiB v0 slot limit");
        return;
    }
    let bytes = unsafe { &STAGE[..staged] };
    let pkg = match dzp::parse(bytes) {
        Ok(p) => p,
        Err(e) => {
            kprintln!("[pkg-update] rejected: {}", e.msg());
            return;
        }
    };
    let Some(name) = dzp::manifest_str(pkg.manifest, "name") else {
        kprintln!("[pkg-update] rejected: manifest has no name");
        return;
    };
    let version = dzp::manifest_str(pkg.manifest, "version").unwrap_or("0.0.0");
    if name != target {
        kprintln!("[pkg-update] rejected: package name '{name}' does not match target '{target}'");
        return;
    }
    if name.len() > NAME_MAX || version.len() > VER_MAX {
        kprintln!("[pkg-update] rejected: name/version length out of range");
        return;
    }
    let mut name_buf = [0u8; NAME_MAX];
    let mut version_buf = [0u8; VER_MAX];
    name_buf[..name.len()].copy_from_slice(name.as_bytes());
    version_buf[..version.len()].copy_from_slice(version.as_bytes());
    let name_len = name.len();
    let version_len = version.len();
    let new_kind = pkg.kind;
    let new_caps = match parse_mcaps(pkg.manifest) {
        Ok(m) => m,
        Err(e) => {
            kprintln!(
                "[pkg-update] rejected: {e} (known: print ipc uptime cairn-read cairn-write)"
            );
            return;
        }
    };
    if let Err(e) = verify_payload(&pkg) {
        kprintln!("[pkg-update] rejected: {e}");
        return;
    }
    let raw_crc = checksum(bytes);
    if entry_version(&reg, slot) == version && old_raw_crc == raw_crc {
        kprintln!("[pkg-update] already active '{target}' {version} with matching checksum");
        return;
    }
    let name = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("");
    let version = core::str::from_utf8(&version_buf[..version_len]).unwrap_or("");
    let (added, removed, unchanged) = cap_delta(old_caps, new_caps);
    if added != 0 && !allow_new_caps {
        kprint!("[pkg-update] review required: new caps requested: ");
        mcap_names(added, &mut crate::Uart);
        kprintln!("; rerun with --allow-new-caps after review");
        crate::record_event("installer", "pkg.update", "package", "REVIEW_REQUIRED");
        return;
    }

    let mut rec = journal_record(
        JOURNAL_OP_REPLACE,
        JOURNAL_PHASE_STARTED,
        slot,
        STATE_ACTIVE,
        STATE_ACTIVE,
        name,
        version,
        new_caps,
        staged,
        raw_crc,
        before_crc,
        0,
    );
    if !write_journal(plan, rec) {
        kprintln!("[pkg-update] rejected: journal write failed before update");
        return;
    }
    if !write_stage_blob(plan, slot, bytes) {
        kprintln!("[pkg-update] rejected: staged blob write failed");
        return;
    }
    rec.phase = JOURNAL_PHASE_BLOB_WRITTEN;
    if !write_journal(plan, rec) || !verify_stage_blob(plan, rec) {
        kprintln!("[pkg-update] rejected: staged blob verify failed");
        return;
    }
    if !copy_blob_area(
        plan,
        slot_blob_sector(slot),
        previous_blob_sector(slot),
        old_raw_len,
    ) {
        kprintln!("[pkg-update] rejected: could not preserve previous active blob");
        return;
    }

    encode_registry_entry(
        slot,
        STATE_PENDING_INSTALL,
        name,
        version,
        new_kind,
        new_caps,
        staged,
        raw_crc,
    );
    let old_version = core::str::from_utf8(&old_version_buf.0[..old_version_buf.1]).unwrap_or("");
    encode_previous_metadata(
        slot,
        old_version,
        old_kind,
        old_caps,
        old_raw_len,
        old_raw_crc,
    );
    rec.phase = JOURNAL_PHASE_REGISTRY_PENDING;
    rec.registry_after_crc = {
        let reg = unsafe { REGISTRY };
        checksum(&reg)
    };
    if !write_journal(plan, rec) || !write_registry(plan) {
        kprintln!("[pkg-update] rejected: pending registry write failed");
        return;
    }
    if !copy_blob_area(
        plan,
        stage_blob_sector(slot),
        slot_blob_sector(slot),
        staged,
    ) {
        kprintln!("[pkg-update] rejected: staged promote failed");
        return;
    }
    set_entry_state(slot, STATE_ACTIVE);
    if !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
        kprintln!("[pkg-update] rejected: update commit failed");
        return;
    }
    invalidate_loaded();
    kprint!("[pkg-update] committed '{target}' {version}; caps added=");
    mcap_names(added, &mut crate::Uart);
    kprint!(" removed=");
    mcap_names(removed, &mut crate::Uart);
    kprint!(" unchanged=");
    mcap_names(unchanged, &mut crate::Uart);
    kprintln!();
    crate::record_event("installer", "pkg.update", "package", "OK");
}

pub(crate) fn pkg_rollback(plan: &KernelPlan, arg: &str) {
    let mut parts = arg.split_whitespace();
    let Some(name) = parts.next() else {
        kprintln!("usage: pkg-rollback <name> [--force]");
        return;
    };
    let force = parts.any(|p| p == "--force");
    if !ensure_loaded(plan) {
        kprintln!("[pkg-rollback] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-rollback] no package slot named '{name}'");
        return;
    };
    if entry_state(&reg, slot) != STATE_ACTIVE || !entry_previous_valid(&reg, slot) {
        kprintln!("[pkg-rollback] refused: no verified previous version for '{name}'");
        return;
    }
    if entry_is_pinned(&reg, slot) && !force {
        kprintln!("[pkg-rollback] refused: '{name}' is pinned; use --force only after review");
        return;
    }
    let prev_version = entry_previous_version(&reg, slot);
    let prev_kind = get_u16(&reg, slot * ENTRY_SIZE + 82);
    let prev_caps = get_u32(&reg, slot * ENTRY_SIZE + 84);
    let prev_raw_len = get_u32(&reg, slot * ENTRY_SIZE + 88) as usize;
    let prev_raw_crc = get_u32(&reg, slot * ENTRY_SIZE + 92);
    let before_crc = checksum(&reg);
    if !read_previous_blob_into_stage(plan, slot, prev_raw_len)
        || checksum(unsafe { &STAGE[..prev_raw_len] }) != prev_raw_crc
    {
        kprintln!("[pkg-rollback] refused: previous blob failed verification");
        return;
    }
    let rec = journal_record(
        JOURNAL_OP_ROLLBACK,
        JOURNAL_PHASE_REGISTRY_PENDING,
        slot,
        STATE_ACTIVE,
        STATE_ACTIVE,
        name,
        prev_version,
        prev_caps,
        prev_raw_len,
        prev_raw_crc,
        before_crc,
        0,
    );
    if !write_journal(plan, rec) {
        kprintln!("[pkg-rollback] refused: journal write failed");
        return;
    }
    let pinned = entry_is_pinned(&reg, slot);
    encode_registry_entry(
        slot,
        STATE_PENDING_INSTALL,
        name,
        prev_version,
        prev_kind,
        prev_caps,
        prev_raw_len,
        prev_raw_crc,
    );
    if pinned {
        set_entry_flags(slot, ENTRY_FLAG_PINNED);
    }
    if !write_registry(plan) {
        kprintln!("[pkg-rollback] failed: pending registry write failed");
        return;
    }
    if !write_blob_at(plan, slot_blob_sector(slot), unsafe {
        &STAGE[..prev_raw_len]
    }) {
        kprintln!("[pkg-rollback] failed: active blob restore failed");
        return;
    }
    set_entry_state(slot, STATE_ACTIVE);
    if !write_registry(plan) || !init_store_marker(plan) || !clear_journal(plan) {
        kprintln!("[pkg-rollback] failed: rollback commit failed");
        return;
    }
    invalidate_loaded();
    kprintln!("[pkg-rollback] restored '{name}' to {prev_version}; previous checkpoint consumed");
    crate::record_event("installer", "pkg.rollback", "package", "OK");
}

pub(crate) fn pkg_versions(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-versions] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-versions] no package slot named '{name}'");
        return;
    };
    kprintln!("pkg-versions {name}:");
    kprintln!(
        "  active={} crc={:#x} caps={:#x}",
        entry_version(&reg, slot),
        get_u32(&reg, slot * ENTRY_SIZE + 16),
        get_u32(&reg, slot * ENTRY_SIZE + 8)
    );
    if entry_previous_valid(&reg, slot) {
        kprintln!(
            "  previous={} crc={:#x} rollback=yes",
            entry_previous_version(&reg, slot),
            get_u32(&reg, slot * ENTRY_SIZE + 92)
        );
    } else {
        kprintln!("  previous=(none) rollback=no");
    }
}

pub(crate) fn pkg_review(plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-review] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-review] no package slot named '{name}'");
        return;
    };
    let flags = entry_flags(&reg, slot);
    kprintln!("pkg-review {name}:");
    kprintln!(
        "  state={} pinned={}",
        state_name(entry_state(&reg, slot)),
        if flags & ENTRY_FLAG_PINNED != 0 {
            "yes"
        } else {
            "no"
        }
    );
    kprint!("  active_caps=");
    mcap_names(get_u32(&reg, slot * ENTRY_SIZE + 8), &mut crate::Uart);
    kprintln!();
    if entry_previous_valid(&reg, slot) {
        let (added, removed, unchanged) = cap_delta(
            get_u32(&reg, slot * ENTRY_SIZE + 84),
            get_u32(&reg, slot * ENTRY_SIZE + 8),
        );
        kprint!("  since_previous added=");
        mcap_names(added, &mut crate::Uart);
        kprint!(" removed=");
        mcap_names(removed, &mut crate::Uart);
        kprint!(" unchanged=");
        mcap_names(unchanged, &mut crate::Uart);
        kprintln!();
    }
    kprintln!("  policy=no silent update; new caps require pkg-update --allow-new-caps");
}

pub(crate) fn pkg_pin(plan: &KernelPlan, arg: &str, pinned: bool) {
    let name = arg.trim();
    if !ensure_loaded(plan) {
        kprintln!("[pkg-pin] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let Some(slot) = find_registry_slot(name) else {
        kprintln!("[pkg-pin] no package slot named '{name}'");
        return;
    };
    let mut flags = entry_flags(&reg, slot);
    if pinned {
        flags |= ENTRY_FLAG_PINNED;
    } else {
        flags &= !ENTRY_FLAG_PINNED;
    }
    set_entry_flags(slot, flags);
    if !write_registry(plan) || !init_store_marker(plan) {
        kprintln!("[pkg-pin] failed: registry commit failed");
        return;
    }
    invalidate_loaded();
    kprintln!(
        "[pkg-pin] '{name}' pinned={}",
        if pinned { "yes" } else { "no" }
    );
    crate::record_event(
        "installer",
        if pinned { "pkg.pin" } else { "pkg.unpin" },
        "package",
        "OK",
    );
}

pub(crate) fn pkg_lifecycle(plan: &KernelPlan) {
    if !ensure_loaded(plan) {
        kprintln!("[pkg-lifecycle] package store unavailable or degraded");
        return;
    }
    let reg = unsafe { REGISTRY };
    let mut active = 0usize;
    let mut previous = 0usize;
    let mut pinned = 0usize;
    let mut removed = 0usize;
    let mut quarantined = 0usize;
    for slot in 0..MAX_PKGS {
        let state = entry_state(&reg, slot);
        if state == STATE_ACTIVE {
            active += 1;
        }
        if state == STATE_REMOVED {
            removed += 1;
        }
        if state == STATE_QUARANTINED {
            quarantined += 1;
        }
        if entry_previous_valid(&reg, slot) {
            previous += 1;
        }
        if entry_is_pinned(&reg, slot) {
            pinned += 1;
        }
    }
    kprintln!(
        "pkg-lifecycle: active={} previous={} pinned={} removed={} quarantined={} policy=no-silent-upgrade",
        active,
        previous,
        pinned,
        removed,
        quarantined
    );
}

pub(crate) fn pkg_retire(plan: &KernelPlan, arg: &str) {
    kprintln!("[pkg-retire] aliasing to logical remove; physical cleanup remains pkg-gc run");
    pkg_remove(plan, arg);
}

pub(crate) fn pkg_audit(_plan: &KernelPlan, arg: &str) {
    let name = arg.trim();
    if name.is_empty() {
        kprintln!("pkg-audit: package lifecycle events are included in global `audit`");
    } else {
        kprintln!("pkg-audit {name}: install/update/rollback/pin/remove/gc events are recorded in global `audit`");
    }
}

pub(crate) fn pkg_gc(plan: &KernelPlan, arg: &str) {
    let mode = arg.trim();
    let run = match mode {
        "" | "plan" => false,
        "run" => true,
        _ => {
            kprintln!("usage: pkg-gc [plan|run]");
            return;
        }
    };
    if !ensure_loaded(plan) {
        kprintln!("[pkg-gc] package store unavailable or degraded");
        return;
    }
    let mut raw = [0u8; JOURNAL_SIZE];
    if !read_journal_raw(plan, &mut raw) {
        kprintln!("[pkg-gc] unavailable: cannot read package journal");
        return;
    }
    match decode_journal(&raw) {
        JournalState::Empty => {}
        JournalState::Valid(_) => {
            kprintln!("[pkg-gc] refused: transaction journal active; run pkg-recover first");
            return;
        }
        JournalState::Corrupt(reason) => {
            kprintln!("[pkg-gc] refused: journal corrupt ({reason}); run pkg-recover first");
            return;
        }
    }

    let reg = unsafe { REGISTRY };
    let mut removed_slots = [0usize; MAX_PKGS];
    let mut removed_count = 0usize;
    for slot in 0..MAX_PKGS {
        if entry_state(&reg, slot) == STATE_REMOVED {
            removed_slots[removed_count] = slot;
            removed_count += 1;
        }
    }
    let reclaimable = removed_count * PKG_SLOT_SECTORS * SECTOR_SIZE;
    if !run {
        kprintln!(
            "pkg-gc plan: removed_slots={} reclaimable_bytes={} dry_run=yes",
            removed_count,
            reclaimable
        );
        for slot in removed_slots.iter().take(removed_count) {
            kprintln!(
                "  slot={} package={} {} active_blob={}..{} previous_blob={}..{} stage_blob={}..{} action=wipe-then-empty",
                slot,
                entry_name(&reg, *slot),
                entry_version(&reg, *slot),
                slot_blob_sector(*slot),
                slot_blob_sector(*slot) + PKG_SLOT_SECTORS - 1,
                previous_blob_sector(*slot),
                previous_blob_sector(*slot) + PKG_SLOT_SECTORS - 1,
                stage_blob_sector(*slot),
                stage_blob_sector(*slot) + PKG_SLOT_SECTORS - 1
            );
        }
        kprintln!("  policy=explicit only; Active/Corrupt/Quarantined slots are untouched");
        return;
    }

    if removed_count == 0 {
        kprintln!("pkg-gc run: nothing to reclaim");
        return;
    }
    let mut wiped = 0usize;
    for slot in removed_slots.iter().take(removed_count) {
        if !zero_blob_slot(plan, *slot)
            || !zero_blob_area(plan, previous_blob_sector(*slot))
            || !zero_blob_area(plan, stage_blob_sector(*slot))
        {
            kprintln!(
                "[pkg-gc] failed while wiping slot {}; registry left unchanged",
                slot
            );
            return;
        }
        clear_registry_slot(*slot);
        wiped += 1;
    }
    if !write_registry(plan) || !init_store_marker(plan) {
        kprintln!("[pkg-gc] failed while committing compacted registry");
        return;
    }
    invalidate_loaded();
    kprintln!(
        "pkg-gc run: wiped_slots={} reclaimed_bytes={} registry_checksum={:#x}",
        wiped,
        wiped * PKG_SLOT_SECTORS * SECTOR_SIZE,
        {
            let reg = unsafe { REGISTRY };
            checksum(&reg)
        }
    );
    kprintln!("  policy=explicit physical cleanup; no automatic erase on remove");
    crate::record_event("installer", "pkg.gc", "package-store", "OK");
}

pub(crate) fn pkg_fault(plan: &KernelPlan, arg: &str) {
    if !read_registry(plan) {
        kprintln!("[pkg-fault] unavailable: cannot read package registry");
        return;
    }
    match arg.trim() {
        "install-after-blob" => {
            let slot = registry_slot_for("faultpkg").unwrap_or(MAX_PKGS - 1);
            let reg = unsafe { REGISTRY };
            let old_state = entry_state(&reg, slot);
            let rec = journal_record(
                JOURNAL_OP_INSTALL,
                JOURNAL_PHASE_BLOB_WRITTEN,
                slot,
                old_state,
                STATE_ACTIVE,
                "faultpkg",
                "0.0.1",
                MCAP_PRINT,
                128,
                0xfeed_b10b,
                checksum(&reg),
                0,
            );
            if write_journal(plan, rec) {
                invalidate_loaded();
                kprintln!("[pkg-fault] injected install-after-blob; reboot then run pkg-journal/pkg-recover");
            }
        }
        "install-pending-registry" => {
            let slot = registry_slot_for("faultpkg").unwrap_or(MAX_PKGS - 1);
            let reg = unsafe { REGISTRY };
            let old_state = entry_state(&reg, slot);
            encode_registry_entry(
                slot,
                STATE_PENDING_INSTALL,
                "faultpkg",
                "0.0.1",
                dzp::KIND_DEZH_IR,
                MCAP_PRINT,
                128,
                0xfeed_b10b,
            );
            let rec = journal_record(
                JOURNAL_OP_INSTALL,
                JOURNAL_PHASE_REGISTRY_PENDING,
                slot,
                old_state,
                STATE_ACTIVE,
                "faultpkg",
                "0.0.1",
                MCAP_PRINT,
                128,
                0xfeed_b10b,
                0,
                {
                    let reg = unsafe { REGISTRY };
                    checksum(&reg)
                },
            );
            if write_registry(plan) && write_journal(plan, rec) {
                invalidate_loaded();
                kprintln!("[pkg-fault] injected install-pending-registry; reboot then recover");
            }
        }
        "remove-pending" => {
            let Some(slot) = find_registry_slot("hello") else {
                kprintln!("[pkg-fault] remove-pending requires an installed package named 'hello'");
                return;
            };
            let reg = unsafe { REGISTRY };
            let rec = journal_record(
                JOURNAL_OP_REMOVE,
                JOURNAL_PHASE_STARTED,
                slot,
                STATE_ACTIVE,
                STATE_REMOVED,
                entry_name(&reg, slot),
                entry_version(&reg, slot),
                get_u32(&reg, slot * ENTRY_SIZE + 8),
                get_u32(&reg, slot * ENTRY_SIZE + 12) as usize,
                get_u32(&reg, slot * ENTRY_SIZE + 16),
                checksum(&reg),
                0,
            );
            set_entry_state(slot, STATE_PENDING_REMOVE);
            if write_registry(plan) && write_journal(plan, rec) {
                invalidate_loaded();
                kprintln!("[pkg-fault] injected remove-pending for 'hello'; reboot then recover");
            }
        }
        "corrupt-journal" => {
            let mut raw = [0u8; JOURNAL_SIZE];
            raw[0..4].copy_from_slice(b"BROK");
            raw[64..72].copy_from_slice(b"faultpkg");
            if write_journal_raw(plan, &raw) {
                invalidate_loaded();
                kprintln!("[pkg-fault] injected corrupt-journal; reboot should degrade package execution");
            }
        }
        _ => kprintln!("usage: pkg-fault <install-after-blob|install-pending-registry|remove-pending|corrupt-journal>"),
    }
}

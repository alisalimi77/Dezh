//! `.dzp` package ingestion + installed-package registry (W1).
//!
//! The install path for out-of-tree apps: the host builds a `.dzp` with
//! `tools/sdk/build-pkg.py`, then streams it over the UART as base64 lines
//! (`pkg-recv`). The kernel checks integrity (CRC-32), verifies Dezh-IR
//! payloads *before* they are ever runnable, and records the capability
//! grants from the manifest at install time. `pkg-run` executes a package
//! with exactly those grants — nothing ambient.

use core::fmt::Write;

use crate::{kprint, kprintln};
use dezh_core::{b64, dzp, ir};

// --- Manifest capability vocabulary -------------------------------------------
// What an app.toml may request. Unknown names make the install FAIL (explicit
// beats silent): a reviewer must never wonder what an undeclared string grants.

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

// --- Registry ------------------------------------------------------------------

const STAGE_SIZE: usize = 64 * 1024;
static mut STAGE: [u8; STAGE_SIZE] = [0; STAGE_SIZE];

const ARENA_SIZE: usize = 128 * 1024;
static mut ARENA: [u8; ARENA_SIZE] = [0; ARENA_SIZE];
static mut ARENA_USED: usize = 0;

const MAX_PKGS: usize = 8;
const NAME_MAX: usize = 24;
const VER_MAX: usize = 12;

#[derive(Clone, Copy)]
pub(crate) struct PkgEntry {
    used: bool,
    name: [u8; NAME_MAX],
    name_len: u8,
    version: [u8; VER_MAX],
    version_len: u8,
    kind: u16,
    mcaps: u32,
    off: u32,
    len: u32,
}

const EMPTY_PKG: PkgEntry = PkgEntry {
    used: false,
    name: [0; NAME_MAX],
    name_len: 0,
    version: [0; VER_MAX],
    version_len: 0,
    kind: 0,
    mcaps: 0,
    off: 0,
    len: 0,
};

static mut PKGS: [PkgEntry; MAX_PKGS] = [EMPTY_PKG; MAX_PKGS];

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

fn find_pkg(name: &str) -> Option<usize> {
    unsafe { (0..MAX_PKGS).find(|&i| PKGS[i].used && PKGS[i].name() == name) }
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

// --- Install -------------------------------------------------------------------

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

/// `pkg-recv`: receive a `.dzp` as base64 lines over the UART, verify it, and
/// register it with its manifest grants. Flow control is line-by-line: the
/// host must wait for `+ok` before sending the next chunk.
pub(crate) fn pkg_recv() {
    kprintln!("[pkg-recv] ready: send base64 lines; end with '.', abort with '!'");
    let mut staged = 0usize;
    let mut line = [0u8; 120];
    loop {
        let n = read_raw_line(&mut line);
        let text = &line[..n];
        if text == b"." {
            break;
        }
        if text == b"!" {
            kprintln!("[pkg-recv] aborted by sender");
            return;
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
                return;
            }
        }
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

    // Payload sanity at install time, per kind: a Dezh-IR payload must pass the
    // static verifier; an ELF payload must at least be a riscv64 ELF.
    match pkg.kind {
        dzp::KIND_DEZH_IR => {
            if let Err(t) = ir::verify(pkg.payload) {
                kprintln!("[pkg-recv] rejected: Dezh-IR verify failed: {}", t.msg());
                return;
            }
        }
        dzp::KIND_ELF_RISCV64 => {
            let p = pkg.payload;
            let is_riscv64_elf = p.len() > 20
                && &p[0..4] == b"\x7fELF"
                && u16::from_le_bytes([p[18], p[19]]) == 243;
            if !is_riscv64_elf {
                kprintln!("[pkg-recv] rejected: payload is not a riscv64 ELF");
                return;
            }
        }
        _ => unreachable!(),
    }

    // Replace an existing package with the same name (arena space of the old
    // payload is not compacted in v1 — install/remove churn is bounded by the
    // arena, stated honestly in the SDK guide).
    if let Some(i) = find_pkg(name) {
        unsafe { PKGS[i].used = false };
        kprintln!("[pkg-recv] replacing already-installed '{name}'");
    }
    let Some(slot) = (unsafe { (0..MAX_PKGS).find(|&i| !PKGS[i].used) }) else {
        kprintln!("[pkg-recv] rejected: registry full ({MAX_PKGS} packages)");
        return;
    };
    let (arena_used, fits) =
        unsafe { (ARENA_USED, ARENA_USED + pkg.payload.len() <= ARENA_SIZE) };
    if !fits {
        kprintln!("[pkg-recv] rejected: payload arena full");
        return;
    }

    unsafe {
        let base = core::ptr::addr_of_mut!(ARENA) as *mut u8;
        core::ptr::copy_nonoverlapping(pkg.payload.as_ptr(), base.add(arena_used), pkg.payload.len());
        let mut e = EMPTY_PKG;
        e.used = true;
        e.name[..name.len()].copy_from_slice(name.as_bytes());
        e.name_len = name.len() as u8;
        e.version[..version.len()].copy_from_slice(version.as_bytes());
        e.version_len = version.len() as u8;
        e.kind = pkg.kind;
        e.mcaps = mcaps;
        e.off = arena_used as u32;
        e.len = pkg.payload.len() as u32;
        PKGS[slot] = e;
        ARENA_USED = arena_used + pkg.payload.len();
    }

    kprintln!(
        "[pkg] installed '{name}' {version} kind={} payload={} bytes",
        dzp::kind_name(pkg.kind),
        pkg.payload.len()
    );
    kprint!("[pkg] grants recorded at install time: ");
    mcap_names(mcaps, &mut crate::Uart);
    kprintln!(" (kernel-enforced at run time)");
    crate::record_event("installer", "pkg.install", "package", "OK");
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

fn task_caps_from(mcaps: u32) -> usize {
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
    c
}

pub(crate) fn pkg_run(plan: &dezh_kernel::KernelPlan, arg: &str) {
    let name = arg.trim();
    let Some(i) = find_pkg(name) else {
        kprintln!("[pkg-run] no installed package '{name}' (see pkg-list)");
        return;
    };
    let entry = unsafe { PKGS[i] };
    kprint!(
        "[pkg-run] '{}' {} kind={} caps=",
        entry.name(),
        entry.version(),
        dzp::kind_name(entry.kind)
    );
    mcap_names(entry.mcaps, &mut crate::Uart);
    kprintln!();
    crate::record_event("installer", "pkg.run", "package", "start");
    match entry.kind {
        dzp::KIND_DEZH_IR => {
            let mut host = crate::KHost {
                caps: ir_caps_from(entry.mcaps),
            };
            match ir::run(entry.payload(), &mut host) {
                Ok(()) => kprintln!("[pkg-run] '{}' finished", entry.name()),
                Err(t) => {
                    if t == ir::Trap::MissingCapability {
                        kprintln!(
                            "[pkg-run] DENIED by kernel: {} (grant it in app.toml caps=[...])",
                            t.msg()
                        );
                        crate::record_event("kernel", "pkg.run", "package", "DENIED");
                    } else {
                        kprintln!("[pkg-run] TRAP: {}", t.msg());
                        crate::record_event("kernel", "pkg.run", "package", "TRAP");
                    }
                    return;
                }
            }
        }
        dzp::KIND_ELF_RISCV64 => {
            let _ = plan;
            kprintln!("[pkg-run] launching as U-mode process (own address space)");
            crate::run_foreground_processes(&[crate::ProcessSpec::new(
                entry.payload(),
                task_caps_from(entry.mcaps),
                0,
            )]);
            kprintln!("[pkg-run] '{}' exited; back in the console", entry.name());
        }
        _ => kprintln!("[pkg-run] unknown payload kind"),
    }
    crate::record_event("installer", "pkg.run", "package", "OK");
}

// --- Inspect / remove ------------------------------------------------------------

pub(crate) fn pkg_list() {
    kprintln!("installed packages (via pkg-recv):");
    let mut any = false;
    for i in 0..MAX_PKGS {
        let e = unsafe { PKGS[i] };
        if !e.used {
            continue;
        }
        any = true;
        kprint!(
            "  {} {} kind={} payload={}B caps=",
            e.name(),
            e.version(),
            dzp::kind_name(e.kind),
            e.len
        );
        mcap_names(e.mcaps, &mut crate::Uart);
        kprintln!();
    }
    if !any {
        kprintln!("  (none — install one with tools/sdk/install-pkg.py)");
    }
}

pub(crate) fn pkg_info(arg: &str) {
    let name = arg.trim();
    let Some(i) = find_pkg(name) else {
        kprintln!("[pkg-info] no installed package '{name}' (see pkg-list)");
        return;
    };
    let e = unsafe { PKGS[i] };
    kprintln!("package: {} {}", e.name(), e.version());
    kprintln!("  kind     {}", dzp::kind_name(e.kind));
    kprintln!("  payload  {} bytes (verified at install)", e.len);
    kprint!("  GRANTED  ");
    mcap_names(e.mcaps, &mut crate::Uart);
    kprintln!();
    kprint!("  DENIED   ");
    let all = MCAP_TABLE.iter().fold(0, |a, &(_, b)| a | b);
    mcap_names(all & !e.mcaps, &mut crate::Uart);
    kprintln!(" + device/DMA/MMIO (never grantable from a manifest)");
    kprintln!("  model    grants fixed at install; kernel checks every use at run time");
}

pub(crate) fn pkg_remove(arg: &str) {
    let name = arg.trim();
    let Some(i) = find_pkg(name) else {
        kprintln!("[pkg-remove] no installed package '{name}'");
        return;
    };
    unsafe { PKGS[i].used = false };
    kprintln!("[pkg-remove] removed '{name}' (grants revoked with it)");
    crate::record_event("installer", "pkg.remove", "package", "OK");
}

//! Manifest capabilities: what a `.dzp` may ask for, and what it actually gets.
//!
//! This is the narrowest and most load-bearing decision in the system. Every
//! other guarantee - the intent ceiling, the effect ledger, mission rollback -
//! is downstream of one question: *given a manifest, exactly which authority
//! does the installed app hold?* If [`task_caps_from`] returns one bit more
//! than the manifest justifies, every claim above it is void, and no amount of
//! kernel enforcement below it helps: the kernel would be faithfully enforcing
//! the wrong answer.
//!
//! It lived inside the RISC-V kernel binary, where it could only be exercised
//! by booting QEMU and reading console output. It is pure - manifest text and
//! bit arithmetic, no hardware - so it lives here instead, where it is tested
//! directly and where the x86 kernel can reach the same answer rather than
//! reimplementing it.

use crate::dzp;

// --- What a manifest may request -------------------------------------------

pub const MCAP_PRINT: u32 = 1 << 0;
pub const MCAP_IPC: u32 = 1 << 1;
pub const MCAP_UPTIME: u32 = 1 << 2;
pub const MCAP_CAIRN_READ: u32 = 1 << 3;
pub const MCAP_CAIRN_WRITE: u32 = 1 << 4;

/// The complete vocabulary. A name absent from this table is refused rather
/// than ignored - a manifest asking for authority Dezh does not model must not
/// install as though it asked for nothing.
pub const MCAP_TABLE: &[(&str, u32)] = &[
    ("print", MCAP_PRINT),
    ("ipc", MCAP_IPC),
    ("uptime", MCAP_UPTIME),
    ("cairn-read", MCAP_CAIRN_READ),
    ("cairn-write", MCAP_CAIRN_WRITE),
];

/// Every bit any manifest can legally set.
pub const MCAP_ALL: u32 =
    MCAP_PRINT | MCAP_IPC | MCAP_UPTIME | MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;

// --- Live task capability bits (mirrored by each kernel) ---------------------

pub const TASK_PRINT: usize = 1 << 0;
pub const TASK_TIME: usize = 1 << 1;
pub const TASK_IPC: usize = 1 << 2;

/// Cairn namespace capabilities occupy bits 8.. of a task's capability word.
pub const TASK_CAIRN_NS_BASE: usize = 8;

/// The fixed Cairn v1 namespace table. An app's namespace is found by *name*,
/// which is what makes "an app reaches its own namespace and no other" a
/// structural property rather than a policy check: there is no syntax for
/// naming someone else's.
pub const CAIRN_NS_NAMES: [&str; 5] = ["note", "lab", "calc", "vault", "agent"];

pub fn cairn_ns_id(name: &str) -> Option<usize> {
    CAIRN_NS_NAMES.iter().position(|n| *n == name)
}

pub const fn task_ns_cap(ns: usize) -> usize {
    1 << (TASK_CAIRN_NS_BASE + ns)
}

// --- Manifest -> requested set ----------------------------------------------

/// Parse the manifest's `caps` list. An unknown name is an error, never a
/// silent zero.
pub fn parse_mcaps(manifest: &str) -> Result<u32, &'static str> {
    let mut set = 0u32;
    for cap in dzp::manifest_list(manifest, "caps") {
        match MCAP_TABLE.iter().find(|(n, _)| *n == cap) {
            Some((_, bit)) => set |= bit,
            None => return Err("unknown capability in manifest"),
        }
    }
    Ok(set)
}

/// Render a set as its manifest names, for display and denial messages.
pub fn mcap_names(set: u32, out: &mut dyn core::fmt::Write) {
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

// --- Requested set -> granted authority --------------------------------------

/// The Dezh-IR engine capabilities an app holds.
pub fn ir_caps_from(mcaps: u32) -> u32 {
    let mut c = 0u32;
    if mcaps & MCAP_PRINT != 0 {
        c |= crate::ir::CAP_PRINT;
    }
    if mcaps & MCAP_CAIRN_READ != 0 {
        c |= crate::ir::CAP_READ;
    }
    if mcaps & MCAP_CAIRN_WRITE != 0 {
        c |= crate::ir::CAP_WRITE;
    }
    c
}

/// The live task capability word an installed app runs with.
///
/// The Cairn rule is the one worth stating twice: a manifest cairn grant maps
/// to the app's **own** namespace bit and nothing else, matched by app name. An
/// app cannot name another app's namespace in its manifest, and an app whose
/// name is not in the v1 table gets no namespace bit at all - it does not
/// fall back to a shared or default one.
pub fn task_caps_from(mcaps: u32, name: &str) -> usize {
    let mut c = 0usize;
    if mcaps & MCAP_PRINT != 0 {
        c |= TASK_PRINT;
    }
    if mcaps & MCAP_IPC != 0 {
        c |= TASK_IPC;
    }
    if mcaps & MCAP_UPTIME != 0 {
        c |= TASK_TIME;
    }
    // A let-chain, available since this crate moved to edition 2024. Asking for
    // Cairn and having a namespace to be granted are one condition: a manifest
    // that requests storage under a name with no v1 namespace gets no storage
    // capability, and nesting the two said that less directly.
    if mcaps & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) != 0
        && let Some(ns) = cairn_ns_id(name)
    {
        c |= task_ns_cap(ns);
    }
    c
}

/// The Cairn v1 namespace an installed app may use: its own, by name. `None`
/// when it asked for no storage, or when its name has no v1 namespace.
pub fn app_cairn_ns(mcaps: u32, name: &str) -> Option<usize> {
    if mcaps & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) == 0 {
        return None;
    }
    cairn_ns_id(name)
}

/// `(added, removed, kept)` between two capability sets. Install-time review
/// uses `added` to refuse a silent escalation on update.
pub fn cap_delta(old_caps: u32, new_caps: u32) -> (u32, u32, u32) {
    (
        new_caps & !old_caps,
        old_caps & !new_caps,
        old_caps & new_caps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The crate is `no_std`; tests opt back into std for String/format!.
    extern crate std;
    use std::string::String;

    // --- The property the whole system rests on -----------------------------

    /// Granted authority never exceeds what the manifest requested. Checked by
    /// exhaustive enumeration over the whole manifest bit space and every app
    /// name, because "we could not think of a case" is not a proof.
    #[test]
    fn granted_authority_never_exceeds_the_manifest() {
        let names: [&str; 7] = [
            "note", "lab", "calc", "vault", "agent", "stranger", "",
        ];
        for mcaps in 0..=MCAP_ALL {
            for name in names {
                let caps = task_caps_from(mcaps, name);

                if mcaps & MCAP_PRINT == 0 {
                    assert_eq!(caps & TASK_PRINT, 0, "print without asking: {mcaps:#x}");
                }
                if mcaps & MCAP_IPC == 0 {
                    assert_eq!(caps & TASK_IPC, 0, "ipc without asking: {mcaps:#x}");
                }
                if mcaps & MCAP_UPTIME == 0 {
                    assert_eq!(caps & TASK_TIME, 0, "time without asking: {mcaps:#x}");
                }

                // No storage request => not one namespace bit.
                let ns_mask: usize = (0..CAIRN_NS_NAMES.len()).map(task_ns_cap).sum();
                if mcaps & (MCAP_CAIRN_READ | MCAP_CAIRN_WRITE) == 0 {
                    assert_eq!(caps & ns_mask, 0, "namespace without asking: {mcaps:#x}");
                }

                // Nothing outside the bits this function is allowed to set.
                let legal = TASK_PRINT | TASK_IPC | TASK_TIME | ns_mask;
                assert_eq!(caps & !legal, 0, "unmodelled bit set: {mcaps:#x} {name}");
            }
        }
    }

    /// An app reaches its own namespace and no other - the F2 claim, checked
    /// here rather than only observed in a console transcript.
    #[test]
    fn an_app_can_only_ever_reach_its_own_namespace() {
        let both = MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;
        for (i, name) in CAIRN_NS_NAMES.iter().enumerate() {
            let caps = task_caps_from(both, name);
            assert_eq!(caps & task_ns_cap(i), task_ns_cap(i), "{name} lost its own ns");
            for (j, other) in CAIRN_NS_NAMES.iter().enumerate() {
                if i != j {
                    assert_eq!(
                        caps & task_ns_cap(j),
                        0,
                        "{name} reached {other}'s namespace"
                    );
                }
            }
        }
    }

    /// An app whose name has no v1 namespace gets none - it must not fall back
    /// to a default, which would be a shared namespace by another name.
    #[test]
    fn an_unknown_app_name_gets_no_namespace_rather_than_a_default() {
        let both = MCAP_CAIRN_READ | MCAP_CAIRN_WRITE;
        let caps = task_caps_from(both, "stranger");
        let ns_mask: usize = (0..CAIRN_NS_NAMES.len()).map(task_ns_cap).sum();
        assert_eq!(caps & ns_mask, 0);
        assert_eq!(app_cairn_ns(both, "stranger"), None);
    }

    #[test]
    fn no_storage_request_means_no_namespace_at_all() {
        for name in CAIRN_NS_NAMES {
            assert_eq!(app_cairn_ns(0, name), None);
            assert_eq!(app_cairn_ns(MCAP_PRINT | MCAP_IPC, name), None);
        }
        // Either half of the grant is enough to select the namespace.
        assert_eq!(app_cairn_ns(MCAP_CAIRN_READ, "note"), Some(0));
        assert_eq!(app_cairn_ns(MCAP_CAIRN_WRITE, "note"), Some(0));
    }

    /// IR engine capabilities are a projection of the manifest too - and the
    /// mapping is exactly three bits, with no path from `ipc`/`uptime` into it.
    #[test]
    fn ir_caps_are_bounded_by_the_manifest() {
        for mcaps in 0..=MCAP_ALL {
            let c = ir_caps_from(mcaps);
            if mcaps & MCAP_PRINT == 0 {
                assert_eq!(c & crate::ir::CAP_PRINT, 0);
            }
            if mcaps & MCAP_CAIRN_READ == 0 {
                assert_eq!(c & crate::ir::CAP_READ, 0);
            }
            if mcaps & MCAP_CAIRN_WRITE == 0 {
                assert_eq!(c & crate::ir::CAP_WRITE, 0);
            }
            let legal = crate::ir::CAP_PRINT | crate::ir::CAP_READ | crate::ir::CAP_WRITE;
            assert_eq!(c & !legal, 0);
        }
        assert_eq!(ir_caps_from(MCAP_IPC | MCAP_UPTIME), 0);
    }

    // --- Parsing -------------------------------------------------------------

    #[test]
    fn an_unknown_capability_name_is_refused_not_ignored() {
        let m = "name = \"x\"\ncaps = [\"print\", \"root\"]\n";
        assert!(parse_mcaps(m).is_err());
        // The failure must not partially apply what it did recognise.
        assert!(parse_mcaps("caps = [\"root\"]\n").is_err());
    }

    #[test]
    fn parsing_round_trips_every_name_in_the_table() {
        for &(name, bit) in MCAP_TABLE {
            let m = alloc_manifest(name);
            assert_eq!(parse_mcaps(&m), Ok(bit), "{name}");
        }
        assert_eq!(parse_mcaps("caps = []\n"), Ok(0));
        assert_eq!(parse_mcaps("name = \"x\"\n"), Ok(0));
    }

    fn alloc_manifest(cap: &str) -> String {
        std::format!("name = \"x\"\ncaps = [\"{cap}\"]\n")
    }

    #[test]
    fn names_render_for_denial_messages() {
        let mut s = String::new();
        mcap_names(MCAP_PRINT | MCAP_CAIRN_WRITE, &mut s);
        assert_eq!(s, "print cairn-write");
        let mut empty = String::new();
        mcap_names(0, &mut empty);
        assert_eq!(empty, "(none)");
    }

    // --- Escalation review ----------------------------------------------------

    /// The update path refuses a silent escalation, so `added` must be exact:
    /// never empty when the new set is wider, never populated when it is not.
    #[test]
    fn cap_delta_reports_escalation_exactly() {
        for old in 0..=MCAP_ALL {
            for new in 0..=MCAP_ALL {
                let (added, removed, kept) = cap_delta(old, new);
                assert_eq!(added & old, 0, "added bit was already held");
                assert_eq!(removed & new, 0, "removed bit is still held");
                assert_eq!(kept, old & new);
                assert_eq!(added | kept, new, "added+kept must reconstruct new");
                assert_eq!(removed | kept, old, "removed+kept must reconstruct old");
                assert_eq!(added.count_ones() > 0, (new & !old) != 0);
            }
        }
    }
}

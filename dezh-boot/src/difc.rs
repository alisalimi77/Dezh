//! Confidentiality and integrity enforcement on the storage path (DIFC).
//!
//! Each namespace carries a secrecy label. The console operator accumulates a
//! taint as it READS namespaces, and a commit is refused if the operator's taint
//! does not flow down into the target namespace (no write-down) - real
//! exfiltration prevention on the live Cairn path, with an explicit privileged
//! `declassify` escape hatch (the standard DIFC declassification). The dual
//! direction is integrity: bytes off the wire are not secret, they are
//! *unvalidated*, and `endorse` is the recorded act that lets them become
//! trusted state. Model in `dezh_core::difc`.
//!
//! Boot hart only: the label table is fixed at init and the taint tracked here
//! is the *console operator's*, so it moves only as console commands run. The
//! two demos that exercise this gate stay in `main.rs` for now - they reach
//! into Cairn and Marz, and they belong with the rest of `demos/`.

use crate::mm::global::Global;
use crate::{kprintln, record_event, CAIRN_NS_NAMES};

pub(crate) const NS_SECRET_VAULT: dezh_core::difc::Label = 1 << 0;
/// The endorsement a namespace can demand of anything written into it. A
/// namespace requiring it will not accept data derived from unvalidated input.
const NS_ENDORSED: dezh_core::difc::Integrity = 1 << 0;
// Boot hart only: the label table is fixed at init, and the operator taint is
// the *console operator's* label - it moves only as console commands run.
static NS_LABEL: Global<[dezh_core::difc::Label; 8]> = Global::new([0; 8]);
static NS_REQUIRES: Global<[dezh_core::difc::Integrity; 8]> = Global::new([0; 8]);
pub(crate) static OP_TAINT: Global<dezh_core::difc::Taint> = Global::new(dezh_core::difc::Taint::new());
static DIFC_INIT: Global<bool> = Global::new(false);

fn difc_init() {
    unsafe {
        if *DIFC_INIT.get() {
            return;
        }
        // vault (ns id 3) holds secrets; other namespaces are public here.
        (*NS_LABEL.get())[3] = NS_SECRET_VAULT;
        // note (0) and vault (3) are trusted state: they demand an endorsement,
        // so raw network input cannot become their content without review. lab
        // (1) is the scratch namespace and demands nothing.
        (*NS_REQUIRES.get())[0] = NS_ENDORSED;
        (*NS_REQUIRES.get())[3] = NS_ENDORSED;
        *DIFC_INIT.get() = true;
    }
}

pub(crate) fn ns_label(ns: usize) -> dezh_core::difc::Label {
    difc_init();
    unsafe { *(*NS_LABEL.get()).get(ns).unwrap_or(&0) }
}

pub(crate) fn ns_requires(ns: usize) -> dezh_core::difc::Integrity {
    difc_init();
    unsafe { *(*NS_REQUIRES.get()).get(ns).unwrap_or(&0) }
}

/// After a successful READ of `ns`, raise the operator's taint by that
/// namespace's secrecy label — reading a secret taints the reader.
pub(crate) fn difc_observe(ns: usize) {
    let l = ns_label(ns);
    if l != 0 {
        unsafe { (*OP_TAINT.get()).observe(l) };
        kprintln!(
            "[difc] operator tainted by reading a labelled namespace (secrecy now {:#x})",
            unsafe { (*OP_TAINT.get()).secrecy() }
        );
    }
}

/// Before a WRITE to `ns`, the operator's taint must flow down into the target
/// (`taint ⊆ ns label`); otherwise the write would exfiltrate a secret to a
/// lower sink. Prints an explainable denial and returns false when refused.
pub(crate) fn difc_may_write(ns: usize) -> bool {
    if !unsafe { (*OP_TAINT.get()).may_flow_to(ns_label(ns)) } {
        kprintln!(
            "[difc] DENIED: writing to ns='{}' would leak secret-tainted data to a lower sink (taint={:#x}, sink label={:#x}); declassify first",
            CAIRN_NS_NAMES.get(ns).copied().unwrap_or("?"),
            unsafe { (*OP_TAINT.get()).secrecy() },
            ns_label(ns)
        );
        return false;
    }
    // The other direction: data derived from unvalidated input must not become
    // trusted state. Secrecy alone never catches this — the bytes are not secret,
    // they are simply attacker-chosen.
    if !unsafe { (*OP_TAINT.get()).may_endorse_to(ns_requires(ns)) } {
        kprintln!(
            "[difc] DENIED: writing to ns='{}' would let UNVALIDATED input become trusted state (operator integrity={:#x}, sink requires={:#x}); endorse first",
            CAIRN_NS_NAMES.get(ns).copied().unwrap_or("?"),
            unsafe { (*OP_TAINT.get()).integrity() },
            ns_requires(ns)
        );
        return false;
    }
    true
}

/// Called after the operator consumes bytes that came from outside the machine.
/// Integrity can only fall this way; nothing but an explicit `endorse` raises it.
pub(crate) fn difc_ingress(source: &'static str) {
    difc_init();
    unsafe { (*OP_TAINT.get()).observe_input(dezh_core::difc::UNTRUSTED) };
    kprintln!(
        "[difc] operator integrity LOWERED by consuming input from {source} (integrity now {:#x}) -- it is not secret, it is unvalidated",
        unsafe { (*OP_TAINT.get()).integrity() }
    );
    record_event("kernel", "difc.ingress", source, "tainted");
}

pub(crate) fn declassify() {
    difc_init();
    let integrity = unsafe { (*OP_TAINT.get()).integrity() };
    unsafe { *OP_TAINT.get() = dezh_core::difc::Taint::new() };
    // Declassification is about secrecy only; it must not silently hand back
    // integrity the operator lost, or one privileged act would grant two.
    unsafe {
        if integrity != dezh_core::difc::TRUSTED {
            (*OP_TAINT.get()).observe_input(integrity);
        }
    }
    kprintln!("[declassify] operator taint cleared (privileged declassification)");
    record_event("kernel", "difc.declassify", "operator", "OK");
}

/// The dual of declassify: an explicit, recorded act saying the operator has
/// validated what it read from outside, restoring its integrity.
pub(crate) fn endorse() {
    difc_init();
    unsafe { (*OP_TAINT.get()).endorse() };
    kprintln!("[endorse] operator integrity restored (privileged endorsement of reviewed input)");
    record_event("kernel", "difc.endorse", "operator", "OK");
}

pub(crate) fn taint_show() {
    kprintln!(
        "[taint] operator secrecy taint = {:#x} (rises by reading secrets; cleared by declassify)",
        unsafe { (*OP_TAINT.get()).secrecy() }
    );
    kprintln!(
        "[taint] operator integrity     = {:#x} (falls by consuming outside input; restored by endorse)",
        unsafe { (*OP_TAINT.get()).integrity() }
    );
}

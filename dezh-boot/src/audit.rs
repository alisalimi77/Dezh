//! The event ledger: what happened, and why a denial happened.
//!
//! A bounded ring of `(tick, actor, action, target, result)` written by
//! `record_event` from every subsystem, and the `why-denied` walk that turns
//! the most recent denial into an explanation naming the boundary that refused
//! it. This is the provenance surface the whole review argument rests on.
//!
//! It lived under the "cooperative multitasking scheduler" banner, which is
//! where W11 found it. It has nothing to do with scheduling - it sat there
//! because `record_event` needs the tick counter and the tick counter was
//! nearby. Pulling it out first is what makes the remaining 1,400 lines under
//! that banner actually be a scheduler.
//!
//! Boot hart only: the ring is written on the console's own path and read back
//! by `why-denied` and `events`, both boot-hart commands.

use core::sync::atomic::Ordering;

use crate::arch::timer::TICKS;
use crate::mm::global::Global;
use crate::kprintln;

#[derive(Clone, Copy)]
struct EventEntry {
    tick: u64,
    actor: &'static str,
    action: &'static str,
    target: &'static str,
    result: &'static str,
}

const EMPTY_EVENT: EventEntry = EventEntry {
    tick: 0,
    actor: "",
    action: "",
    target: "",
    result: "",
};

const EVENT_CAP: usize = 32;
// Boot hart only: the event ring is written by record_event on the console's
// own path and read back by why-denied / events, both boot-hart commands.
static EVENTS: Global<[EventEntry; EVENT_CAP]> = Global::new([EMPTY_EVENT; EVENT_CAP]);
static EVENT_NEXT: Global<usize> = Global::new(0);
static EVENT_COUNT: Global<usize> = Global::new(0);

pub(crate) fn record_event(
    actor: &'static str,
    action: &'static str,
    target: &'static str,
    result: &'static str,
) {
    unsafe {
        (*EVENTS.get())[*EVENT_NEXT.get()] = EventEntry {
            tick: TICKS.load(Ordering::Relaxed),
            actor,
            action,
            target,
            result,
        };
        *EVENT_NEXT.get() = (*EVENT_NEXT.get() + 1) % EVENT_CAP;
        if *EVENT_COUNT.get() < EVENT_CAP {
            *EVENT_COUNT.get() += 1;
        }
    }
}

pub(crate) fn print_events() {
    unsafe {
        kprintln!("events:");
        kprintln!("  TICK   ACTOR      ACTION          TARGET          RESULT");
        let start = if *EVENT_COUNT.get() == EVENT_CAP {
            *EVENT_NEXT.get()
        } else {
            0
        };
        let mut n = 0usize;
        while n < *EVENT_COUNT.get() {
            let idx = (start + n) % EVENT_CAP;
            let e = (*EVENTS.get())[idx];
            kprintln!(
                "  {:<6} {:<10} {:<15} {:<15} {}",
                e.tick,
                e.actor,
                e.action,
                e.target,
                e.result
            );
            n += 1;
        }
        if *EVENT_COUNT.get() == 0 {
            kprintln!("  (no events recorded yet)");
        }
    }
}

/// A recorded event result that denotes a refusal/denial (not a success).
fn is_denial(result: &str) -> bool {
    matches!(
        result,
        "DENIED" | "TRAP" | "fail" | "escaped" | "REVIEW_REQUIRED" | "CORRUPT"
    )
}

/// Map an event's action to the Dezh enforcement boundary that produced it, so a
/// denial can be explained in terms of a real mechanism, not a policy string.
fn denial_boundary(action: &str) -> &'static str {
    if action.starts_with("intent") {
        "intent-derivation ceiling (derived cap <= Ahd), enforced in the kernel"
    } else if action.starts_with("sfar") || action.starts_with("tbar") {
        "mission authority: the caller must hold every namespace the mission touched"
    } else if action.starts_with("sand") || action.starts_with("cairn") {
        "storage-service capability check (kernel-attested namespace caps)"
    } else if action.starts_with("pkg") {
        "package manifest grants (no capability beyond the verified manifest)"
    } else if action.starts_with("cap") || action.starts_with("mmio") {
        "kernel capability check (no ambient authority to forge or amplify)"
    } else if action.starts_with("pol") {
        "Pol personality capability check (legacy syscalls are capability-gated)"
    } else if action.starts_with("redteam") {
        "adversary containment: an escape attempt stopped at a named boundary"
    } else {
        "kernel capability boundary"
    }
}

/// W8 P5: explain denials. Every important effect and refusal is recorded in the
/// in-kernel event ring; `why-denied` walks it newest-first and names the
/// boundary that produced each denial. A refusal is never a silent "no" — it is
/// attributable to a specific mechanism.
///
/// `why-denied`       explains the most recent denial (default).
/// `why-denied all`   lists every recent denial with its boundary (audit a whole
///                    agent run, e.g. after `overnight`).
pub(crate) fn why_denied(arg: &str) {
    let all = arg.trim() == "all";
    unsafe {
        if *EVENT_COUNT.get() == 0 {
            kprintln!("[why-denied] no events recorded yet");
            return;
        }
        let start = if *EVENT_COUNT.get() == EVENT_CAP { *EVENT_NEXT.get() } else { 0 };
        let mut found = 0usize;
        let mut k = *EVENT_COUNT.get();
        while k > 0 {
            k -= 1;
            let idx = (start + k) % EVENT_CAP;
            let e = (*EVENTS.get())[idx];
            if !is_denial(e.result) {
                continue;
            }
            found += 1;
            let label = if all { "denial" } else { "last denial" };
            kprintln!(
                "[why-denied] {label}: actor={} action={} target={} result={} (tick {})",
                e.actor,
                e.action,
                e.target,
                e.result,
                e.tick
            );
            kprintln!("[why-denied] boundary: {}", denial_boundary(e.action));
            if !all {
                kprintln!("[why-denied] policy: authority is explicit and unforgeable; nothing runs on ambient permission");
                return;
            }
        }
        if found == 0 {
            kprintln!(
                "[why-denied] no denial in the last {} events; every recent action was authorized",
                *EVENT_COUNT.get()
            );
        } else if all {
            kprintln!(
                "[why-denied] {found} denial(s) recorded; each attributable to a named boundary (no ambient authority)"
            );
        }
    }
}

pub(crate) fn print_audit() {
    kprintln!("audit summary:");
    kprintln!("  model: no ambient authority; important effects are event-recorded");
    kprintln!(
        "  tracked: install, app install/run/remove, service stop/restart/fault, denial demos"
    );
    print_events();
}

//! Preemptive scheduling on x86.
//!
//! There is nothing here yet but the hook the timer interrupt calls. It exists
//! now so that the change that lets an interrupt resume a *different* context
//! can be landed and tested while still resuming the same one every time.

use core::sync::atomic::{AtomicBool, Ordering};

/// Off until a scheduler is actually installed. While it is off the tick hook
/// hands back exactly the frame it was given, which is the old behaviour.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Called from the timer interrupt with interrupts off, given the interrupted
/// context's `rsp`. Returns the context to resume.
pub(crate) fn on_tick(frame: u64) -> u64 {
    if !ENABLED.load(Ordering::Relaxed) {
        return frame;
    }
    frame
}

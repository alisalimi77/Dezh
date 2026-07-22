//! Decentralized information-flow control (DIFC): the confidentiality primitive
//! that closes the **exfiltration** gap.
//!
//! The effect ledger (W8) is an *integrity* mechanism — it attributes and undoes
//! what an actor *did*. It cannot un-leak what an actor *read and sent*. The real
//! threat from an autonomous agent is exfiltration: read a secret it was granted,
//! then send it somewhere it should not go. Capability read-access control alone
//! does not stop this once the data is in hand.
//!
//! DIFC (Denning's lattice; Asbestos/HiStar/Flume, see `docs/RELATED_WORK.md`)
//! adds a **secrecy label** to each object and a **taint** to each actor:
//! reading an object *raises* the actor's taint, and the actor may only write to
//! a sink whose label can *hold* everything it is tainted with — no write-down,
//! so a secret cannot flow to a less-secret channel. This module is the
//! arch-independent, `no_std` primitive, host-tested here and driven by the
//! kernel `exfil-demo`.

/// A secrecy label: a set of secrecy tags. `PUBLIC` (empty) is the bottom of the
/// lattice; adding tags moves *up* (more secret). A sink's label is the set of
/// secrecy tags it is cleared to hold.
pub type Label = u32;

/// The bottom of the lattice — no secrecy tags. Anything may flow here only if it
/// is itself public.
pub const PUBLIC: Label = 0;

/// A flow from data labelled `src` to a sink labelled `sink` is permitted only if
/// the sink can hold every secrecy tag the source carries (`src ⊆ sink`). This is
/// the no-write-down rule: secret data cannot flow to a less-secret sink.
#[inline]
pub fn can_flow(src: Label, sink: Label) -> bool {
    src & !sink == 0
}

/// An actor's accumulated secrecy taint. It only ever rises as the actor reads
/// more-secret data, so its permitted set of sinks only ever shrinks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taint {
    secrecy: Label,
}

impl Default for Taint {
    fn default() -> Self {
        Self::new()
    }
}

impl Taint {
    /// A fresh, untainted (public) actor.
    pub const fn new() -> Self {
        Taint { secrecy: PUBLIC }
    }

    /// The actor reads/observes an object of label `object`; its taint rises to
    /// include that object's secrecy. Reading can never *lower* taint.
    pub fn observe(&mut self, object: Label) {
        self.secrecy |= object;
    }

    /// The actor's current secrecy taint.
    pub fn secrecy(&self) -> Label {
        self.secrecy
    }

    /// May this actor write/send to a sink labelled `sink`? Only if the sink can
    /// hold everything the actor is tainted with (`taint ⊆ sink`). A public sink
    /// (`PUBLIC`) accepts a write only from a still-public actor — so an actor
    /// that has read a secret is refused, blocking exfiltration.
    pub fn may_flow_to(&self, sink: Label) -> bool {
        can_flow(self.secrecy, sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_VAULT: Label = 1 << 0;
    const SECRET_KEYS: Label = 1 << 1;

    #[test]
    fn public_actor_may_write_anywhere() {
        let t = Taint::new();
        assert!(t.may_flow_to(PUBLIC));
        assert!(t.may_flow_to(SECRET_VAULT));
    }

    #[test]
    fn reading_a_secret_blocks_writing_to_a_public_sink() {
        // The exfiltration case: read vault, then try to send to a public sink.
        let mut t = Taint::new();
        t.observe(SECRET_VAULT);
        assert!(t.may_flow_to(SECRET_VAULT), "write-up/equal is allowed");
        assert!(
            !t.may_flow_to(PUBLIC),
            "a tainted actor must not leak a secret to a public sink"
        );
    }

    #[test]
    fn taint_only_rises() {
        let mut t = Taint::new();
        t.observe(SECRET_VAULT);
        let after_one = t.secrecy();
        t.observe(SECRET_KEYS);
        assert_eq!(t.secrecy(), after_one | SECRET_KEYS);
        // A sink that held the one-secret actor may no longer hold the two-secret
        // one unless it clears both.
        assert!(!t.may_flow_to(SECRET_VAULT));
        assert!(t.may_flow_to(SECRET_VAULT | SECRET_KEYS));
    }

    #[test]
    fn may_flow_iff_subset_exhaustive() {
        // Exhaustive over the 8-bit label space: a flow is permitted iff the
        // taint is a subset of the sink (no-write-down).
        for taint in 0u32..=255 {
            let mut t = Taint::new();
            t.observe(taint);
            for sink in 0u32..=255 {
                assert_eq!(t.may_flow_to(sink), (taint & !sink) == 0);
                assert_eq!(can_flow(taint, sink), (taint & !sink) == 0);
            }
        }
    }
}

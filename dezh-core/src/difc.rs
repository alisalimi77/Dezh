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

/// An **integrity** label: the set of endorsements a value carries. This is the
/// dual of secrecy and it is what the *network* needs.
///
/// Secrecy answers "may this leave?". It says nothing about data arriving from
/// outside, which is the opposite problem: bytes off the wire are attacker-chosen
/// and must not silently become trusted state (Biba's integrity lattice; the
/// endorsement half of HiStar/Flume). A sink can *require* endorsements; a value
/// may flow into it only if it carries them. Reading untrusted input can only
/// ever *lower* an actor's integrity, exactly as reading secrets only ever raises
/// its secrecy.
pub type Integrity = u32;

/// Carries every endorsement — the top of the integrity lattice.
pub const TRUSTED: Integrity = !0;

/// Carries none. Data straight off the network starts here.
pub const UNTRUSTED: Integrity = 0;

/// May a value whose endorsements are `data` flow into a sink that *requires*
/// `sink_requires`? Only if the value carries every endorsement demanded
/// (`sink_requires ⊆ data`). This is the no-write-**up** rule.
#[inline]
pub fn integrity_ok(data: Integrity, sink_requires: Integrity) -> bool {
    sink_requires & !data == 0
}

/// An actor's accumulated labels: secrecy that only rises as it reads secrets,
/// and integrity that only falls as it reads untrusted input. Both movements
/// shrink the set of sinks the actor may write to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taint {
    secrecy: Label,
    integrity: Integrity,
}

impl Default for Taint {
    fn default() -> Self {
        Self::new()
    }
}

impl Taint {
    /// A fresh actor: public, and fully endorsed until it reads something that
    /// is not.
    pub const fn new() -> Self {
        Taint {
            secrecy: PUBLIC,
            integrity: TRUSTED,
        }
    }

    /// The actor reads/observes an object of label `object`; its taint rises to
    /// include that object's secrecy. Reading can never *lower* taint.
    pub fn observe(&mut self, object: Label) {
        self.secrecy |= object;
    }

    /// The actor consumes input carrying only the endorsements `object`; its own
    /// integrity falls to the intersection. Reading untrusted input can never
    /// *raise* integrity — the dual of `observe`.
    pub fn observe_input(&mut self, object: Integrity) {
        self.integrity &= object;
    }

    /// The actor's current secrecy taint.
    pub fn secrecy(&self) -> Label {
        self.secrecy
    }

    /// The endorsements the actor still carries.
    pub fn integrity(&self) -> Integrity {
        self.integrity
    }

    /// May this actor write into a sink that *requires* `sink_requires`? Only if
    /// it still carries those endorsements. An actor that has consumed network
    /// input cannot write into a sink demanding an endorsement it lost.
    pub fn may_endorse_to(&self, sink_requires: Integrity) -> bool {
        integrity_ok(self.integrity, sink_requires)
    }

    /// Privileged **endorsement**: restore the actor's integrity. The exact dual
    /// of declassification — an explicit, auditable act by someone who has
    /// validated the input, never something that happens implicitly.
    pub fn endorse(&mut self) {
        self.integrity = TRUSTED;
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

    // --- Integrity: the ingress half. -------------------------------------
    const ENDORSED: Integrity = 1 << 0;
    const REVIEWED: Integrity = 1 << 1;

    #[test]
    fn a_fresh_actor_carries_every_endorsement() {
        let t = Taint::new();
        assert!(t.may_endorse_to(ENDORSED));
        assert!(t.may_endorse_to(ENDORSED | REVIEWED));
    }

    #[test]
    fn consuming_network_input_blocks_writing_to_a_trusted_sink() {
        // The ingress case: read bytes off the wire, then try to write them into
        // a namespace that requires an endorsement.
        let mut t = Taint::new();
        t.observe_input(UNTRUSTED);
        assert!(t.may_endorse_to(UNTRUSTED), "an unendorsed sink still accepts it");
        assert!(
            !t.may_endorse_to(ENDORSED),
            "unvalidated network input must not become trusted state"
        );
    }

    #[test]
    fn integrity_only_falls_until_explicitly_endorsed() {
        let mut t = Taint::new();
        t.observe_input(ENDORSED | REVIEWED);
        assert!(t.may_endorse_to(ENDORSED));
        t.observe_input(ENDORSED); // reading something less endorsed
        assert!(!t.may_endorse_to(REVIEWED), "integrity cannot rise by reading");
        assert!(t.may_endorse_to(ENDORSED));
        t.endorse();
        assert!(t.may_endorse_to(ENDORSED | REVIEWED), "endorsement is the escape");
    }

    #[test]
    fn the_two_axes_are_independent() {
        // Reading a secret must not change integrity, and reading untrusted input
        // must not change secrecy — otherwise one gate could mask the other.
        let mut t = Taint::new();
        t.observe(SECRET_VAULT);
        assert_eq!(t.integrity(), TRUSTED);
        let mut u = Taint::new();
        u.observe_input(UNTRUSTED);
        assert_eq!(u.secrecy(), PUBLIC);
    }

    #[test]
    fn integrity_ok_iff_superset_exhaustive() {
        // Exhaustive over the 8-bit space: a write is permitted iff the value
        // carries every endorsement the sink requires (no-write-up).
        for data in 0u32..=255 {
            let mut t = Taint::new();
            t.observe_input(data);
            for requires in 0u32..=255 {
                assert_eq!(t.may_endorse_to(requires), (requires & !data) == 0);
                assert_eq!(integrity_ok(data, requires), (requires & !data) == 0);
            }
        }
    }
}

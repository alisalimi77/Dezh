//! Package signing: bind the *authority* a package requests, not just its bytes.
//!
//! See `docs/PACKAGE_SIGNING.md` for the full design and the supply-chain
//! mistakes this avoids. The crypto is Ed25519 via the reputable, zero-
//! dependency, `no_std` `ed25519-compact` crate — **never** a hand-rolled
//! implementation. The kernel only *verifies* (deterministic, no RNG); signing
//! lives in the host SDK.
//!
//! The signed message binds the payload to its requested authority:
//!
//! ```text
//! SIG_MSG = payload_bytes || "DZSIG1"
//!           || len(name)    || name
//!           || len(version) || version
//!           || counter (u64 LE)      // monotonic per name (anti-rollback)
//!           || kind    (u16 LE)
//!           || caps    (u32 LE)      // the requested capability bitmask
//! ```
//!
//! Because `caps` is inside the signed message, a signature is a precise claim:
//! *"signer S authorizes name@version (seq counter) to request exactly caps."*
//! Tampering with the requested capabilities breaks the signature.

use ed25519_compact::{PublicKey, Signature};

pub const SIG_MAGIC: &[u8; 6] = b"DZSIG1";
pub const PK_LEN: usize = 32;
pub const SIG_LEN: usize = 64;

/// Upper bounds for the signed header's variable fields (the package registry
/// uses shorter limits still). Keeps the header buffer a fixed small size.
pub const NAME_MAX: usize = 64;
pub const VERSION_MAX: usize = 32;
/// Enough for the magic + length-prefixed name/version + counter/kind/caps.
pub const SIG_HEADER_MAX: usize = 6 + 1 + NAME_MAX + 1 + VERSION_MAX + 8 + 2 + 4;

/// Write the canonical signed *header* (everything after the payload bytes) into
/// `out`, returning its length. The full signed message is `payload || header`,
/// so a caller that already holds the payload only appends this. Returns `None`
/// if a field is over length or `out` is too small.
pub fn sig_header(
    out: &mut [u8],
    name: &[u8],
    version: &[u8],
    counter: u64,
    kind: u16,
    caps: u32,
) -> Option<usize> {
    if name.len() > NAME_MAX || version.len() > VERSION_MAX {
        return None;
    }
    let total = 6 + 1 + name.len() + 1 + version.len() + 8 + 2 + 4;
    if out.len() < total {
        return None;
    }
    let mut p = 0usize;
    out[p..p + 6].copy_from_slice(SIG_MAGIC);
    p += 6;
    out[p] = name.len() as u8;
    p += 1;
    out[p..p + name.len()].copy_from_slice(name);
    p += name.len();
    out[p] = version.len() as u8;
    p += 1;
    out[p..p + version.len()].copy_from_slice(version);
    p += version.len();
    out[p..p + 8].copy_from_slice(&counter.to_le_bytes());
    p += 8;
    out[p..p + 2].copy_from_slice(&kind.to_le_bytes());
    p += 2;
    out[p..p + 4].copy_from_slice(&caps.to_le_bytes());
    p += 4;
    Some(p)
}

/// Verify `sig` over `msg` under public key `pk`. Verification only — no
/// allocation, no RNG, deterministic. Returns `true` iff the signature is valid.
pub fn verify(pk: &[u8; PK_LEN], sig: &[u8; SIG_LEN], msg: &[u8]) -> bool {
    matches!(
        PublicKey::new(*pk).verify(msg, &Signature::new(*sig)),
        Ok(())
    )
}

/// Publisher **capability attenuation**: the authority a signed package may
/// actually receive is the structural subset of what it requested with the
/// signer key's ceiling — `granted = requested ∩ signer_ceiling`. This is the
/// W8 `derived ⊆ intent` rule applied to the supply chain: a publisher key can
/// never authorize authority beyond the ceiling its root granted it.
#[inline]
pub fn attenuate(requested_caps: u32, signer_ceiling: u32) -> u32 {
    requested_caps & signer_ceiling
}

/// The capabilities that were requested but lie beyond the signer's ceiling —
/// dropped and reported at install time, never silently granted.
#[inline]
pub fn beyond_ceiling(requested_caps: u32, signer_ceiling: u32) -> u32 {
    requested_caps & !signer_ceiling
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    extern crate std;
    use std::vec::Vec;

    fn arr32(s: &[u8]) -> [u8; 32] {
        let mut a = [0u8; 32];
        a.copy_from_slice(s);
        a
    }
    fn arr64(s: &[u8]) -> [u8; 64] {
        let mut a = [0u8; 64];
        a.copy_from_slice(s);
        a
    }

    /// Build the full signed message `payload || header` the way both the SDK
    /// and the kernel do.
    fn message(payload: &[u8], name: &[u8], version: &[u8], counter: u64, kind: u16, caps: u32) -> Vec<u8> {
        let mut hdr = [0u8; SIG_HEADER_MAX];
        let n = sig_header(&mut hdr, name, version, counter, kind, caps).unwrap();
        let mut m = Vec::new();
        m.extend_from_slice(payload);
        m.extend_from_slice(&hdr[..n]);
        m
    }

    fn keypair(seed: u8) -> KeyPair {
        KeyPair::from_seed(Seed::new([seed; 32]))
    }

    #[test]
    fn valid_signature_verifies() {
        let kp = keypair(1);
        let pk = arr32(kp.pk.as_ref());
        let msg = message(b"agent-ir-bytes", b"agent", b"1.0.0", 1, 1, 0b10011);
        let sig = arr64(kp.sk.sign(&msg, None).as_ref());
        assert!(verify(&pk, &sig, &msg));
    }

    #[test]
    fn tampered_capabilities_break_the_signature() {
        // The whole point: change the REQUESTED CAPABILITIES and the signature
        // must fail — authority is bound, not just bytes.
        let kp = keypair(2);
        let pk = arr32(kp.pk.as_ref());
        let good = message(b"payload", b"app", b"1.0.0", 1, 1, 0b00001);
        let sig = arr64(kp.sk.sign(&good, None).as_ref());
        let tampered = message(b"payload", b"app", b"1.0.0", 1, 1, 0b11111);
        assert!(verify(&pk, &sig, &good));
        assert!(!verify(&pk, &sig, &tampered));
    }

    #[test]
    fn tampered_payload_breaks_the_signature() {
        let kp = keypair(3);
        let pk = arr32(kp.pk.as_ref());
        let good = message(b"payload-A", b"app", b"1.0.0", 1, 1, 1);
        let sig = arr64(kp.sk.sign(&good, None).as_ref());
        let other = message(b"payload-B", b"app", b"1.0.0", 1, 1, 1);
        assert!(!verify(&pk, &sig, &other));
    }

    #[test]
    fn rollback_counter_is_bound() {
        let kp = keypair(4);
        let pk = arr32(kp.pk.as_ref());
        let v2 = message(b"p", b"app", b"2.0.0", 2, 1, 1);
        let sig = arr64(kp.sk.sign(&v2, None).as_ref());
        // A signature for counter=2 cannot be replayed for counter=1.
        let v1 = message(b"p", b"app", b"2.0.0", 1, 1, 1);
        assert!(!verify(&pk, &sig, &v1));
    }

    #[test]
    fn wrong_key_fails() {
        let signer = keypair(5);
        let attacker = keypair(6);
        let msg = message(b"p", b"app", b"1.0.0", 1, 1, 1);
        let sig = arr64(signer.sk.sign(&msg, None).as_ref());
        assert!(!verify(&arr32(attacker.pk.as_ref()), &sig, &msg));
    }

    #[test]
    fn attenuation_is_a_subset_of_both_ceiling_and_request() {
        // Exhaustive over the 8-bit capability space (proof by enumeration):
        // a publisher can never authorize authority beyond its ceiling.
        for req in 0u32..=255 {
            for ceil in 0u32..=255 {
                let g = attenuate(req, ceil);
                assert_eq!(g & !ceil, 0, "granted exceeded the signer ceiling");
                assert_eq!(g & !req, 0, "granted exceeded the request");
                // granted + beyond partition the request, without overlap.
                assert_eq!(g | beyond_ceiling(req, ceil), req);
                assert_eq!(g & beyond_ceiling(req, ceil), 0);
            }
        }
    }

    #[test]
    fn header_is_deterministic() {
        let mut a = [0u8; SIG_HEADER_MAX];
        let mut b = [0u8; SIG_HEADER_MAX];
        let na = sig_header(&mut a, b"app", b"1.0.0", 7, 1, 3).unwrap();
        let nb = sig_header(&mut b, b"app", b"1.0.0", 7, 1, 3).unwrap();
        assert_eq!(&a[..na], &b[..nb]);
    }
}

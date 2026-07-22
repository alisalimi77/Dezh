//! Package signing: bind the *authority* a package requests, not just its bytes.
//!
//! See `docs/PACKAGE_SIGNING.md` for the full design and the supply-chain
//! mistakes this avoids. The crypto is Ed25519 via the reputable, zero-
//! dependency, `no_std` `ed25519-compact` crate — **never** a hand-rolled
//! implementation. The kernel only *verifies* (deterministic, no RNG); signing
//! lives in the host SDK.
//!
//! The signed message binds the **whole inner `.dzp`** to a monotonic counter:
//!
//! ```text
//! SIG_MSG = inner_dzp_bytes || "DZSIG1" || counter (u64 LE)
//! ```
//!
//! The requested authority is bound because the capabilities live in the
//! manifest *inside* `inner_dzp_bytes` — any change to name, version, kind,
//! payload, or the requested `caps` changes the signed bytes and breaks the
//! signature. The counter is a per-name monotonic sequence, so a signature for
//! one version cannot be replayed for an older one (anti-rollback). Signing the
//! artifact bytes rather than a re-parsed field list also avoids any
//! serialization mismatch between the host SDK (Python) and the kernel (Rust).

use ed25519_compact::{PublicKey, Signature};

pub const SIG_MAGIC: &[u8; 6] = b"DZSIG1";
pub const PK_LEN: usize = 32;
pub const SIG_LEN: usize = 64;

/// Length of the trailing signed context (`"DZSIG1" || counter`).
pub const SIG_CONTEXT_LEN: usize = 6 + 8;

/// Write the trailing signed context — `"DZSIG1" || counter(u64 LE)` — into
/// `out`, returning its length. The full signed message is
/// `inner_dzp_bytes || context`, so a caller that already holds the inner
/// package bytes only appends this small suffix. Returns `None` if `out` is too
/// small.
pub fn signed_context(out: &mut [u8], counter: u64) -> Option<usize> {
    if out.len() < SIG_CONTEXT_LEN {
        return None;
    }
    out[0..6].copy_from_slice(SIG_MAGIC);
    out[6..14].copy_from_slice(&counter.to_le_bytes());
    Some(SIG_CONTEXT_LEN)
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

// --- Signed-package envelope (DZSP) -------------------------------------------
//
// A signed package WRAPS an unsigned inner `.dzp`, so signing is a clean layer
// that never touches the core package format — the F3 byte-pinning of the inner
// package still holds, and an unsigned `.dzp` remains valid on its own.
//
// Layout (little-endian):
//
// ```text
// 0    magic "DZSP"             (4)
// 4    u16   envelope version   (2)  = 1
// 6    u16   flags / reserved   (2)  = 0
// 8    [u8;32] signer pubkey    (32)  (the key id IS the public key)
// 40   [u8;64] signature        (64)  over  inner_dzp || "DZSIG1" || counter
// 104  u64   counter            (8)
// 112  u32   inner .dzp length  (4)
// 116  inner .dzp bytes         (var)
// ```

pub const ENV_MAGIC: &[u8; 4] = b"DZSP";
pub const ENV_VERSION: u16 = 1;
pub const ENV_HEADER_LEN: usize = 116;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvError {
    TooShort,
    BadMagic,
    BadVersion,
    Truncated,
}

impl EnvError {
    pub fn msg(self) -> &'static str {
        match self {
            EnvError::TooShort => "shorter than the signed-envelope header",
            EnvError::BadMagic => "not a signed package (bad DZSP magic)",
            EnvError::BadVersion => "unsupported signed-envelope version",
            EnvError::Truncated => "declared inner length exceeds the envelope",
        }
    }
}

/// A parsed signed envelope. Fields are copied out (no borrow) so a caller may
/// mutate the underlying buffer afterwards — e.g. append the signed context in
/// place before verifying.
#[derive(Clone, Copy, Debug)]
pub struct SignedEnvelope {
    pub signer_pk: [u8; PK_LEN],
    pub signature: [u8; SIG_LEN],
    pub counter: u64,
    pub inner_offset: usize,
    pub inner_len: usize,
}

/// True if `bytes` looks like a signed envelope (has the DZSP magic).
pub fn is_signed(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[0..4] == ENV_MAGIC
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_le_bytes(a)
}

/// Parse a signed-package envelope header. Does NOT verify the signature — the
/// caller assembles `inner || context` and calls [`verify`].
pub fn parse_envelope(bytes: &[u8]) -> Result<SignedEnvelope, EnvError> {
    if bytes.len() < ENV_HEADER_LEN {
        return Err(EnvError::TooShort);
    }
    if &bytes[0..4] != ENV_MAGIC {
        return Err(EnvError::BadMagic);
    }
    if u16le(bytes, 4) != ENV_VERSION {
        return Err(EnvError::BadVersion);
    }
    let inner_len = u32le(bytes, 112) as usize;
    let total = ENV_HEADER_LEN.checked_add(inner_len).ok_or(EnvError::Truncated)?;
    if bytes.len() != total {
        return Err(EnvError::Truncated);
    }
    let mut signer_pk = [0u8; PK_LEN];
    signer_pk.copy_from_slice(&bytes[8..40]);
    let mut signature = [0u8; SIG_LEN];
    signature.copy_from_slice(&bytes[40..104]);
    Ok(SignedEnvelope {
        signer_pk,
        signature,
        counter: u64le(bytes, 104),
        inner_offset: ENV_HEADER_LEN,
        inner_len,
    })
}

/// Encode a signed envelope into `out`, returning its total length. The SDK path
/// (host) uses this after signing `inner || context`. `out` must hold at least
/// `ENV_HEADER_LEN + inner.len()`.
pub fn pack_envelope(
    out: &mut [u8],
    signer_pk: &[u8; PK_LEN],
    signature: &[u8; SIG_LEN],
    counter: u64,
    inner: &[u8],
) -> Option<usize> {
    let total = ENV_HEADER_LEN + inner.len();
    if out.len() < total {
        return None;
    }
    out[0..4].copy_from_slice(ENV_MAGIC);
    out[4..6].copy_from_slice(&ENV_VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..40].copy_from_slice(signer_pk);
    out[40..104].copy_from_slice(signature);
    out[104..112].copy_from_slice(&counter.to_le_bytes());
    out[112..116].copy_from_slice(&(inner.len() as u32).to_le_bytes());
    out[ENV_HEADER_LEN..total].copy_from_slice(inner);
    Some(total)
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

    /// Build the full signed message `inner_dzp || context` the way both the SDK
    /// and the kernel do.
    fn message(inner_dzp: &[u8], counter: u64) -> Vec<u8> {
        let mut ctx = [0u8; SIG_CONTEXT_LEN];
        let n = signed_context(&mut ctx, counter).unwrap();
        let mut m = Vec::new();
        m.extend_from_slice(inner_dzp);
        m.extend_from_slice(&ctx[..n]);
        m
    }

    fn keypair(seed: u8) -> KeyPair {
        KeyPair::from_seed(Seed::new([seed; 32]))
    }

    // A stand-in inner .dzp whose manifest text carries the requested caps.
    const INNER: &[u8] = b"DZP1...manifest: caps=[print,cairn-write]...payload-bytes";

    #[test]
    fn valid_signature_verifies() {
        let kp = keypair(1);
        let pk = arr32(kp.pk.as_ref());
        let msg = message(INNER, 1);
        let sig = arr64(kp.sk.sign(&msg, None).as_ref());
        assert!(verify(&pk, &sig, &msg));
    }

    #[test]
    fn tampered_capabilities_break_the_signature() {
        // The requested capabilities live in the manifest inside the inner .dzp;
        // changing them changes the signed bytes and the signature must fail.
        let kp = keypair(2);
        let pk = arr32(kp.pk.as_ref());
        let good = message(INNER, 1);
        let sig = arr64(kp.sk.sign(&good, None).as_ref());
        let mut inner2 = INNER.to_vec();
        // flip a byte inside the "caps=[...]" region
        let pos = INNER
            .windows(5)
            .position(|w| w == b"caps=")
            .unwrap()
            + 6;
        inner2[pos] ^= 0x20;
        let tampered = message(&inner2, 1);
        assert!(verify(&pk, &sig, &good));
        assert!(!verify(&pk, &sig, &tampered));
    }

    #[test]
    fn tampered_payload_breaks_the_signature() {
        let kp = keypair(3);
        let pk = arr32(kp.pk.as_ref());
        let good = message(b"inner-A", 1);
        let sig = arr64(kp.sk.sign(&good, None).as_ref());
        let other = message(b"inner-B", 1);
        assert!(!verify(&pk, &sig, &other));
    }

    #[test]
    fn rollback_counter_is_bound() {
        let kp = keypair(4);
        let pk = arr32(kp.pk.as_ref());
        let v2 = message(INNER, 2);
        let sig = arr64(kp.sk.sign(&v2, None).as_ref());
        // A signature for counter=2 cannot be replayed for counter=1.
        let v1 = message(INNER, 1);
        assert!(!verify(&pk, &sig, &v1));
    }

    #[test]
    fn wrong_key_fails() {
        let signer = keypair(5);
        let attacker = keypair(6);
        let msg = message(INNER, 1);
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
    fn context_is_deterministic() {
        let mut a = [0u8; SIG_CONTEXT_LEN];
        let mut b = [0u8; SIG_CONTEXT_LEN];
        let na = signed_context(&mut a, 7).unwrap();
        let nb = signed_context(&mut b, 7).unwrap();
        assert_eq!(&a[..na], &b[..nb]);
    }

    /// The full envelope round trip the way the SDK builds and the kernel reads:
    /// sign `inner || context`, pack the envelope, parse it, reassemble the same
    /// message from the parsed fields, and verify. Then prove a tampered inner is
    /// rejected.
    #[test]
    fn envelope_round_trip_and_tamper() {
        let kp = keypair(11);
        let counter = 3u64;
        let inner = INNER;

        // SDK side: sign inner||context, then pack the envelope.
        let msg = message(inner, counter);
        let sig = arr64(kp.sk.sign(&msg, None).as_ref());
        let pk = arr32(kp.pk.as_ref());
        let mut env = std::vec![0u8; ENV_HEADER_LEN + inner.len()];
        let n = pack_envelope(&mut env, &pk, &sig, counter, inner).unwrap();

        // Kernel side: parse, reassemble inner||context, verify with the
        // envelope's own pubkey/signature/counter.
        let e = parse_envelope(&env[..n]).unwrap();
        assert_eq!(e.counter, counter);
        assert_eq!(&e.signer_pk, &pk);
        let parsed_inner = &env[e.inner_offset..e.inner_offset + e.inner_len];
        assert_eq!(parsed_inner, inner);
        let check = message(parsed_inner, e.counter);
        assert!(verify(&e.signer_pk, &e.signature, &check));

        // Tamper one inner byte -> verification must fail.
        let mut bad = env[..n].to_vec();
        bad[ENV_HEADER_LEN] ^= 0xFF;
        let eb = parse_envelope(&bad).unwrap();
        let bad_inner = &bad[eb.inner_offset..eb.inner_offset + eb.inner_len];
        let bad_msg = message(bad_inner, eb.counter);
        assert!(!verify(&eb.signer_pk, &eb.signature, &bad_msg));
    }

    #[test]
    fn is_signed_detects_envelope() {
        assert!(is_signed(ENV_MAGIC));
        assert!(!is_signed(b"DZP1and more"));
        assert!(!is_signed(b"xy"));
    }
}

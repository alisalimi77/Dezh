# Package Signing in Dezh — a principled, capability-native design

This document specifies how Dezh signs and verifies `.dzp` packages. It is
written the way the rest of the project is: we first study, precisely, the
mistakes real package-signing systems have made, then design so we do not repeat
them — and we add the one thing that is only possible on a capability substrate,
which is **signing the *authority* a package requests, not merely its bytes.**

Status: **design + phased implementation.** What is built vs designed is marked
in §7. It follows D015 (no claim beyond what is enforced) and the no-ambient-
authority thesis.

---

## 1. Why the current story is a real gap

Today a `.dzp` package is CRC32-checked and manifest-verified — this catches
*accidental* corruption but not *forgery*: anyone who alters a package can
recompute its CRC. For a system whose thesis is "no authority without explicit
provenance," an unsigned package is a structural contradiction: an app *requests
capabilities* in its manifest, but the request's author is unattributable. This
is the gap `docs/STATUS.md` names, and it is the one we close here.

## 2. Mistakes we studied, and how Dezh avoids each

| Mistake in the wild | Consequence | How Dezh avoids it |
| --- | --- | --- |
| **Sign the artifact bytes, not the metadata** (early apt/yum, npm) | Rollback, freeze, and mix-and-match attacks — the version/dependency/permission metadata is unprotected ([TUF][tuf]). | The signature covers a **canonical serialization of the whole manifest** — name, version, a **monotonic counter**, payload kind, and the **requested capability set** — plus the payload hash. Changing any of them invalidates the signature. |
| **A trusted signer is trusted with *unbounded* authority** (xz / CVE-2024-3094: a maintainer social-engineered for two years, then shipped a backdoor in a validly-built release) ([OpenSSF][xz]) | A signed package still receives full ambient authority; one malicious/compromised signer = total compromise. | **Signing is provenance, not safety.** A signed package still receives *only* the capabilities its manifest requests, those are **bounded by the signer's own capability ceiling** (§4), and every effect it makes is ledgered and reversible (W8). Dezh's defense is layered; the signature is one layer, not the wall. |
| **A single, long-lived, online signing key** (code-signing guidance) ([Keyfactor][kf]) | One key compromise signs everything, forever. | **Role separation:** an *offline root* key authorizes *publisher* keys; each publisher key is scoped (a capability ceiling), rotatable, and independently revocable. |
| **No, or slow, revocation** — a compromised cert keeps being honored ([AppViewX][avx]) | Users keep trusting malicious software after compromise is known. | A signer key is a **trust-store entry**; revoking it is an explicit, **ledgered** effect, and a revoked key's future installs are rejected. This is the same lease/revocation principle Dezh already applies to intents. |
| **Signing is opaque and unauditable** — you cannot tell what was signed, by whom, when (Sigstore's motivation for the Rekor **transparency log**) ([Sigstore][sig]) | No accountability; silent key abuse. | An install **is a Sand effect on the ledger**: `installer → signer identity → package → granted caps`. The provenance graph (`tbar`) answers "who authorized this app's authority." The ledger *is* the transparency log, native to the system. |
| **Roll-your-own crypto** | Subtle, catastrophic bugs. | We use an **audited** Ed25519 implementation ([RustCrypto `ed25519-dalek`]), never a hand-rolled one, isolated in one module shared by the SDK (signing) and the kernel (verification only — deterministic, no RNG in the kernel). |
| **TOCTOU: verify one copy, execute another** | The verified bytes are not the run bytes. | The signature is verified over the **exact staged blob** at install time, and the registry independently re-hashes the blob on every load (existing behavior). |

## 3. What is signed (bind authority, not just bytes)

The signed message is a canonical, length-prefixed serialization:

```
SIG_MSG = "DZSIG1" ||
          payload_hash (FNV/SHA of the payload bytes) ||
          len(name)    || name ||
          len(version) || version ||
          counter (u64, monotonic per name) ||
          kind (u16) ||
          caps (u32 manifest capability bitmask)
```

The **capabilities are inside the signed message.** No other package format does
this, because no other package format treats requested authority as a
first-class, install-time value. A signature therefore attests a precise claim:

> *Signer S authorizes `name@version` (sequence `counter`) to request exactly
> capability set `caps`.*

Tampering with the requested capabilities — the most security-relevant field —
breaks the signature.

## 4. The novel part: publisher capability attenuation

This is the W8 authority rule (`derived ⊆ intent`) applied to the **supply
chain**. Every publisher key in the trust store carries a **capability ceiling**
— the maximum authority that key is trusted to authorize. Install enforces:

```
granted_caps  =  requested_caps ∩ signer_ceiling         (structural subset)
```

Exactly as an intent bounds a running agent, a **publisher key bounds what
authority it may ever put into the world.** A key trusted only for `print +
cairn` *cannot* sign a package that receives device, MMIO, or DMA authority —
the excess is dropped and reported, the same way `intent-run` drops beyond-intent
capability. The confused-deputy and over-privileged-publisher problems dissolve:
a publisher can never escalate a package beyond the ceiling the root granted the
publisher's key.

This is the same algebra proved exhaustively in `dezh-kernel::authority`; package
signing is that algebra at a new layer, so the invariant "authority can only ever
be a subset of what authorized it" now holds from **root → publisher → package →
running app → effect**, unbroken.

## 5. Trust model and roles

- **Root key (offline).** The anchor. Signs the trust store: the set of trusted
  **publisher keys**, each with its capability ceiling and status (live/revoked).
  The root is never online; it only re-signs the trust store when publishers
  change. Compromise of a publisher key cannot forge a new trusted publisher.
- **Publisher keys.** Sign packages. Scoped by a ceiling, revocable, rotatable.
- **Verifier (the Dezh kernel).** Holds the root public key (measured/pinned).
  On install it: verifies the trust store against the root; looks up the signing
  publisher; verifies the package signature over `SIG_MSG`; enforces
  `granted = requested ∩ signer_ceiling`; and records the install as a Sand
  effect. The kernel only *verifies* — it never holds a private key.

## 6. Install becomes a ledgered, attributable effect

When a signed package installs, Dezh writes a Sand effect that binds the granted
authority to the signer:

```
actor = installer
intent/authority-source = signer key id
effect = "installed name@version, granted caps = C (⊆ signer ceiling)"
reversibility = reversible (an install can be rolled back)
```

So `tbar` and the audit surface answer, unforgeably, *who authorized the
authority this app holds* — the property Sigstore approximates with an external
transparency log, here intrinsic to the OS because the OS already has an
unbypassable effect ledger.

## 7. Defense in depth — the honest, layered claim

The xz backdoor is the cautionary tale: a **validly signed** artifact from a
**trusted** maintainer was still malicious. Signing did not, and cannot, prevent
that. What Dezh adds is that even a validly-signed malicious package:

1. receives **only** the capabilities its (signed) manifest requested,
2. bounded by its **publisher's ceiling** (a `cairn`-only publisher cannot ship
   a package that touches devices),
3. runs with **no ambient authority** to escalate from,
4. has **every effect ledgered** and attributable, and
5. is **reversible as a mission** (retract / compensate / refuse).

Package signing on npm/PyPI/apt gives a signed package the host's full ambient
authority — so an xz-style compromise is game over. On Dezh, signing is the
*provenance* layer of a stack whose *confinement* and *accountability* layers do
not depend on the signer being honest. **That layering — not the signature
alone — is the actual security claim, and it is only possible because the
substrate has no ambient authority.**

## 8. Implementation phases

- **P1 — crypto core.** Ed25519 verify via the audited RustCrypto crate, wrapped
  in `dezh-core`, host-tested (known-answer vectors), building for the bare-metal
  target. *No hand-rolled crypto.*
- **P2 — signed `.dzp`.** The `SIG_MSG` canonicalization + an appended signature
  block; an SDK signing tool; the packer/parser round-trip pinned by a test.
- **P3 — kernel enforcement.** Trust store (root-anchored publisher keys +
  ceilings + revocation), install-time verify, `granted = requested ∩ ceiling`,
  install-as-Sand-effect, a `sig-demo` (a good signature installs attenuated; a
  tampered payload/manifest is rejected; a beyond-ceiling request is attenuated;
  a revoked key is refused), and CI legs.

Each phase is a separate, CI-green commit, in the disciplined style of W8.

### Explicit non-goals (honest scope)

No online PKI, no certificate transparency service, no threshold signatures
(single root key for the prototype), no hardware key storage, no timestamping
authority. These are the production hardening beyond a reviewable prototype; the
*mechanism and the capability-native attenuation* are the contribution.

<!-- sources -->
[tuf]: https://theupdateframework.io/docs/security/
[xz]: https://openssf.org/blog/2024/03/30/xz-backdoor-cve-2024-3094/
[sig]: https://blog.sigstore.dev/the-update-framework-and-you-2f5cbaa964d5/
[kf]: https://www.keyfactor.com/blog/code-signing-101-locking-down-your-software-supply-chain/
[avx]: https://www.appviewx.com/blogs/beware-of-expired-or-compromised-code-signing-certificates/
[RustCrypto `ed25519-dalek`]: https://github.com/dalek-cryptography/curve25519-dalek

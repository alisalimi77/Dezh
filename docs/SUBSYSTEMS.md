# Subsystem designs

Design notes for the subsystems that carry their own trust argument. Each is
self-contained.

---

## Marz: guarded egress

<!-- was docs/MARZ.md until the 2026-07-23 consolidation -->

**Marz** (*border*) is the boundary an effect crosses to leave the machine.
Crossing it is irreversible: once bytes are on the wire, no ledger can call them
back. So Marz is where Dezh's whole stack has to hold at once — capability,
intent, information flow, and effect accountability.

This document follows the project method: study what existing systems do and
where they fail, state the precise delta, then design. Status: **design +
phased implementation**; §6 marks what is built.

---

### 1. What the field already does

| System | Network access model | Source |
| --- | --- | --- |
| **seL4 / Genode / Fuchsia** | The protocol stack runs in **user space** (lwIP/PicoTCP). An application has **no direct channel to the NIC driver**; it reaches the network only by capability-protected IPC to the stack. | [seL4 whitepaper][sel4] |
| **HiStar / Flume (DIFC)** | Data carries **labels**; exporting data *out of the system* is a **declassification** that only a privileged principal may perform. Flume gives each tag two capabilities (`t+`/`t−`) for declassify/endorse. | [Flume, SOSP'07][flume] |
| **Linux / Windows** | **Ambient authority**: a process names a destination and connects. Authorization is a global property of the process, not of the *destination*. | [Ambient authority][ambient] |

Read together, the field has solved two different halves:

- capability systems confine **access to the device/stack**, and
- DIFC systems constrain **which data may flow out**,

while mainstream systems do neither per-destination — which is exactly the
exfiltration channel: a compromised process connects anywhere, and nothing
records *which* destination on whose authority.

### 2. The mistakes we design against

1. **Ambient egress.** "Any process may connect anywhere." → In Marz the
   **destination is part of the capability**, not a parameter the caller picks
   freely.
2. **Access control without flow control.** Confining *who may use the NIC* does
   not stop a permitted principal from shipping a secret. → Marz applies the
   DIFC rule on export: a tainted actor may not send to a lower-secrecy
   destination without an explicit **declassification** (the Flume lesson).
3. **Flow control without accountability.** A label check leaves no record of
   *what left, under whose intent*. → Every send is a **Sand effect**:
   `actor → intent → derived cap → destination → irreversible`.
4. **Pretending egress is reversible.** Rollback machinery that "undoes" a send
   is a lie. → Marz effects are classified **irreversible**; `sfar-rollback`
   refuses them with an explanation, exactly as it already does for the modeled
   external effects.
5. **A side-channel audit log.** A log beside the socket can be bypassed. → The
   Marz record is on the authorization path: no ambient route to the NIC exists,
   so an effect cannot reach the wire without going through the record.

### 3. Design

**Principals.** The NIC is owned by a user-space **Marz daemon** holding only an
explicit MMIO + DMA grant for the virtio-net device — the same shape as the
existing `virtio-block` daemon. No task, and no agent, ever touches the NIC.

**The egress capability names a destination.** Authority to send is not "network
access"; it is a capability for a specific **destination** (address + port
class). It is derived from an intent (`Ahd`) exactly like every other authority:

```text
derived_destinations = requested_destinations ∩ intent_ceiling
```

so an agent can only reach destinations its intent already allowed, and anything
beyond is dropped and reported.

**Export requires declassification (the DIFC gate).** Before a send, the actor's
secrecy taint must flow to the destination's label:

```text
send permitted  ⟺  taint(actor) ⊆ label(destination)
```

A secret-tainted actor sending to a public destination is **refused** — the
exfiltration case — unless a privileged principal explicitly declassifies. This
is Flume's rule applied at the wire.

**Every send is a ledgered, irreversible effect.** On success Marz appends a Sand
record: actor, intent, derived capability, destination, `reversibility =
irreversible`, so `tbar` attributes it and `sfar-plan` forecasts honestly that it
cannot be undone.

### 4. The precise delta (what is ours)

We claim no novelty on user-space network stacks (seL4/Genode/Fuchsia) or on
DIFC labels and declassification (HiStar/Flume). The recombination is:

> **egress as a per-destination, intent-derived capability whose every use is a
> declassification-checked, irreversibly-classified record on the same effect
> ledger** — on a substrate with no ambient authority to route around it.

| Property | Linux/Win | seL4/Genode/Fuchsia | HiStar/Flume | Marz |
| --- | --- | --- | --- | --- |
| No ambient path to the NIC | ✗ | ✓ | ~ | ✓ |
| Capability names the **destination** | ✗ | ~ | ✗ | ✓ |
| Authority derived from an **intent** | ✗ | ✗ | ✗ | ✓ |
| Flow control on export (declassify) | ✗ | ✗ | ✓ | ✓ |
| Send is a **ledgered effect** | ✗ | ✗ | ✗ | ✓ |
| Classified **irreversible**, rollback refuses | ✗ | ✗ | ✗ | ✓ |
| Attributed to a **mission** | ✗ | ✗ | ✗ | ✓ |

### 5. Why this matters beyond a feature

Until now every external effect in Dezh has been **modeled** (`email.send`,
`prod.deploy`) and the docs say so. Marz makes one real. It also makes the
confidentiality work load-bearing: today an agent is bound to a single Cairn
namespace, so there is no channel to exfiltrate *through*. A network gives it
one — and the DIFC gate is what stands in the way.

### 6. Phases (each CI-green, in the W8 style)

- **M1 — device. DONE.** The `marz` daemon is a separate U-mode ELF holding
  exactly two grants: the **single** virtio-net MMIO page the kernel discovered
  (capability `TASK_DEVICE_VIRTIO_NET` — not the whole window the block grant
  maps) and a DMA window. It never scans for hardware. It negotiates no features,
  arms the transmit queue, builds a real Ethernet + IPv4 + UDP frame and sends
  it. `marz-send` drives it; CI asserts the frame in QEMU's packet capture, so
  the claim is verified **on the wire**, not from a print.
- **M2 — the gate. DONE.** Egress authority names a **destination**, not "the
  network": each destination carries an address and a secrecy label, and the gate
  requires (a) the capability for *that* destination and (b) a flow the
  destination may legally receive (`taint(actor) subset of label(destination)`).
  Revoking one destination leaves the others intact. `marz-demo` proves both on
  the wire, and CI counts frames in the capture: exactly the authorized sends
  appear, and a refused send leaves **nothing** behind. (Deriving the destination
  set from an intent ceiling is the remaining slice.)
- **M3 — the effect. DONE.** Every authorized send is recorded as an
  **irreversible** Sand effect carrying its actor, intent and destination, so
  `tbar` attributes what left the machine and `sfar-plan` forecasts it honestly.
  `sfar-rollback` **refuses** it - the wire cannot be undone and Dezh does not
  pretend otherwise. `marz-effect-demo` shows the whole loop.
- **M4 — the receive path. DONE.** Transmitting proves little on its own: a stack
  that cannot *receive* cannot be checked against reality. The daemon now offers
  the NIC receive buffers, blocks on the device interrupt, and parses what comes
  back: it resolves the destination with **ARP** and completes a real **ICMP echo**
  exchange, matching the reply by id and sequence (`marz-ping <dest>`, reported as
  `NET-RX-OK`). Ingress carries the same authority as egress — a revoked device or
  destination refuses the probe — because reaching the wire is reaching the wire.
- **Verification.** QEMU's packet capture (`-object filter-dump`) lets CI assert
  the permitted frame actually left **and that the refused one did not** — a real
  test, not a printed claim. CI **decodes** the capture as packets rather than
  scanning it for bytes, which matters once the host starts answering: its ICMP
  errors quote our datagram back, and a substring count would score those quotes as
  extra egress. The assertions are now structural — exactly four guest-sourced UDP
  datagrams carry the marker, and the echo request and its reply both appear.

#### Honest non-goals (v0)

No TCP, no DNS, no inbound listening (nothing accepts a connection), no routing,
no DHCP — the address is static. ARP and ICMP echo exist because they are what a
reachability probe needs; ingress is **not** yet a ledgered effect or DIFC-labelled
(a received packet is matched and dropped). No cryptographic transport. This is the
authority + accountability mechanism at the network edge, plus enough of a stack to
prove the edge is real — not a general network stack.

<!-- sources -->
[sel4]: https://sel4.systems/About/seL4-whitepaper.pdf
[flume]: https://pdos.csail.mit.edu/papers/flume-sosp07.pdf
[ambient]: https://en.wikipedia.org/wiki/Ambient_authority

---

## Package signing

<!-- was docs/PACKAGE_SIGNING.md until the 2026-07-23 consolidation -->

This document specifies how Dezh signs and verifies `.dzp` packages. It is
written the way the rest of the project is: we first study, precisely, the
mistakes real package-signing systems have made, then design so we do not repeat
them — and we add the one thing that is only possible on a capability substrate,
which is **signing the *authority* a package requests, not merely its bytes.**

Status: **design + phased implementation.** What is built vs designed is marked
in §7. It follows D015 (no claim beyond what is enforced) and the no-ambient-
authority thesis.

---

### 1. Why the current story is a real gap

Today a `.dzp` package is CRC32-checked and manifest-verified — this catches
*accidental* corruption but not *forgery*: anyone who alters a package can
recompute its CRC. For a system whose thesis is "no authority without explicit
provenance," an unsigned package is a structural contradiction: an app *requests
capabilities* in its manifest, but the request's author is unattributable. This
is the gap `docs/STATUS.md` names, and it is the one we close here.

### 2. Mistakes we studied, and how Dezh avoids each

| Mistake in the wild | Consequence | How Dezh avoids it |
| --- | --- | --- |
| **Sign the artifact bytes, not the metadata** (early apt/yum, npm) | Rollback, freeze, and mix-and-match attacks — the version/dependency/permission metadata is unprotected ([TUF][tuf]). | The signature covers a **canonical serialization of the whole manifest** — name, version, a **monotonic counter**, payload kind, and the **requested capability set** — plus the payload hash. Changing any of them invalidates the signature. |
| **A trusted signer is trusted with *unbounded* authority** (xz / CVE-2024-3094: a maintainer social-engineered for two years, then shipped a backdoor in a validly-built release) ([OpenSSF][xz]) | A signed package still receives full ambient authority; one malicious/compromised signer = total compromise. | **Signing is provenance, not safety.** A signed package still receives *only* the capabilities its manifest requests, those are **bounded by the signer's own capability ceiling** (§4), and every effect it makes is ledgered and reversible (W8). Dezh's defense is layered; the signature is one layer, not the wall. |
| **A single, long-lived, online signing key** (code-signing guidance) ([Keyfactor][kf]) | One key compromise signs everything, forever. | **Role separation:** an *offline root* key authorizes *publisher* keys; each publisher key is scoped (a capability ceiling), rotatable, and independently revocable. |
| **No, or slow, revocation** — a compromised cert keeps being honored ([AppViewX][avx]) | Users keep trusting malicious software after compromise is known. | A signer key is a **trust-store entry**; revoking it is an explicit, **ledgered** effect, and a revoked key's future installs are rejected. This is the same lease/revocation principle Dezh already applies to intents. |
| **Signing is opaque and unauditable** — you cannot tell what was signed, by whom, when (Sigstore's motivation for the Rekor **transparency log**) ([Sigstore][sig]) | No accountability; silent key abuse. | An install **is a Sand effect on the ledger**: `installer → signer identity → package → granted caps`. The provenance graph (`tbar`) answers "who authorized this app's authority." The ledger *is* the transparency log, native to the system. |
| **Roll-your-own crypto** | Subtle, catastrophic bugs. | We use an **audited** Ed25519 implementation ([RustCrypto `ed25519-dalek`]), never a hand-rolled one, isolated in one module shared by the SDK (signing) and the kernel (verification only — deterministic, no RNG in the kernel). |
| **TOCTOU: verify one copy, execute another** | The verified bytes are not the run bytes. | The signature is verified over the **exact staged blob** at install time, and the registry independently re-hashes the blob on every load (existing behavior). |

### 3. What is signed (bind authority, not just bytes)

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

### 4. The novel part: publisher capability attenuation

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

### 5. Trust model and roles

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

### 6. Install becomes a ledgered, attributable effect

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

### 7. Defense in depth — the honest, layered claim

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

### 8. Implementation phases

- **P1 — crypto core. DONE.** Ed25519 verify via the reputable, zero-dependency,
  `no_std` `ed25519-compact` crate, wrapped in `dezh-core::sig`; host-tested;
  builds for both bare-metal targets. `attenuate`/`beyond_ceiling` are the
  publisher-ceiling algebra, proved a subset exhaustively over the 8-bit space.
  *No hand-rolled crypto.*
- **P2 — signed `.dzp`. DONE.** The `DZSP` envelope wraps an unsigned inner
  `.dzp` (so the core format and F3 byte-pinning are untouched); the signed
  message is `inner || "DZSIG1" || counter`; `parse_envelope`/`pack_envelope` with
  a full sign→pack→parse→verify round-trip test.
- **P3 — kernel enforcement. DONE.** A build-time signer (`build.rs`, fixed seed,
  deterministic) embeds a signed demo package + its publisher key; the kernel
  trust store holds root-anchored publisher keys with ceilings + revocation;
  `sig-demo` verifies the signature, requires a trusted non-revoked signer,
  attenuates `granted = requested ∩ ceiling` (the demo's `ipc` is dropped),
  records the install as a ledgered Sand effect, and refuses a tampered package
  and a revoked key. A CI leg asserts all of it.

Each phase is a separate, CI-green commit, in the disciplined style of W8.

**Still open (honest):** a stand-alone developer signing CLI (today only the
build-time signer exists); a *root-signed* trust store loaded from disk with key
rotation (today the store is kernel-embedded); and wiring signature enforcement
into the live `pkg-recv` install path so uploaded packages are verified too
(today `sig-demo` proves the mechanism end to end on an embedded package). These
are additive; the mechanism and the capability-native attenuation are built.

#### Explicit non-goals (honest scope)

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

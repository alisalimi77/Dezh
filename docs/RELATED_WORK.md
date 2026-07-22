# Related Work and Novelty

This document places Dezh in the scientific lineage it draws on, and states —
honestly, mechanism by mechanism — what is **prior art we deliberately reuse**
and what is the **genuinely new recombination** we claim. It follows the
project's D015/D021 rule: we do not claim as novel anything that already has
strong prior art, and we name that prior art precisely.

The short version: **every ingredient in Dezh exists in the literature.** The
contribution is a specific recombination — *intent as the sole, structurally
enforced authority-derivation path* + *an effect ledger that is the
authorization record itself, carrying a reversibility class* + *whole-mission
saga rollback* — made **unbypassable by being built on a kernel with no ambient
authority underneath**, and aimed at a target the OS literature has not yet
claimed: **autonomous AI agents as first-class, effect-accountable principals.**

---

## 1. Capability security (the authority model)

| Work | What it established | What Dezh reuses |
| --- | --- | --- |
| Dennis & Van Horn, *Programming Semantics for Multiprogrammed Computations*, CACM 1966 | The capability: an unforgeable token naming an object + permitted operations. | The core primitive — every authority in Dezh is a capability. |
| Saltzer & Schroeder, *The Protection of Information in Computer Systems*, 1975 | Least privilege, fail-safe defaults, **complete mediation**, economy of mechanism. | The design principles: zero authority by default, mediate every effect. |
| KeyKOS (Hardy, 1985); EROS (Shapiro et al., SOSP 1999); Coyotos | Persistent capability microkernels; orthogonal persistence; confinement. | The microkernel-of-capabilities shape; persistence of authority-bearing state. |
| Miller, Yee & Shapiro, *Capability Myths Demolished*, 2003; Miller, *Robust Composition* (PhD), 2006 | The object-capability (ocap) model; POLA; why capabilities avoid the confused-deputy problem; **attenuation** (you can only delegate what you hold, and may narrow it). | Attenuated delegation over IPC (`granted = requested ∩ sender_caps`). |
| seL4 (Klein et al., SOSP 2009) | The first **formally verified** OS kernel; a capability microkernel with machine-checked proofs. | The proof that a small capability kernel is a sound TCB. Dezh is **not** verified (honest gap); seL4 is the bar. |
| Barrelfish (Baumann et al., SOSP 2009); Genode; Fuchsia/Zircon | Capabilities across cores (multikernel); a capability component framework; object handles as capabilities in a shipping OS. | Evidence the model scales to real system structure. |
| CHERI (Woodruff et al., ISCA 2014; Watson et al., IEEE S&P 2015); Arm Morello | **Hardware-enforced** capabilities at the pointer level. | A future substrate: Dezh enforces at the syscall + paging boundary today; CHERI is the hardware end-state (D017-adjacent). |

**Dezh's delta here is not "capabilities."** It is that authority may only be
*derived through a declared intent*, and the derivation is a **structural subset
operation** (`derived = requested ∩ intent_ceiling`), not a purpose string or a
policy annotation. Attenuation is classic ocap; making the attenuation ceiling a
**first-class "intent" (`Ahd`) that is the only path any authority can enter
through** is the sharpening — see [`SECURITY_MODEL.md`](SECURITY_MODEL.md).

## 2. Information flow, provenance, and audit

| Work | What it established | Relation to Dezh |
| --- | --- | --- |
| Asbestos (SOSP 2005); HiStar (Zeldovich et al., OSDI 2006); Flume (Krohn et al., SOSP 2007) | **Decentralized information-flow control (DIFC)** at the OS level: labels track and constrain how data propagates. | Closest in spirit. Crucially, **HiStar built a new OS** because retrofitting IFC onto a conventional kernel leaks through ambient channels — the same architectural bet Dezh makes for *effect accountability* rather than *information flow*. |
| PASS — Provenance-Aware Storage System (Muniswamy-Reddy et al., USENIX ATC 2006) | OS-level provenance: record where data came from. | Dezh records `actor → intent → effect` provenance too — but **not as a side-log**. In PASS/DIFC the provenance is collected alongside the operation; in Dezh the effect record *is* the authorization-and-persistence record the effect flows through. |
| SELinux / AppArmor; Linux audit + `audit2why` | Mandatory access control + explainable denial ("why was this denied"). | Dezh's `why-denied` names the boundary that produced a refusal — the same reviewer need, but attributed to a capability mechanism rather than a policy rule. |

**Dezh's delta here** is that provenance is **on the authorization path, not
beside it**: because there is no ambient authority under the ledger, an effect
cannot reach a resource without going through the record that authorizes and
logs it. A retrofit provenance/audit layer on an ambient-authority OS can be
bypassed by any path to the resource that skips the logger; Dezh removes those
paths by construction.

## 3. Recovery, transactions, and compensation (honest rollback)

| Work | What it established | Relation to Dezh |
| --- | --- | --- |
| Garcia-Molina & Salem, *Sagas*, SIGMOD 1987; the distributed **saga** pattern | Long-lived transactions that cannot hold locks are undone by running **compensating actions**, not by rolling back a log. | Dezh's `Sfar` rollback is a saga at the OS effect layer: reversible effects are retracted by a ref move, **compensatable** effects are undone by running and *recording* a registered compensating action, and effects with no inverse are **refused with a reason**. |
| Copy-on-write / log-structured stores; NixOS / Nix (Dolstra, PhD 2006) | Versioned, immutable, reproducible state; roll back by moving a pointer, not by mutation; no ambient mutable global state. | Cairn is a commit-log store: rollback moves a ref, history is never erased, state survives reboot. Nix's "no ambient mutable state" is the storage analogue of Dezh's "no ambient authority". |

**Dezh's delta here** is the **reversibility class as a first-class property of
every effect** (`reversible / compensatable / irreversible / unknown`) that
drives an *honest* rollback: the system computes a **forecast** of what a
rollback can and cannot undo *before touching anything*, and it never claims to
undo what it cannot. An effect whose connector does not declare its semantics is
`unknown` and is never optimistically treated as reversible. We are not aware of
an OS-level effect ledger that classifies reversibility and refuses to
over-promise.

## 4. Legacy compatibility and untrusted-code isolation (the real competitor)

| Work | What it is | Relation to Dezh |
| --- | --- | --- |
| Mach/L4 personalities; User-Mode Linux; gVisor | Legacy ABIs served by user-space personality servers / a user-space kernel. | `Pol` runs unmodified static Linux ELFs capability-gated — compatibility as a *security downgrade-free bridge*, not the authority baseline (D014). |
| gVisor, Firecracker (microVMs), WebAssembly/WASI (wasmtime), seccomp-bpf + Landlock, containers | Strong, shipping **confinement** of untrusted code. | **This is Dezh's real point of comparison, not other OSes (D021).** They confine resources well. What they structurally cannot do — because they sit on an ambient-authority host — is attribute *every* effect to its authorizing intent and **reverse a whole agent mission** with no ambient path to route around the ledger. |

## 5. The unclaimed ground: agents as effect-accountable OS principals

Autonomous-agent frameworks (tool-use runtimes, agent orchestrators) enforce
permissions and log actions **in user space, on top of an ambient-authority OS**.
That is exactly the retrofit HiStar showed leaks. The operating-systems
literature has deep results on capabilities, IFC, provenance, and sagas — but
**not** a from-scratch substrate that makes an AI agent a first-class principal
whose every effect is intent-derived, ledgered on the authorization path,
reversibility-classified, and reversible as a mission. That is the ground Dezh
claims (D013, D021), and it is where the recombination above becomes a thesis
rather than a feature.

## 6. Precise novelty claim (what we do and do not claim)

**We claim as new** the *combination*, enforced end-to-end on a no-ambient-
authority kernel:

1. **Intent-as-mechanism:** authority exists only as `derived = requested ∩
   intent_ceiling`, structurally ⊆ a declared intent — the sole derivation path.
2. **Ledger-on-the-authorization-path:** the effect record is the thing the
   effect flows through, not a side-log, carrying `actor → intent → derived cap
   → reversibility class → status`.
3. **Honest, forecastable, saga-style mission rollback** driven by a
   per-effect reversibility class, with compensation and explicit refusal.
4. **Unbypassable because from-scratch:** the above is only sound with no
   ambient authority underneath — the reason the OS form factor exists.
5. **Agent-first:** the target principal is an autonomous agent, an
   OS-level position the literature has not taken.

**We do not claim as new:** capabilities, microkernels, attenuated delegation,
formal-verification potential, DIFC, OS provenance, sagas, multi-ISA IR, or
personality-based compatibility. Each has strong prior art named above, and
Dezh's identity rests on the recombination and its substrate, not on any one of
them (D021).

## 7. Honest scope versus this prior art

- **seL4** is formally verified; Dezh is not (a stated gap — see
  [`THREAT_MODEL.md`](THREAT_MODEL.md)).
- **gVisor/Firecracker/WASI** are mature, portable, and battle-tested at
  confinement; Dezh is a QEMU-only research prototype.
- **CHERI** enforces capabilities in hardware; Dezh enforces at the syscall and
  paging boundary in software.
- **HiStar/Flume** have a worked-out DIFC label calculus; Dezh's authority model
  is today a capability-ceiling algebra, not yet a multi-dimensional,
  formally-specified one (future work — a stated risk).

The point of this document is not to claim Dezh is better than any of these
systems. It is to show we know exactly where Dezh sits, what is borrowed, and
what is genuinely new — which is the minimum a serious operating-systems reader
should demand before taking a new OS seriously.

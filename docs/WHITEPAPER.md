# Dezh: an intent-native, effect-accountable OS substrate

**Whitepaper v1** · research prototype · QEMU-only · see
[`STATUS.md`](STATUS.md) for the honest state and [`THREAT_MODEL.md`](THREAT_MODEL.md)
for what is and is not defended.

## Abstract

Dezh is a from-scratch, capability-secure operating-system substrate built
around one non-negotiable thesis: **no ambient authority.** Every principal —
including an autonomous AI agent — starts with zero access and can act only
through an explicit, unforgeable, attenuable capability for a specific resource
and operation. On top of that base, Dezh adds an **intent-to-effect runtime**:
authority may only be *derived through a declared intent*, every effect is
recorded on the authorization path itself with a **reversibility class**, and a
whole agent **mission** can be forecast, attributed, and rolled back honestly —
retracting what is reversible, compensating what is compensatable, and refusing
with an explanation what is not. The whole chain is enforced by a small kernel,
Sv39 paging, and user-space services, and is exercised end-to-end in CI on a
RISC-V kernel under QEMU (with a second x86_64 kernel proving ISA portability of
the program format).

The contribution is not any single mechanism — capabilities, provenance, and
sagas are all decades old (see [`RELATED_WORK.md`](RELATED_WORK.md)). It is their
**recombination made unbypassable by the absence of ambient authority
underneath**, aimed at a principal the OS literature has not yet claimed: the
autonomous agent whose every effect must be accountable and reversible.

## 1. Problem

General-purpose systems make compatibility and convenience the default authority
model: a process inherits broad filesystem access, environment, file
descriptors, `/proc`, `ptrace`; devices are kernel-resident; service contracts
rest on convention. This ambient authority makes isolation, recovery, and audit
hard — and it is exactly the wrong default when the program you are running is an
**untrusted autonomous agent** that will change repositories, CI, deployments,
and external services on your behalf.

The mainstream response is a user-space sandbox (gVisor, Firecracker, WASI,
seccomp+Landlock). These confine resources well, but they run *on top of* an
ambient-authority host, so an effect log sits *beside* the resource and there is
generally a path to the resource that skips it. You can kill an agent's process;
you cannot cleanly **attribute and reverse the whole set of effects it produced
under one intent.**

## 2. Thesis and system model

Dezh tests the opposite default:

> No operation is authorized unless the caller holds an unforgeable capability
> for that exact operation and target — and that capability could only have been
> **derived from a declared intent** that structurally bounds it.

The model has five layered claims, each enforced by a concrete mechanism:

1. **No ambient authority.** Zero access by default; enforced at the syscall
   boundary *and* by Sv39 paging (a U-mode task faults on ungranted memory/MMIO).
2. **Intent is the only path to authority (`Ahd`).** A capability is derived as
   `derived = requested ∩ intent_ceiling` — a structural subset, not a purpose
   annotation. Anything beyond the intent is dropped and reported; the kernel
   denies the host call if it is attempted anyway.
3. **Every effect is a ledger record (`Sand`).** An effect *is* a Cairn commit,
   enriched to carry `actor → intent → derived cap → reversibility class →
   status → generation`. The record is on the authorization/persistence path,
   not a side-log, so on a no-ambient-authority kernel it cannot be bypassed.
4. **A mission is reversible honestly (`Sfar`).** The effects under one intent
   form a mission. A rollback **forecast** is computed before touching anything;
   the rollback retracts reversible effects by moving a ref, undoes compensatable
   effects by running and *recording* a registered compensating action (a saga
   step), and **refuses** irreversible/unknown effects with an explanation.
   Mission authority spans every namespace the mission touched.
5. **Denial and provenance are explainable (`why-denied`, `Tbar`).** A refusal
   names the boundary that produced it; the `actor → intent → effect` provenance
   graph is queryable and unforgeable, because the intent id and derived
   capability are stamped kernel→daemon on the commit path.

The precise authority rules (derivation, attenuation, the effect-record schema,
and the invariants they must satisfy) are stated in
[`SECURITY_MODEL.md`](SECURITY_MODEL.md).

## 3. Architecture

- **Kernel (`dezh-boot`, RISC-V).** Boots in S-mode via OpenSBI, validates a
  boot contract, installs Sv39 paging and a trap/syscall boundary, attests each
  IPC sender's capabilities, enforces the intent-derivation rule, and preempts
  non-yielding tasks with a timer. Deliberately small — it is the TCB.
- **Drivers and storage out of kernel.** `virtio-block` is a U-mode daemon
  holding only an explicit MMIO page grant + a DMA window + IPC/block authority.
  Clients reach it over typed IPC; there is no hidden kernel block path.
- **Cairn / Sand / Sfar / Tbar** live inside that daemon: an on-disk commit-log
  store whose records double as the effect ledger, with mission rollback and the
  provenance query as read/rewrite operations over the same records.
- **Programs are typed IR (`Dezh-IR`) or capability-gated foreign ELF (`Pol`).**
  The same byte-identical `.dzp` package runs on the RISC-V and x86_64 kernels
  (D016), and an unmodified static Linux/RISC-V ELF runs capability-gated under
  the Linux personality — the same bytes also run on real riscv64 Linux (D014).

## 4. Evaluation

Everything below is asserted by `tools/ci/qemu_smoke.py` and runs on every push;
transcripts live in `docs/`.

- **Containment against an adversary.** `redteam` turns a malicious agent loose
  against five escapes — cross-namespace read, raw MMIO write, capability
  forgery/amplification, out-of-intent action, CPU monopoly — and each is stopped
  at a *named* boundary (storage capability check / hardware paging / kernel
  syscall check / intent-derivation ceiling / preemptive scheduler); the console
  survives every one. Value is only legible with a villain in the room.
- **Honest whole-mission rollback.** `sfar-demo`, `comp-demo`, and
  `sfar-cross-demo` show forecast → retract reversible → compensate compensatable
  → refuse irreversible, across one and multiple namespaces, with the refused
  effect and its provenance surviving a reboot.
- **The flagship.** `overnight` collapses the whole story — an agent loose under
  one intent, a morning of forecast + provenance + honest rollback, and a
  contained escape — into one command (`docs/demo-transcript-overnight.md`).
- **Measurement (D015).** Performance claims are architecture-backed *and*
  measured, never bare superlatives. The one real-silicon, same-CPU figure is
  the capability-check cost (~1 ns) vs the Linux syscall floor (~49 ns);
  everything measured inside the kernel is QEMU-emulated and labelled as such.
  The per-effect ledger overhead is analysed in `dezh-boot/BENCH.md`: the
  enrichment is bytes in a commit sector already being written — **zero extra
  I/O per effect**, a property of the record layout independent of timing.

## 5. Related work and novelty

Full treatment in [`RELATED_WORK.md`](RELATED_WORK.md). In one paragraph: Dezh
reuses capabilities (Dennis & Van Horn; ocap/Miller; KeyKOS/EROS; seL4), the
DIFC/provenance insight that such properties must be built in, not retrofitted
(HiStar; Flume; PASS), and compensation-based recovery (sagas; Nix-style
immutable versioned state). It does **not** claim any of these as its identity
(D021). What it claims as new is the recombination — intent as the sole
structurally-enforced authority path, an effect ledger on the authorization path
carrying a reversibility class, honest forecastable saga rollback of a whole
mission — made unbypassable by a from-scratch no-ambient-authority kernel, and
aimed at autonomous agents as first-class principals.

## 6. Limitations (see `STATUS.md`, `THREAT_MODEL.md`)

QEMU-only; not formally verified (seL4 is the bar); external effects are
*modeled*, not wired to real connectors; ledger integrity trusts the storage
daemon (records are hashed/chained for corruption + rollback, not signed against
a malicious writer); the commit log is a fixed 255 slots with no GC; intents are
runtime sessions with no lease/revocation for long-lived agents yet; no IOMMU; a
small Pol syscall subset; no package signing. Each is named rather than elided.

## 7. Future work

Leases/revocation for long-lived agents (before real networking); a
multi-dimensional, formally-specified intent algebra (operation × resource ×
namespace × time × quota × destination × data-class × delegation-depth) with
property tests that no dimension widens on derivation; real **Gateways**
(git/CI/deploy/HTTP/DB/secrets connectors with enforced effect schemas and
compensation); unifying the host-crate and bare-metal authority implementations
to a single source of truth; and, longer term, hardware-enforced capabilities
(CHERI) and verification of the smallest kernel authority rules.

## 8. Review request

We specifically invite scrutiny of: the intent-derivation rule and its
invariants; whether the effect ledger is genuinely unbypassable given the TCB;
the honesty of the reversibility classification and mission rollback; the
threat-model non-goals; and the novelty claim in §5 against the prior art in
[`RELATED_WORK.md`](RELATED_WORK.md).

# Security

What Dezh defends against, what it does not, and how the enforcement is actually
built. The threat model comes first because every claim below is scoped by it.

---

## Threat model

<!-- was docs/THREAT_MODEL.md until the 2026-07-23 consolidation -->

This document states, honestly and specifically, **what Dezh defends, what it
does not, and what you must trust for those defenses to hold.** It follows the
project's D015 honesty rule: no defense is claimed that is not enforced by a
real mechanism in the tree, and every explicit non-goal is named rather than
elided.

Dezh is a from-scratch, capability-secure OS substrate whose one non-negotiable
thesis is **no ambient authority**: every principal — including an AI agent —
starts with zero access and can only act through an explicit, unforgeable,
attenuable capability for a specific resource and operation. W8 builds on that
to make the *intent → derived authority → effect → provenance → reversibility*
chain the only path to an effect, and to make a whole agent *mission*
attributable and reversible.

Status: **research prototype, QEMU-only.** There is no real-silicon port, no
production boot chain, and no formal verification. Read every claim below in
that light.

---

### 1. Assets

What an attacker wants, and what Dezh is trying to protect:

- **Confidentiality of an app's state** — one app's Cairn namespace must not be
  readable or writable by another principal that was not granted it.
- **Integrity of the effect ledger** — the recorded chain `actor → intent →
  derived cap → effect → reversibility class` must not be forgeable or
  rewritable after the fact by the principal that produced the effect.
- **Containment of an untrusted agent** — an agent must not exceed the authority
  derived from its intent, reach devices it was not granted, read another
  task's memory, or monopolize the CPU.
- **Reversibility guarantees being honest** — a rollback must never *claim* to
  have undone something it cannot undo.

### 2. Principals

- **The operator / console.** The human (or their tooling) driving the machine.
  Trusted to open intents and authorize missions; acts as the mission owner.
- **Installed apps / agents (Dezh-IR or Linux-ELF).** Untrusted. Get exactly the
  capabilities their verified manifest declares, derived down through the intent
  (`Ahd`) they run under — never more.
- **User-space services** (e.g. the `virtio-block` daemon that owns the disk and
  the Cairn/Sand/Sfar/Tbar store). Partially trusted — see the TCB below.

### 3. Trusted Computing Base (TCB)

For the defenses in §4 to hold, you must trust:

1. **The kernel** (`dezh-boot`): the trap/syscall boundary, the Sv39 page tables,
   capability attestation on IPC, the intent-derivation rule (`derived cap ⊆
   Ahd`), and the preemptive scheduler. A bug here can defeat everything.
2. **The boot chain** (OpenSBI / firmware → S-mode entry). Not measured, not
   attested.
3. **The hardware / emulator** (today: QEMU `virt`). Assumed to implement
   privilege levels, paging, and the timer honestly. No defense against a
   malicious or buggy CPU/emulator.
4. **The storage daemon** *for ledger integrity*. The daemon owns the block
   device and is the sole writer of the Cairn/Sand records. It is a user-space
   process with **no ambient authority of its own** (it holds only the device
   MMIO + DMA capabilities it was granted, and it attests every caller's
   capabilities), but a compromised daemon could forge or corrupt ledger
   records. Moving more of its integrity into the kernel/records (e.g. signed or
   chained-hash records) is future work.

Everything outside this list is untrusted, including all installed apps/agents.

### 4. What Dezh defends — and the mechanism that enforces it

Each of these is exercised by the `redteam` console command (an adversary that
*tries* each escape) and asserted in CI. The point of the differentiator is only
legible with a villain in the room.

| Attack | Stopped at (named boundary) | Mechanism |
| --- | --- | --- |
| Read another app's Cairn namespace | storage-service capability check | Kernel attests the sender's caps on every IPC `recv`; the daemon checks the requested namespace's bit and denies with an explanation. |
| Write a device MMIO register directly | hardware memory boundary | Sv39 paging maps MMIO `U=0`; a U-mode store faults, the kernel kills only that task, the console survives. |
| Forge / amplify a capability | kernel syscall capability check | A zero-authority task calling a privileged syscall is denied; `granted = requested & sender_caps` on delegation means you cannot pass authority you do not hold. |
| Act beyond the granted intent | intent-derivation ceiling | `derived cap = requested & Ahd_ceiling`; anything beyond the intent is dropped, and the kernel denies the host call if attempted anyway. |
| Monopolize the CPU | preemptive scheduler | A timer interrupt forces a context switch; a non-yielding task cannot starve others. |

Beyond containment, W8 defends **honest reversibility**:

- **Mission authority spans every namespace a mission touched.** A whole-mission
  rollback (`sfar-rollback`) or provenance query (`tbar`) is refused unless the
  caller holds the capability for *every* namespace the mission wrote to — a
  partial rollback would be dishonest, so it is refused all-or-nothing with the
  missing namespace named.
- **Rollback never over-promises.** Reversible effects are retracted by moving a
  ref; compensatable effects are undone by *running and recording* a registered
  compensating action (a saga step, itself an accountable effect on the ledger);
  irreversible/unknown effects are **refused with an explanation**, never
  silently "undone". A connector that does not declare its semantics is
  classified `unknown` and is never optimistically treated as reversible.
- **Effects are attributable.** The intent id and derived cap are stamped
  kernel → daemon on the commit path, so the `actor → intent → effect`
  provenance (`tbar`) is not something the actor asserts about itself.

### 5. What Dezh does **not** defend (explicit non-goals)

Naming these is part of the honesty rule.

- **Confidentiality beyond read-access control — the exfiltration gap.** This is
  the most important one for the agent-containment thesis, so it leads. Dezh
  confines *read access* by capability: an agent cannot read a Cairn namespace it
  was not granted (the `redteam` cross-namespace read is denied). The W8 effect ledger and mission rollback are **integrity** mechanisms
  — they attribute and *undo* what an agent *did*; they cannot un-leak what it
  *read and sent*. A commit log does not help against exfiltration. **Information-flow control (DIFC) now exists and is enforced on the storage
  path.** `dezh_core::difc` provides the primitive (a secrecy label per object, a
  taint per actor, `taint ⊆ sink` for a write — no write-down, HiStar/Flume,
  [RELATED_WORK.md](RELATED_WORK.md) §2), and it is *enforced on the live Cairn
  console path* (`taintflow-demo`): reading `ns=vault` (labelled secret) taints
  the operator, after which a commit to a lower-secrecy namespace is refused
  until an explicit, privileged `declassify`. It is enforced at the **network
  edge** too: a secret-tainted operator cannot export to a destination not cleared
  for that secret (`exfil-demo`, [Marz](SUBSYSTEMS.md#marz-guarded-egress)).

  The **integrity** axis — the dual, and the one *ingress* needs — is enforced as
  well (`ingress-demo`). Secrecy asks "may this leave?"; it says nothing about
  bytes arriving from outside, which are attacker-chosen and must not silently
  become trusted state (Biba; the endorsement half of HiStar/Flume). A namespace
  can *require* an endorsement; consuming network input lowers the operator's
  integrity, so a write into such a namespace is refused until a privileged
  `endorse`. The two escapes are deliberately separate: `declassify` does not hand
  back integrity, and `endorse` does not clear secrecy, so one privileged act never
  grants two.

  What is **not** yet enforced: either taint across the U-mode client→daemon hop
  or IPC generally, and the ingress taint is at *operator* granularity — consuming
  any network reply lowers integrity wholesale rather than tracking the individual
  bytes. So information-flow control is real on the storage path and at the network
  edge in both directions, but not yet pervasive per-value.
- **Side channels and covert channels.** No defense against timing, cache,
  Spectre/Meltdown-class, or power side channels; no mitigation of covert
  channels between principals.
- **A malicious or buggy kernel.** The kernel is fully trusted (§3). There is no
  formal verification (unlike seL4) and no runtime self-protection against a
  kernel-level bug.
- **Hardware and firmware faults.** Rowhammer, malicious DMA from a device Dezh
  did not sandbox, firmware implants, a lying emulator — all out of scope.
- **DMA-capable devices without an IOMMU.** A device with a DMA capability can,
  absent an IOMMU, reach memory outside its grant. Dezh has the *device-as-
  process + device capability* model but **no IOMMU** yet (D017 is a hypothesis).
  A driver process is trusted with the memory its DMA can reach.
- **Denial of service beyond CPU monopoly.** CPU starvation is handled by
  preemption. Storage exhaustion (the 255-slot commit log filling; GC is future
  work), memory exhaustion, and IPC flooding are **not** bounded yet.
- **Ledger integrity against a compromised storage daemon** (§3). Records are
  parent-linked and hashed for *corruption detection and rollback*, not signed
  against a malicious writer.
- **External / irreversible effects in the real world.** Dezh models external
  effects (e.g. `email.send`) and is honest that they cannot be un-happened. It
  does not (yet) integrate real network/DB/secret connectors with enforced
  effect schemas — that is the Gateways line of future work.
- **Supply-chain integrity of packages.** `.dzp` packages are CRC-checked and
  manifest-verified, **not** cryptographically signed. Signed manifests are
  future work.
- **Real hardware.** QEMU-only today. VMware/VirtualBox is proven for the x86
  port's boot path only.
- **Multi-agent sub-delegation, leases/revocation for long-lived agents, and a
  formal authority algebra** are designed-for but not yet built; treat
  long-lived-agent authority as coarse today.

### 6. Why not just a user-space sandbox? (head-to-head)

The real competitor is not another OS; it is user-space agent isolation —
gVisor, Firecracker, `wasmtime`/WASI, `seccomp`+`landlock`. Those are strong at
*confinement*. Dezh's claim is narrower and different: **it makes the effect
ledger unbypassable and a whole mission attributable and reversible**, which is
structurally hard for a sandbox layered over an ambient-authority host.

- **Unbypassable ledger.** On a host with ambient authority (inherited fds,
  `/proc`, `ptrace`, environment, shared mounts), any effect log sits *beside*
  the resource, and there is generally a path to the resource that skips the
  log. On Dezh there is no ambient authority under the ledger: the effect path
  goes *through* the record that authorizes it. This is the reason the
  from-scratch kernel exists — it is the only substrate where the ledger cannot
  be gone around.
- **Whole-mission accountability and rollback.** A sandbox can kill a process;
  it cannot cleanly *attribute and reverse the set of effects an agent produced
  across resources under one intent*. Dezh can: `sfar-plan` forecasts what a
  rollback can and cannot undo *before* touching anything, `sfar-rollback`
  retracts the reversible effects, runs registered compensations, and refuses
  the irreversible with an explanation, and `tbar` renders the provenance graph.
  The Dezh side of this comparison is reproducible in CI (`sfar-demo`,
  `comp-demo`, `sfar-cross-demo`, `tbar`, `redteam`).

The honest scope: a sandbox is more mature, portable, and battle-tested at raw
confinement today. Dezh trades that maturity for a property they cannot easily
offer — an effect ledger that cannot be bypassed and a mission that can be
accounted for and undone.

### 7. Reproduce the defended cases

Boot the RISC-V kernel (see `docs/GETTING_STARTED.md#build-and-run`) and run:

```
redteam          # five escapes, five named boundaries, system survives
why-denied       # explains the most recent denial and names its boundary
sfar-demo        # a mission with mixed effect classes: forecast, then honest rollback
comp-demo        # a compensatable effect undone by a recorded compensating action
sfar-cross-demo  # a mission across two namespaces; rollback needs authority over both
tbar <ahd>       # the actor -> intent -> effect provenance graph for an intent
```

All of the above are also asserted by `tools/ci/qemu_smoke.py`.

---

## Enforcement model

<!-- was docs/SECURITY_MODEL.md until the 2026-07-23 consolidation -->

### Core Rule

No task receives authority by default. A task can only perform an effect if the
kernel, boot plan, service registry, or caller has explicitly granted the
required authority.

### Prototype scope

The authoritative threat model is [above](#threat-model); this is the narrower
list of cases the prototype's enforcement was built and tested against:

- untrusted U-mode tasks
- apps with limited declared capabilities
- service clients that should not touch devices directly
- faulty or stopped services
- malformed IPC requests
- no-grant MMIO access attempts

### Enforced Today

- Syscalls are gated by task capabilities.
- U-mode page tables deny access outside the task grant.
- MMIO is mapped only for tasks with explicit device grants.
- IPC send requires IPC capability.
- Transferred capabilities are attenuated to the sender's own authority.
- Foreground task faults kill only the faulting task.
- User-space block driver failure does not kill the console.
- Stopped or faulted block service causes clean command failure.

### Not Enforced Yet

- Real IOMMU-backed DMA isolation.
- Production package signatures.
- Multi-client block queues with per-client data windows.
- Full revocation model for long-lived delegated capabilities.
- Production installer and bootloader flow.
- Side-channel resistance.
- Formal verification.

### Revocation (honest answer)

Reviewers ask this first, so here is the current stance plainly.

**What exists today.** Authority is *attenuable* and its *effects are
reversible*, which covers the common cases without a general revocation
mechanism:

- A delegated capability can never exceed the sender's own (`granted =
  requested & sender_caps`), so authority only ever narrows as it spreads.
- A capability is bound to a task; when the task exits or is killed on a fault,
  its authority is gone with it.
- Damage done through a granted capability is undone structurally: Cairn's
  commit log lets an operator roll a namespace back to a prior state (the F1/F2
  demos show exactly this — an agent's bad write is reverted after the fact).

**What now exists (intent level).** An intent (`Ahd`) can be opened with a
**lease** (a bounded run count that auto-revokes on exhaustion) or **revoked**
explicitly; a revoked or exhausted intent authorizes nothing further, while the
effects it already produced keep their provenance (`tbar`/`sfar` still resolve).
This is the first realization of the generation/lease scheme, at the intent
layer — `lease-demo` proves it.

**What still does not exist (capability level).** There is no runtime
lease/revoke for a single, long-lived **task capability bit** already delegated
to a still-running task — you cannot reach into a live task and rescind one bit
mid-execution. The honest reason is the point below: task capabilities are
bitmask bits, not per-object revocable references.

### What kind of capability is this? (bitmask vs object-capability)

Being precise, because it was the most important honest caveat. Dezh **started**
with authority as a bit in a per-task bitmask (print, IPC, a Cairn namespace,
device, block) rather than an unforgeable reference to one object as in seL4 or
CHERI. That is no longer the whole story: the authorities that name real objects
— **namespaces, devices, egress destinations** — are now generation-stamped
handles with per-object revocation and attenuated delegation (see the migration
below). What remains a plain bit is the process-level authority that names no
object (`print`, `time`, `ipc`), and the per-message attestation the storage
daemon uses.

Two things keep this from being "just Linux capabilities," though:

- **Not ambient, not inherited.** Linux capabilities are ambient process
  privileges that a child inherits by default. A Dezh task starts with **zero**
  authority; it holds only bits explicitly granted, and a spawned process
  inherits none.
- **Kernel-attested and attenuable per message.** The kernel stamps the sender's
  capabilities on **every** IPC message, and delegation is
  `granted = requested ∩ sender_caps` — you can pass a *narrower* subset of what
  you hold, checked by the kernel, and never more. Linux capabilities are not
  attenuable this way.

So Dezh sits **between** Linux capabilities and seL4/CHERI object-capabilities:
far stronger than the former (no ambient authority, attenuable, kernel-attested,
and now per-object revocable for every authority that names an object), and still
short of the latter, whose object references are the *only* form authority takes
and are enforced by the kernel (or hardware) on every use rather than at
kernel-side chokepoints.

**The path (the one big change), now prototyped.** Turn a capability into a
first-class object — a generation-stamped handle to a specific resource — so that
(a) revocation of a single capability falls out (bump the generation; every
outstanding handle is invalidated at next use), and (b) delegation forms a real
provenance graph. This primitive is now **built and proven** in
`dezh_core::ocap` (`Cap` = object + rights + generation; `CapTable` holds the
live generation per object; `derive` attenuates rights along a delegation graph;
`revoke` bumps a generation to invalidate every outstanding handle to *that*
object). It is host-tested exhaustively and driven in the kernel by `cap-demo`:
mint a handle, derive an attenuated child, use both, then revoke the object and
watch the whole delegation subtree go stale at next use while a handle to a
*different* object keeps working — per-object revocation a bitmask cannot
express. A forged handle (guessed generation) is rejected.

Migration has **started on the live plumbing**, not just the primitive. The
Cairn **namespace** capability is now ocap-backed at the kernel chokepoint: the
console holds a generation-stamped handle per namespace, and the ocap gate is
enforced on **both** the operator console path (`cairn-commit`/`-get`/... via
`ns_authority_live`) **and the untrusted agent path** (`KHost::cairn_put`/
`cairn_get`). `ns-revoke` bumps a namespace's generation, and from that point a
commit or an agent's write to that namespace is refused until `ns-grant`
(`nsrevoke-demo`, `agentrevoke-demo`). So runtime revocation of a live namespace
capability is real for every kernel-side path today.

Revocation is now also **enforced by the object owner and survives reboot**: the
storage daemon records a per-namespace revoked flag in the Cairn superblock, so
`ns-revoke` persists on disk and the daemon refuses every operation on a revoked
namespace until `ns-grant` — independent of the in-memory kernel gate. A CI
reboot leg proves it: revoke a namespace, power-cycle, and the daemon still
refuses it from its superblock even though the kernel's in-memory handle is fresh.
So the Cairn namespace capability has full ocap revocation at three layers: the
console gate, the untrusted-agent (`KHost`) gate, and the persisted object-owner
check.

**Breadth: the object-like authorities are now all ocap-backed.** Beyond
namespaces, the two other authorities that name real objects have been migrated:

- **Devices.** Each device is an object with a generation-stamped handle
  (`dev-revoke` / `dev-grant`). Revoking one stops every use of that device
  regardless of finer authority — a kill-switch above the per-destination gate
  (`dev-demo`). The grants themselves are now **per-device**: the kernel finds
  the block device and the NIC and maps only their own pages, so neither daemon
  can reach the other's hardware. (The block grant previously mapped the whole
  virtio-mmio window.)
- **Egress destinations.** Authority names a destination, not "the network", and
  destinations are revoked individually (`marz-revoke <dest>`).

What deliberately stays a simple bit is the process-level authority that does not
name an object: `print`, `time`, `ipc`. These are ambient-style permissions of a
task, not references to a resource, so a generation-stamped handle would add
ceremony without adding a revocable object. If they ever name objects (a specific
console, a specific channel), they should migrate too.

### Reviewer Notes

The current security value is architectural discipline, not production
hardening. The relevant question is whether the authority boundaries are in the
right places and whether the demo proves those boundaries under fault and denial
scenarios.

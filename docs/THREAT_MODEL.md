# Dezh Threat Model

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

## 1. Assets

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

## 2. Principals

- **The operator / console.** The human (or their tooling) driving the machine.
  Trusted to open intents and authorize missions; acts as the mission owner.
- **Installed apps / agents (Dezh-IR or Linux-ELF).** Untrusted. Get exactly the
  capabilities their verified manifest declares, derived down through the intent
  (`Ahd`) they run under — never more.
- **User-space services** (e.g. the `virtio-block` daemon that owns the disk and
  the Cairn/Sand/Sfar/Tbar store). Partially trusted — see the TCB below.

## 3. Trusted Computing Base (TCB)

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

## 4. What Dezh defends — and the mechanism that enforces it

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

## 5. What Dezh does **not** defend (explicit non-goals)

Naming these is part of the honesty rule.

- **Confidentiality beyond read-access control — the exfiltration gap.** This is
  the most important one for the agent-containment thesis, so it leads. Dezh
  confines *read access* by capability: an agent cannot read a Cairn namespace it
  was not granted (the `redteam` cross-namespace read is denied). But Dezh has
  **no information-flow control (DIFC)**: once an agent legitimately holds data,
  nothing stops it from *exfiltrating* that data through a channel it is allowed
  to use. The W8 effect ledger and mission rollback are **integrity** mechanisms
  — they attribute and *undo* what an agent *did*; they cannot un-leak what it
  *read and sent*. A commit log does not help against exfiltration. Closing this
  needs label-propagation / DIFC in the spirit of HiStar/Flume
  ([RELATED_WORK.md](RELATED_WORK.md) §2), and it becomes urgent the moment real
  networking exists (which it does not yet). Treat Dezh today as strong on
  *integrity and attribution*, weak on *confidentiality of already-granted data*.
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

## 6. Why not just a user-space sandbox? (head-to-head)

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

## 7. Reproduce the defended cases

Boot the RISC-V kernel (see `docs/BUILD_AND_RUN.md`) and run:

```
redteam          # five escapes, five named boundaries, system survives
why-denied       # explains the most recent denial and names its boundary
sfar-demo        # a mission with mixed effect classes: forecast, then honest rollback
comp-demo        # a compensatable effect undone by a recorded compensating action
sfar-cross-demo  # a mission across two namespaces; rollback needs authority over both
tbar <ahd>       # the actor -> intent -> effect provenance graph for an intent
```

All of the above are also asserted by `tools/ci/qemu_smoke.py`.

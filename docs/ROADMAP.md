# Roadmap and direction

Where the work is going. The roadmap is the near-term plan; the strategic
direction is the reasoning that produced it and is the slower-moving document.

---

## Roadmap

<!-- was docs/ROADMAP.md until the 2026-07-23 consolidation -->

### MVP — The Reviewable OS (current focus)

One sentence: **install Dezh, write an app for it, hand it an untrusted
program or agent — it can only do what you granted, and its effects are
rollbackable.**

MVP is done when a stranger, with no help from us, can:

1. Boot a downloadable Dezh image in a VM (QEMU one-liner; VirtualBox for x86).
2. Write and install their own app in about 10 minutes using the SDK.
3. Reproduce four flagship demos, one per differentiator (below).

Every claim follows D015: measured, honestly scoped, no bare superlatives.

#### Flagship demos (one per differentiator)

| # | Differentiator | Demo a reviewer runs | Honest claim wording |
| --- | --- | --- | --- |
| F1 | Agent containment (D001/D013) | Install an "agent" app with narrow caps: it works inside its grant, is DENIED by the kernel beyond it, delegates an attenuated cap to a sub-task over IPC, and its damage is undone by rollback. | "Authority is explicit, unforgeable, attenuable — enforced by hardware privilege + paging, not by a sandbox policy file." |
| F2 | Cairn storage (D004/D005) | App state is versioned: write, snapshot, corrupt, roll back → restored across reboot. A second app is DENIED access to the first app's namespace. | "State recovery is structural (versioned objects + refs), not fsck. Per-app namespaces are capability-gated." |
| F3 | Multi-ISA apps (D003/D016) | The same byte-identical `.dzp` package (Dezh-IR payload) installs and runs on the RISC-V kernel and the x86_64 kernel. | "Apps are ISA-portable by construction; proven today on 2 ISAs (RISC-V, x86_64), designed for all." |
| F4 | Pol compatibility (D007/D011/D014) | An unmodified static Linux riscv64 ELF (built on stock Ubuntu) runs under the Linux personality, capability-gated; syscall-translation overhead is measured and published. | "Near-native compute for same-ISA binaries (no emulation); syscall translation overhead measured at N ns vs native Linux on the same substrate. Coverage is a small syscall subset today." |

#### Workstreams

##### W1 — SDK, packages, install flow (foundation; everything rides on it)

- `app.toml` manifest: name, version, entry, payload type (`elf-riscv64` |
  `dezh-ir`), requested capabilities (print, uptime, cairn namespace, ...).
- `.dzp` package format: header + manifest + payload, built by
  `tools/sdk/build-pkg.py` from an out-of-tree app directory.
- App template + "write your first Dezh app in 10 minutes" guide
  (becomes the heart of REVIEWER_GUIDE).
- Package ingestion into a live system: UART upload command
  (`install-pkg`, chunked/base64) first; disk-image staging as fallback.
- Grants happen at install time from the manifest (mobile-permission
  feel, but kernel-enforced and unforgeable); recorded in the app
  registry; visible via `app-permissions`.
- Dogfood: port `calc`, `vault`, `lab` from embedded bundles to `.dzp`.
- Acceptance: an out-of-tree hello app builds on the host, installs into a
  running Dezh, runs; an undeclared cap use is DENIED.

##### W2 — Cairn v1 (differentiator F2)

- On-disk object store with a ref/commit log: rollback N steps, not just
  current/previous sectors; survives reboot.
- Per-app namespaces (`/app/<name>/...`) mediated by the storage service
  over typed IPC; namespace access is a manifest capability.
- `cairn-demo` console flow proving F2 end to end.
- Acceptance: F2 transcript reproducible by the demo runner.

Status (2026-07-04): DONE. Commit-log store on sectors 1600..1855
(superblock + append-only commit records carrying parent ref, FNV-1a object
hash, actor task id, and a reversibility flag — the D020 effect-ledger seed).
Namespace access is enforced by kernel task-capability bits 8..15: the kernel
attests the sender's caps on every IPC recv and the storage daemon checks the
requested namespace's bit, with an explainable denial message. Console:
`cairn-commit/get/log/rollback/verify/status` + `cairn-demo`; rollback moves
the head ref and keeps history. Manifest wiring: a `cairn-read`/`cairn-write`
grant maps to the app's OWN namespace only (matched by app name); IR apps
reach the store through the kernel Host routed over IPC to the user-space
daemon (no kernel block I/O path). Covered by CI smoke (including a
second-boot persistence phase) and the review demo runner.

##### W3 — Agent containment demo (differentiator F1; ties W1+W2 together)

- Agent app (Dezh-IR payload) with a narrow cairn namespace grant.
- Shows: in-grant work, kernel DENIED beyond grant, attenuated delegation
  over IPC (`granted = requested & sender_caps`), rollback of its writes.
- Publish alongside the capability-vs-syscall mediation benchmark.
- Acceptance: F1 transcript reproducible by the demo runner.

Status (2026-07-04): first full pass DONE via `tools/demo/run_agent_demo.py`
(in CI): SDK-built out-of-tree `agent` app uploaded over the UART, installed
with manifest-scoped grants (own namespace only), does durable in-grant
commits then a bad write; the operator undoes the damage with a one-step
rollback (hash-verified, history kept); a no-capability `spy` app is DENIED
by the kernel; attenuated delegation shown over IPC; state re-checked after
reboot. Transcript: `docs/transcripts/agent-f1.md`. Found and fixed a
latent W1 bug on the way: the storage daemon truncated every sector write to
511 bytes, corrupting any package larger than two sectors. Remaining polish:
fold the mediation benchmark numbers into the published F1 material.

##### W4 — Pol: run a real foreign binary (differentiator F4)

- Extend the process ELF loader to load an unmodified static Linux
  riscv64 ELF (musl hello-world class), personality = Linux.
- Syscall subset: write, exit/exit_group, brk, set_tid_address (sane
  stubs); everything else → clean ENOSYS. No threads, no dynamic linking.
- Measure translation overhead vs native Linux on the same substrate;
  publish the number and method (D015).
- Acceptance: a binary compiled on stock Ubuntu runs on Dezh,
  capability-gated (no PRINT cap → denied).

Status (2026-07-06): DONE. `dezh-boot/linux-guest` is a genuine static
riscv64 musl ELF (no Dezh code) issuing the raw Linux syscall ABI via `ecall`;
the console `linux-elf` command loads it under the Linux personality — `write`
serviced by Pol with the PRINT cap, denied `-EACCES` without it, unsupported
`getpid` returns a clean `-ENOSYS`. The very same bytes also run unmodified on
real riscv64 Linux (verified under `qemu-riscv64-static`). Translation overhead
is measured by the `bench-pol` command (native vs Pol path, kernel-timed):
~0–80 ns/call, within noise of the ~780 ns emulated round trip — a fixed,
near-noise dispatch (BENCH.md, F4). Both legs are in CI smoke.

##### W5 — x86_64 to parity for F3 (largest chunk)

- M2: IDT/exceptions + timer on the x86 kernel.
- Package runner on x86: execute the same `.dzp` Dezh-IR payload
  (print/arith hostcalls; cairn on x86 deferred until it has a disk).
- M3: real bootable ISO (Limine) → boots in VirtualBox/VMware, which also
  delivers the "install it like a real OS" feel.
- Acceptance: F3 — byte-identical package runs on both kernels; x86 ISO
  boots in VirtualBox.

Status (2026-07-06): F3 and the bootable ISO are DONE; M2 is partial. The
x86_64 kernel installs and runs a real `.dzp` agent package
(pack → parse → verify → run) — the same architecture-independent format the
SDK builds and the RISC-V kernel installs. The agent bytecode is pinned
byte-identical by dezh-core's `demo_sum_bytes_are_pinned` test (in CI), so both
ISAs provably execute the same bytes. A Multiboot2 header + `tools/x86/build-iso.sh`
(GRUB `grub-mkrescue`) produce a BIOS ISO that boots in QEMU `-cdrom` **and in
VirtualBox** (screenshot: docs/assets/dezh-x86-virtualbox.png); output is
mirrored to the VGA text buffer so it is visible on the VM screen. The QEMU
`-kernel` PVH path still works for CI. **M2 (partial, DONE for exceptions):**
the x86 kernel installs a 32-vector exception IDT and routes every CPU fault to
a handler that reports vector/error/RIP and halts — the boot deliberately raises
a breakpoint to prove faults are caught, not silent triple-faults. Still future
work: a returnable interrupt path (timer / device IRQs).

##### W6 — Independence and release packaging

- Prebuilt release artifacts: `dezh-riscv.img` + one-line QEMU script,
  `dezh-x86.iso` for VirtualBox.
- Install/app state persists across reboot (app registry on disk).
- CI builds the images and runs the full demo transcript from a fresh
  clone.

##### W7 — Presentation hygiene (before any outreach)

- LICENSE (Apache-2.0 proposed).
- Honesty pass over all docs: QEMU-only status, emulated-vs-native
  benchmark caveats, syscall coverage, no IOMMU yet, revocation status.
- Revocation: at minimum a documented honest answer; implement cheap
  lease/revoke if it falls out of the registry work.
- Refresh REVIEWER_GUIDE / DEMO_SCRIPT around the four flagship demos.

Suggested order: W1 → W2 → W3 → W4 → W5 → W6 → W7 (W7 items can land
alongside any workstream; outreach only after all four flagship demos are
green in CI).

##### W8 — Intent + Effect Runtime (the differentiator made visible; D020/D021)

The MVP (W1–W7) proves the *mechanism* — no ambient authority, capability-gated
storage, rollback, multi-ISA, Pol. W8 turns that mechanism into the thing the
project is actually *about*: an unbypassable intent-to-effect ledger, and it is
scoped so the value is legible to a skeptical practitioner audience (not another
happy-path demo). It is the final form of the F1 demo, not a new differentiator.

Real competitor to beat: not another OS, but user-space agent isolation
(gVisor, Firecracker, wasmtime/WASI, seccomp+landlock). W8 must show something
they structurally cannot — attributing and reversing a whole agent mission.

- **Intent as mechanism (Ahd). — DONE (P1).** `intent-open <kind>` mints an
  **Ahd** (a capability ceiling), `intent-run <ahd> <app>` runs an app whose
  derived capability is proven ⊆ the Ahd — the *only* path to authority — and
  `intent-list` enumerates open Ahds. `intent-demo` is the self-contained proof
  (same agent under two Ahds). A request for authority beyond the Ahd is DENIED
  in a CI smoke leg.
- **Effect ledger on Cairn (Sand). — DONE (P2).** Sand is the **same** Cairn v1
  commit log (user-space, never kernel), enriched so every commit *is* an effect
  record: the commit header now carries `intent (Ahd id) → derived cap →
  reversibility class → status → generation` alongside the existing
  `actor → parent → hash`. It is **not** a parallel store. The intent id and
  derived cap are threaded kernel→daemon on the commit IPC (request-id +
  status byte) and recorded by the daemon that owns the disk. Commands
  `sand-log <ns>`, `sand-info <ns>`, and the self-contained `sand-demo` (open a
  writer intent → run the built-in agent under it → read the effect back off the
  ledger). CI proves effects are recorded, carry their intent, and survive a
  reboot with the provenance intact.
- **Mission (Sfar) + whole-mission rollback + honest external effect. — DONE
  (P3, first slice).** A **Sfar** = the effects under one Ahd (found by the
  intent id stamped on each Sand commit). `sfar-plan <ahd>` is the rollback
  **forecast** — it walks the *live* per-namespace chains and reports how many of
  the mission's effects are `reversible` / `compensatable` / `irreversible` /
  `unknown`, with an honest confidence (never "full" if anything cannot be
  undone). `sfar-rollback <ahd>` retracts the contiguous reversible head-run per
  namespace with a single atomic superblock write and **refuses** the rest with
  an explanation. A fourth reversibility class `unknown` exists so a connector
  that does not declare semantics is never optimistically treated as reversible.
  `sfar-demo` is the self-contained proof: a mission with one MODELED
  irreversible external send + two reversible writes → forecast "partial" →
  rollback undoes the two writes and refuses the send ("already happened in the
  outside world"). CI proves the outcome and that the refused effect + its
  provenance survive a reboot. **Slice 2 — DONE.** `comp-demo` proves a
  `compensatable` effect with a *registered compensating action* is undone by
  running and recording that action (`status=compensation` on the ledger) rather
  than refused; `sfar-cross-demo` proves mission authority spans every namespace
  a mission touched (a rollback holding authority over only one of two namespaces
  is refused, naming the missing one).
- **The adversary (`redteam`). — DONE (P4).** A malicious agent *tries to escape*
  five ways — cross-namespace read, raw MMIO write, capability forgery/
  amplification, out-of-intent action, CPU monopoly — each stopped at a named
  boundary (storage capability check / hardware paging / kernel syscall check /
  intent-derivation ceiling / preemptive scheduler); the system survives every
  one. CI asserts all five named boundaries.
- **Explainable denial + provenance. — DONE (P5).** `why-denied` walks the event
  ring and names the boundary that produced the last denial; **Tbar** (`tbar
  <ahd>`) renders the queryable `actor → intent → effect` provenance graph,
  unforgeable because the intent id + derived cap are stamped kernel→daemon.
- **Credibility layer. — DONE (P6).** Per-effect ledger overhead documented in
  BENCH.md (D015: the enrichment is +12 header bytes in the same commit sector,
  zero extra I/O); `docs/SECURITY_MODEL.md#threat-model` states the trusted base, what is
  defended (with the mechanism for each), and the explicit non-goals (side
  channels, malicious kernel, hardware, no-IOMMU DMA), plus the head-to-head
  where a user-space sandbox cannot cleanly undo a whole mission but Dezh can
  (Dezh side reproducible in CI).
- **One flagship narrative. — DONE (P7).** `overnight` collapses P1–P5 into a
  single story — "leave a coding agent loose on your machine overnight" — with a
  captured transcript (`docs/transcripts/overnight.md`) and a CI smoke leg.

**W8 is complete:** every part above is green in `tools/ci/qemu_smoke.py`.

##### W9 — Hardware maturity: interrupts and SMP

The bottleneck that most kept Dezh from reading as a real OS was that it drove no
hardware asynchronously: all device I/O was polled and only one hart ever ran.
Both are now addressed on RISC-V, in order:

- **Interrupt-driven I/O. — DONE.** A PLIC routes virtio device interrupts to the
  boot hart's S-mode context; drivers block on `sys_irq_wait` (a restartable
  blocking syscall) and are woken by the device rather than by spinning; the
  scheduler idles with `wfi` for a device when nothing else is runnable and
  services the PLIC by hand (the hardware clears `sstatus.SIE` on trap entry, so a
  pending interrupt must be taken explicitly). `irq-stat` reports interrupts
  serviced and driver waits woken by hardware; CI asserts both.
- **SMP bring-up + parallel proof. — DONE.** Secondary harts are started through
  the standard **SBI Hart State Management** call, each given its own stack and
  `tp` = hart id; a parallel round has every secondary hammer one shared atomic
  counter and the coherent total proves genuine concurrent execution on shared
  memory (`smp-demo`; asserted at boot under `-smp 4`). The boot hart is read from
  the firmware, not assumed to be hart 0 — which surfaced and fixed a latent PLIC
  bug (interrupts were hardcoded to hart 0's context).
- **Mutual-exclusion lock. — DONE.** Symmetric scheduling needs a run queue shared
  by more than one hart, which is impossible without a lock — and the kernel had
  none (single-hart discipline covered everything until now). A fair **ticket
  spinlock** (`TicketLock`, FIFO order so no hart starves) is now in the kernel and
  proven: all four harts hammer a **non-atomic** counter under it and the total
  lands exactly on `contributors x work` (`smp-demo` reports `MUTEX-OK`; CI asserts
  it at boot and interactively). Atomics alone cannot prove this — the hardware
  serialises them regardless — so the non-atomic counter is the point.
- **Shared run queue. — DONE.** The structural core of a symmetric scheduler: ONE
  queue of work, every hart popping the next item under the lock and running it in
  parallel. 48 jobs are enqueued and drained concurrently by all harts, and the
  correctness property a run queue must have is checked — **every item runs exactly
  once** (none lost to a torn dequeue, none run twice by two harts). `smp-demo`
  reports `QUEUE-OK`; CI asserts it at boot and interactively.
- **Symmetric task scheduling. — NEXT, not started.** The remaining work is to make
  a run-queue job be a real **U-mode task dispatch** rather than a marker: give each
  hart its own trap infrastructure (per-hart `KCTX`, trap stack, `sscratch`) and
  switch address spaces per hart, so a task pulled from the shared queue runs in
  U-mode on a secondary hart while the boot hart keeps serving the console. The
  scheduler's other shared state (task table, IPC mailboxes, frame allocator) then
  moves under the lock too. Until that lands, the secondaries are a proven
  parallel-compute facility with a correct shared run queue, not yet U-mode
  task-scheduling CPUs.

Post-MVP horizon (recorded, deliberately not started in W8): explicit system
generations / time-travel, multi-agent attenuated sub-delegation with
provenance chains, full saga/compensation for external effects, human-approval
gates for sensitive intents, cross-ISA effect-semantics identity, and
non-storage typed effects (network/service/install). See
`docs/ROADMAP.md#strategic-direction`.

### Medium Term (post-MVP)

- Convert more services from embedded demos into separate ELF services.
- Add revocation and lease semantics for long-lived capabilities.
- Build a richer app lifecycle: install, update, rollback, remove, audit.
- ARM bring-up (third ISA) once x86 reaches parity.
- Signed package manifests.
- Per-client block queues and better storage concurrency.
- Reusable typed service interface definitions.

### Long Term

- IOMMU-backed DMA isolation.
- Production boot media and installer flow.
- Capability-aware GUI/compositor boundary.
- Strong package signing and measured boot integration.
- Formal verification of the smallest kernel authority rules.

### Non-Goals For MVP

- Claiming production readiness.
- Replacing an existing general-purpose OS.
- Full POSIX compatibility (small measured subset only).
- Full package ecosystem.
- Real-hardware driver support (VM targets only).
- Production cryptographic supply-chain infrastructure.

---

## Strategic direction

<!-- was docs/STRATEGIC_DIRECTION.md until the 2026-07-23 consolidation -->

### Position

Dezh should not be framed as a cleaner copy of existing operating-system ideas.
The long-term thesis is stronger:

**Dezh is an intent-native, effect-accountable operating-system prototype.**

The goal is not just to combine a microkernel, capability security, user-space
drivers, package rollback, and service supervision. Those are necessary
building blocks, but they are not the differentiator by themselves.

The differentiator should be that Dezh treats **intent** and **effect** as
first-class OS concepts.

### The Ground We Own (D021)

Dezh is **not** trying to be a better microkernel, a cleaner capability system,
or a kernel that compiles to more ISAs. Each of those has strong prior art
(seL4, KeyKOS, EROS, Barrelfish) and none is a defensible identity. Running on
both x86 and RISC-V is a portability property, not the point — **ISA is an
implementation backend, not identity**: the same mission bytes should produce
the same effect semantics on every backend, and if a new ISA appears in ten
years, Dezh's identity does not change.

**The real competitor is not another OS.** For the concrete job "contain an
untrusted agent and let it be productive," the incumbents are user-space
isolation layers: gVisor, Firecracker / microVMs, wasmtime / WASI,
seccomp+landlock, containers. They confine syscalls and resources well and they
ship today. Any honest positioning compares against *them*, not against a
research microkernel.

What none of them do:

- **Tie every effect to the intent that authorized it** as part of the
  execution model (not a bolt-on audit log an app can route around).
- **Reverse a whole agent mission atomically** — undo everything one intent
  caused, in one operation.
- Do both on a substrate where **the ledger cannot be bypassed**. On a
  conventional OS the ledger is a library sitting on top of ambient authority;
  a program can always reach the resource underneath. On Dezh there is no
  authority underneath to reach — the intent-derived path is the only path, so
  the ledger is not optional instrumentation, it is the execution itself.

**One-line differentiator** (the reviewer challenge): *Unlike seL4, Barrelfish,
Fuchsia, or Redox — which make **access** safe — Dezh makes **effect**
accountable: every action an agent takes is bound to its intent, attributable,
and reversible where possible, and because the kernel has no ambient authority
by construction, that ledger cannot be bypassed.*

#### Value Is Only Visible Against An Adversary

A secure system that is never attacked is just an assertion. The proving demo
must carry a **villain**: an agent that actively *tries to escape* its intent —
read another namespace, write raw device MMIO, forge or amplify a capability,
act outside its declared intent, monopolize the CPU — and is stopped at a named
boundary each time, with `why-denied` explaining it. Happy-path demos (an app
acting inside its grant) do not make the value visible; the escape that fails
does.

#### The Mission Is The Reversible Unit

The unit that makes effect-accountability compelling is not a single write but a
**mission**: the set of effects produced under one intent. Whole-mission atomic
rollback ("undo everything this agent's task did") is precisely what the
user-space sandboxes above cannot offer, because they have no structured notion
of which effects belonged to which authorized purpose. The ledger groups
effects by intent so a mission is a first-class, reversible object.

Honesty boundary: a mission may contain an **irreversible external effect** (a
network send, a physical output). Whole-mission rollback undoes the internal and
compensatable effects and **refuses the irreversible ones with an explanation**
— it never pretends an external effect was recalled. A separate
`docs/SECURITY_MODEL.md#threat-model` states what Dezh's trusted base is, what it defends, and
what it explicitly does not defend (side channels, a malicious kernel, hardware,
no-IOMMU DMA).

### Why This Matters

Traditional operating systems usually grant authority around processes, users,
files, devices, paths, package managers, or broad service APIs. That creates
common failure modes:

- Ambient authority that silently spreads through the system.
- Filesystem or registry state that accumulates unclear ownership.
- Package updates that change code, data, and permissions without enough
  reviewability.
- Service failures that turn into hangs, vague errors, or hidden recovery.
- Logs that describe what happened after the fact, but are not part of the OS
  authority model.

Dezh should avoid repeating these patterns.

### Core Thesis

Instead of asking only:

- Which process is running?
- Which file or device can it access?
- Which package is installed?
- Which service is reachable?

Dezh should also ask:

- What is the declared intent?
- Which authority was derived for that specific intent?
- Which namespace or service route was used?
- What effect did the operation create?
- Can the effect be verified, explained, rolled back, or quarantined?

### Competitive Advantages To Build Toward

#### Intent-Scoped Authority

Authority should be issued for a declared purpose, not as a broad ambient grant.

Example:

- Avoid: "this app can write storage."
- Prefer: "this app can commit note update transaction #42 in its own namespace."

**Hard rule: intent is a mechanism, not metadata.** A narrow capability by
itself is not new — capability attenuation is decades old. Intent only becomes
a real OS concept if:

- deriving authority from a declared intent is the **only** way to obtain it,
- the kernel/runtime guarantees the derived capability is **narrower than or
  equal to** the declared intent,
- the intent, the derivation, and the resulting effects are linked in the
  ledger.

If intent is just a purpose string attached to a grant, it degenerates into
permission theater (the failure mode of macOS TCC purpose strings and loosely
checked OAuth scopes). Dezh must not ship that version.

#### Effect Ledger

Important OS effects should be structured records, not loose logs:

- actor/component
- declared intent
- derived capability
- target namespace/service
- status
- **reversibility class**: `reversible` | `compensatable` | `irreversible`
- rollback or compensation handle (when the class allows one)
- generation/checkpoint metadata

Not every effect can be undone (a network send, a physical output). Claiming
universal rollback would violate D015 honesty; instead every ledger entry
declares its class up front, and `effect-rollback` refuses irreversible
entries with an explanation rather than pretending.

This should support commands such as:

- `effect-log`
- `effect-info <id>`
- `effect-rollback <id>`
- `why-denied <last|id>`

**Placement rule:** the ledger and denial-context store are user-space
services backed by Cairn, not kernel code. The kernel only emits minimal
structured events at authority boundaries; anything stateful lives outside it.
This keeps the microkernel minimal (D008) and makes the ledger itself
rollback-aware for free (D004).

#### Reversible OS Boundary

Install, update, storage writes, service lifecycle changes, and namespace
migrations should be transaction-aware and preferably reversible or
compensatable.

Package lifecycle work already moves in this direction:

- transactional install/remove
- journaled recovery
- quarantine
- explicit GC
- update checkpoints
- rollback
- pin/unpin
- cap escalation review

The next step is to extend this model beyond packages into app data,
namespaces, services, and system generations.

#### No Ambient Continuity

State should not silently carry forward forever.

Dezh should make generations explicit:

- boot generation
- service graph generation
- package generation
- namespace generation
- intent/effect generation

Rollback and audit should be generation-aware.

#### Explainable Denial

"Permission denied" is not enough.

Dezh should explain:

- which intent was denied
- which capability was missing or too broad
- which component requested it
- which safer route is available
- whether review, migration, or explicit override is required

#### Agent-Ready Without Blind Trust

Future systems will run more agents and automation.

Dezh should be designed so agents can operate productively without receiving
ambient authority:

- intent-scoped capability grants
- bounded namespaces
- structured effect ledger
- review gates for sensitive changes
- rollback/compensation where possible
- denial explanations that can guide safer retries

### Honest Novelty Accounting (D015)

Serious reviewers (seL4, Genode, CHERI communities) will immediately map each
piece to prior art. Dezh's public claims must do that mapping first:

Existing ideas Dezh builds on (never claim these as new):

- Capability security: KeyKOS, EROS, seL4, Capsicum.
- User-space drivers and minimal kernel: every serious microkernel.
- Generations and transactional packages: NixOS, ostree.
- Snapshot/rollback storage: ZFS, btrfs.
- Denial explanation: SELinux `audit2why` (bolted-on; ours is first-class,
  which is a UX differentiator, not a research one).

What is genuinely new in combination:

1. **Intent as the sole authority-derivation path**, enforced (derived
   capability ⊆ declared intent), not annotated.
2. **An effect ledger that ties each effect to its authority provenance**
   (actor → intent → derived capability → effect → rollback class/handle) as
   part of the OS authority model, not an audit afterthought.
3. **Agent-first framing**: the above two designed so untrusted agents are
   productive without ambient authority (D013).

Public wording pattern: "Dezh combines known building blocks X and Y; what is
new is 1–3 above." Anything stronger must be measured or demonstrated first.

### Architectural Guardrails

These should remain hard rules:

- No intent-as-metadata: authority is only derivable from a declared intent,
  and the derived capability must be provably narrower or equal.
- No ledger or denial-context state inside the kernel; those are user-space
  services on Cairn.
- No hidden kernel block I/O path.
- No global registry as an app-facing configuration dump.
- No Unix-style ambient filesystem authority as the default app model.
- No silent package update.
- No silent permission expansion.
- No automatic physical cleanup without explicit command and audit.
- No recovery path that widens authority.
- No service failure that causes indefinite hangs.
- No device/MMIO/DMA access without explicit grant.

### Relationship To The MVP (D019)

This document is the **narrative** over the already-defined MVP (D019,
`docs/ROADMAP.md` W1–W7), not a parallel roadmap. Rule: any work item here
must map onto an existing workstream or be explicitly marked post-MVP. Two
competing "what's next" documents would be strategic drift.

Mapping:

- Effect ledger → extends **W2** (Cairn v1 commit log is the ledger
  substrate) and the existing package journal.
- `effect-log` / `effect-info` / `effect-rollback` / `why-denied` →
  fold into **F1/W3** (agent containment demo) and **W2**.
- Intent-derivation rule → hardens **W1** (manifest cap grants become
  intent-derived grants).
- Capability attestation (`cap-audit`, `cap-tree`, `component-info`) →
  supports **F1** demo credibility; small enough to ride along W3.
- App storage namespace + migration (`ns-*`) → genuinely new scope;
  explicitly **post-MVP**. Recorded here so it is not lost, deliberately not
  started before the four flagship demos are green.

### Near-Term Milestones

These are now consolidated as roadmap **W8 (Intent + Effect Runtime)**, the one
workstream that turns D020/D021 from prose into a demonstrated differentiator.
W8 is not "add a feature"; it is the feature plus the three things that make its
value legible to a skeptical practitioner audience — an adversary, a
whole-mission rollback with an honest irreversible effect, and an owned cost.

#### 1. Intent as mechanism (Ahd)

- `intent-open <kind>` issues an **Ahd** (an intent token: a ceiling of
  capabilities for a target namespace), `intent-run <ahd> <app>` runs an app
  whose derived capability is proven ⊆ the Ahd, `intent-list` enumerates open
  Ahds.
- Manifest grants (W1) become Ahd-derived; a request for authority beyond the
  Ahd is denied. This rides the existing IPC attenuation and per-task
  capability bits.

#### 2. Effect ledger on Cairn (Sand) — built (W8 P2)

- **Sand is the same Cairn v1 commit log, enriched — not a parallel store.**
  The user-space storage daemon (which alone holds the disk capability) records
  each effect on the very commit that produces it: `actor → intent (Ahd) →
  derived capability → target namespace → status → reversibility class →
  generation`, alongside the pre-existing `parent → hash`. The intent id and
  derived cap are supplied by the kernel on the commit IPC; the daemon only
  records them.
- Commands: `sand-log <ns>`, `sand-info <ns>`, and `sand-demo` (open an intent →
  run an agent under it → read the effect back off the ledger). Provenance
  survives a reboot because it lives on the durable commit.

#### 3. Mission (Sfar) + whole-mission rollback + honest external effect

- A **Sfar** groups the effects under one Ahd; `effect-rollback <sfar>` undoes
  them atomically; `effect-rollback <id>` undoes one.
- At least one `irreversible` external effect (simulated network/print) that
  rollback **refuses with an explanation**, and one `compensatable` effect with
  a registered compensation action.

#### 4. The adversary

- A `redteam` scenario: a malicious agent that attempts cross-namespace reads,
  raw MMIO writes, capability forgery/amplification, out-of-intent actions, and
  CPU monopoly — each stopped at a named boundary (page fault / capability check
  / intent bound / preemption) with `why-denied`.

#### 5. Explainable denial + provenance

- `why-denied <last|id>`, `cap-tree` / `cap-audit` / `component-info`, and
  **Tbar**, a queryable `actor → intent → effect` provenance graph
  ("everything this agent touched and why").

#### 6. Credibility layer

- **Cost:** the per-effect ledger overhead measured and folded into
  `BENCH.md` (D015).
- **Head-to-head:** a documented scenario where gVisor / Firecracker /
  wasmtime cannot cleanly undo a whole mission but Dezh can (Dezh's side
  reproducible in CI even if the competitor is only described).
- **`docs/SECURITY_MODEL.md#threat-model`:** trusted base, what is defended, and what is
  explicitly not defended.

#### 7. One flagship narrative

All of the above collapse into a single story — "leave a coding agent loose on
your machine overnight" — with a transcript and a CI smoke leg. This is the
final form of the F1 (D020) agent-containment demo, not a separate demo.

The first implementation maps a small set of intents onto existing package,
storage, and service operations, with the ledger stored in Cairn.

#### 2. Capability Attestation v1 (rides along W3)

Make authority explainable at runtime.

Candidate commands:

- `cap-audit`
- `cap-tree`
- `why-denied`
- `component-info <id>`

#### 3. App Storage Namespace + Migration v0 (post-MVP)

Package update is now stronger than data lifecycle. The next major gap after
the MVP demos is app data.

Build:

- per-app namespace identity
- namespace metadata
- migration-required flag
- migration transaction
- rollback-aware data contract
- namespace verification

Candidate commands:

- `ns-list`
- `ns-info <app>`
- `ns-migrate <app>`
- `ns-verify <app>`

#### 4. Dezh Tooling MCPs

MCP should be used around Dezh, not inside the OS kernel/runtime.

Highest-value MCP candidates:

1. `dezh-qemu-mcp`
   - boot QEMU
   - send commands
   - preserve disk image across reboots
   - collect transcript
   - assert expected OS behavior

2. `dezh-image-mcp`
   - inspect raw disk image
   - decode install marker
   - decode package registry
   - decode journal
   - show package blobs, quarantine, GC state

3. `dezh-guard-mcp`
   - enforce architecture guardrails
   - detect kernel-side block I/O regressions
   - detect ambient capability paths
   - scan public docs/package for unsafe claims, secrets, local paths, or
     non-public identity markers

4. GitHub MCP
   - CI status
   - PR/release/review package workflow

5. Browser/Playwright MCP
   - docs/review kit/demo rendering checks

### Review Outcome (2026-07-04)

The direction was reviewed critically and accepted with three corrections,
now folded into the text above:

1. **Intent must be a mechanism, not metadata** — otherwise it is renamed
   audit logging. Added as a hard guardrail.
2. **This document binds to the MVP (D019)** instead of forking the roadmap;
   namespace migration is explicitly post-MVP.
3. **Novelty claims follow D015 honesty** — prior art is named; the genuinely
   new parts are the intent-derivation rule, the provenance-linked effect
   ledger, and the agent-first combination.

Answers to the open review questions:

- The strongest single differentiator is the **effect ledger tied to
  authority provenance**, not intent alone.
- The proving demo is **F1 extended**: give an untrusted agent an intent →
  show the derived narrow capability → agent acts → `effect-log` shows the
  record → `effect-rollback` undoes it → agent attempts something outside the
  intent → kernel denial → `why-denied` explains. One demo covers intent,
  ledger, rollback, explainable denial, and agent containment.
- The main drift risks are convenience pressure (granting the shell broad
  capabilities) and letting ledger/denial state creep into the kernel; both
  are now guardrails.

Registered as D020 in `DECISIONS.md`.

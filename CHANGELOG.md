# Changelog

All notable public-review changes are tracked here. Dezh follows milestone
review tags rather than production semantic-version releases at this stage.

## v0.2-review Candidate

All four flagship demos are green in CI and a stranger can boot a release in a
VM. Since v0.1-review:

- **F4 (Pol):** a real, unmodified static Linux/RISC-V ELF (`linux-guest`) runs
  under the capability-gated Linux personality; the same bytes also run on real
  riscv64 Linux. Pol syscall-translation overhead measured (`bench-pol`).
- **F3 (multi-ISA):** the x86_64 kernel installs and runs the byte-identical
  `.dzp` agent package (`dzp::pack`/`parse`); the bytecode is pinned by a
  dezh-core test so both ISAs provably run the same bytes.
- **Bootable x86 ISO (M3):** a GRUB Multiboot2 ISO (`tools/x86/build-iso.sh`)
  boots the x86 kernel in QEMU `-cdrom` and in VirtualBox; output is mirrored to
  the VGA text buffer.
- **x86 exception IDT (M2):** 32-vector exception table; faults are reported and
  halted, not silent triple-faults.
- **Release + docs:** the release ships the bootable ISO and a `RUN.txt`;
  new `GETTING_STARTED.md#running-in-a-vm`, `STATUS.md` (honest limitations), a plain revocation
  answer in `SECURITY_MODEL.md#enforcement-model`, and a `REVIEWER_GUIDE.md` rewritten around the
  four demos.

## v0.3-review Candidate

Two milestones since v0.2-review. W8 made intent and effect first-class, so an
agent's night can be read back and undone honestly. W9 took the same rule down
to the hardware — real device interrupts, several harts, and a network edge —
and showed the no-ambient-authority thesis survives all three.

Three limitations named in the v0.2 notes are now closed: runtime revocation,
package signing, and SMP. The IOMMU is still open, and still the honest gap in
the driver story.

### W9 — Hardware And Information Flow

- **Device interrupts:** the kernel is interrupt-driven rather than polled. A
  PLIC routes virtio IRQs into the boot hart's S-mode context, drivers **sleep**
  on `sys_irq_wait` until the device wakes them, and the scheduler idles on
  `wfi` for a device when nothing is runnable (`irq-stat`).
- **SMP, up to symmetric scheduling:** secondary harts come up over the real SBI
  HSM protocol (`smp-demo`); a fair **ticket spinlock** makes a non-atomic
  counter come out exact under every hart at once (`MUTEX-OK`) — something
  atomics cannot demonstrate; 48 jobs drain from one shared queue, each running
  exactly once (`QUEUE-OK`); and U-mode tasks are dispatched across harts and
  run **at the same instant** (`smp-sched`, `SCHED-OK`). The boot hart is taken
  from firmware and never assumed to be hart 0.
- **Isolation survives parallelism:** each task carries its own address space, so
  concurrent tasks on different harts cannot reach each other. An intruder
  page-faults and dies on its own hart while its neighbour keeps running
  (`smp-isolate`, `ISOLATION-OK`).
- **Marz, a bidirectional network edge:** egress names a *destination* rather
  than "the network" and is checked before a packet exists. The daemon now also
  **receives** — it offers the NIC receive buffers, blocks on the interrupt,
  resolves by **ARP**, and completes a real **ICMP echo**, matched on id and
  sequence (`marz-ping`, `NET-RX-OK`). CI decodes the capture structurally
  instead of grepping console text.
- **Information flow on both axes:** secrecy stops a labelled value being written
  down or exported (`taintflow-demo`); integrity stops unvalidated **network
  input** becoming trusted state (`ingress-demo`, `INGRESS-OK`). Each axis has
  exactly one explicit, privileged, recorded escape — `declassify` and
  `endorse` — and neither grants the other.
- **Object capabilities:** the Cairn namespace capability is an ocap handle with
  generation-stamped, per-object revocation that survives reboot, and device
  authority is a revocable handle rather than an ambient grant.
- **Package signing:** a `.dzp` can be wrapped in a signed `DZSP` envelope whose
  Ed25519 signature binds the *authority the package asks for*, verified in the
  kernel against an audited crate (`sig-demo`). The distribution layer — a
  signing CLI and an on-disk trust store — is deliberately still open; see
  [STATUS](docs/STATUS.md).
- **Docs:** 31 files consolidated into 15, held there by a CI check.

### W8 — Intent + Effect Runtime (complete)

The intent-to-effect runtime that makes the differentiator legible — an
unbypassable effect ledger, whole-mission accountability, and honest rollback.
All parts are green in `tools/ci/qemu_smoke.py`.

- **Intent as mechanism (`Ahd`):** intent is the only path to authority; a
  derived capability is provably ⊆ its intent ceiling.
- **Effect ledger (`Sand`):** every Cairn commit is enriched into an effect
  record — `actor → intent → derived cap → reversibility class → status` — with
  no second write and no bypass.
- **Mission rollback (`Sfar`):** `sfar-plan` forecasts what a rollback can and
  cannot undo before touching anything; `sfar-rollback` retracts reversible
  effects, **runs and records** the registered compensating action for
  compensatable effects, and **refuses irreversible effects with a reason**.
  Mission authority spans every namespace the mission touched (a partial-
  authority rollback is refused, naming the missing namespace).
- **Adversary (`redteam`):** a malicious agent attempts five escapes
  (cross-namespace read, raw MMIO write, capability forgery, out-of-intent
  action, CPU monopoly); each is stopped at a named boundary and the system
  survives.
- **Explainable denial + provenance:** `why-denied` names the boundary that
  produced the last denial; `tbar <ahd>` renders the `actor → intent → effect`
  provenance graph.
- **Flagship narrative:** `overnight` — "leave a coding agent loose overnight" —
  collapses the above into one story (`docs/transcripts/overnight.md`).
- **Credibility:** `docs/SECURITY_MODEL.md#threat-model` (trusted base, defenses + mechanisms,
  explicit non-goals, head-to-head vs user-space sandboxes) and a per-effect
  ledger-overhead analysis in `dezh-boot/BENCH.md`.

- Added public repository governance files:
  - `LICENSE`
  - `NOTICE`
  - `SECURITY.md`
  - `CONTRIBUTING.md`
  - `CODE_OF_CONDUCT.md`
- Added GitHub issue and pull request templates.
- Added documentation index, getting-started guide, build/run guide, FAQ, and
  release notes.
- Added a consolidated review validation runner.
- Expanded public architecture and repository-structure documentation.

## v0.1-review Candidate

This review candidate presents Dezh as a capability-secure research OS
prototype with:

- RISC-V QEMU bare-metal boot
- x86_64 smoke target
- U-mode process isolation
- capability-gated syscalls
- user-space `virtio-block` daemon
- typed IPC and timeout-aware service paths
- supervised services with stop, restart, and fault demos
- transactional package lifecycle with journal recovery
- reboot-safe SDK `.dzp` package acceptance
- embedded review apps and denial proofs

Known limitations:

- QEMU-first prototype
- no production boot media installer
- deterministic v0 package checksums, not production signing
- modeled DMA isolation without real IOMMU integration
- small fixed package-store limits for reviewability

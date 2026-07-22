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
  new `QUICKSTART_VM.md`, `STATUS.md` (honest limitations), a plain revocation
  answer in `SECURITY_MODEL.md`, and a `REVIEWER_GUIDE.md` rewritten around the
  four demos.

## Unreleased

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
  collapses the above into one story (`docs/demo-transcript-overnight.md`).
- **Credibility:** `docs/THREAT_MODEL.md` (trusted base, defenses + mechanisms,
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

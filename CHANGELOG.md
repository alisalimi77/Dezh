# Changelog

All notable public-review changes are tracked here. Dezh follows milestone
review tags rather than production semantic-version releases at this stage.

## Unreleased

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

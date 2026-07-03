# Dezh OS Roadmap

## Near Term

- Keep the external review kit reproducible and clean.
- Add a real installer image builder for the v0 disk layout.
- Move app bundles toward signed package manifests.
- Expand typed IPC into reusable service interface definitions.
- Add per-client block queues and better storage concurrency.
- Improve benchmark coverage against same-substrate baselines.

## Medium Term

- Convert more services from embedded demos into separate ELF services.
- Add revocation and lease semantics for long-lived capabilities.
- Build a richer app lifecycle: install, update, rollback, remove, audit.
- Add a minimal filesystem or object namespace over the Cairn-style root path.
- Expand x86_64 and ARM bring-up beyond smoke coverage.

## Long Term

- IOMMU-backed DMA isolation.
- Production boot media and installer flow.
- Capability-aware GUI/compositor boundary.
- Strong package signing and measured boot integration.
- Formal verification of the smallest kernel authority rules.

## Non-Goals For v0

- Claiming production readiness.
- Replacing an existing general-purpose OS.
- Full POSIX compatibility.
- Full package ecosystem.
- Production cryptographic supply-chain infrastructure.

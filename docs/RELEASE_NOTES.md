# Release Notes

## v0.1-review Candidate

`v0.1-review` is the first public review candidate for Dezh OS.

It is intended for architecture, security-model, package-lifecycle, and
prototype-execution review. It is not a production release.

## Highlights

- Bare-metal RISC-V QEMU boot through OpenSBI.
- x86_64 smoke target for the shared runtime path.
- U-mode task isolation with contained page faults.
- Explicit capability gates for syscall effects.
- Long-lived user-space `virtio-block` daemon.
- Typed IPC with status-aware replies and timeout accounting.
- Service registry with stop, restart, and controlled fault demos.
- Reboot-safe package store for SDK-built `.dzp` packages.
- Transactional install/remove/update/rollback path.
- Journal recovery, quarantine, pin/unpin, explicit GC, and capability
  escalation review.
- Embedded apps for note, lab, calculator, and vault workflows.
- Public demo transcripts and review tooling.

## Validation

Recommended validation:

```sh
python tools/review/run_full_review.py --quick
```

Full validation:

```sh
python tools/review/run_full_review.py --full
```

Expected release artifacts are described in
[Release Process](RELEASE_PROCESS.md). GitHub Packages usage is described in
[Packages And Releases](PACKAGES_AND_RELEASES.md).

## Known Limitations

- QEMU is the primary validation environment.
- The installer initializes a prototype disk layout, not production boot media.
- Package checksums are deterministic v0 checks, not production cryptographic
  signatures.
- DMA isolation is modeled through page-table discipline and grants; real IOMMU
  work is future scope.
- Store sizes and package limits are intentionally small for reviewability.
- Networking, graphics, formal verification, and real hardware bring-up are
  future work.

## Review Questions

- Is the no-ambient-authority model visible in the code and tests?
- Is the user-space block driver boundary placed correctly?
- Are package lifecycle states and recovery rules sufficiently explicit?
- Are service failure modes clean enough for long-running operation?
- Which parts should be reduced, split, or formalized before the next review
  candidate?

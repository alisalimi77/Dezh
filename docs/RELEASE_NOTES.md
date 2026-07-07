# Release Notes

## v0.2-review Candidate

The milestone where all four flagship demos are green in CI and a reviewer can
boot Dezh in a VM with no source tree.

### What a reviewer can now do

- Boot the x86_64 kernel from a real bootable ISO in **VirtualBox / VMware** (or
  QEMU `-cdrom`); it reaches 64-bit long mode, installs and runs a `.dzp` agent
  package, enforces the print capability, and catches a deliberately-raised CPU
  exception instead of triple-faulting. See `QUICKSTART_VM.md`.
- Run the RISC-V capability console — agent containment (F1), Cairn versioned
  storage with rollback across reboot (F2), the same byte-identical Dezh-IR app
  on both ISAs (F3), and a real unmodified Linux ELF under Pol (F4).

### Flagship demos

- **F1 agent containment** — narrow caps, kernel-DENIED beyond grant, attenuated
  IPC delegation, rollback (`tools/demo/run_agent_demo.py`).
- **F2 Cairn v1** — commit log, rollback, reboot-persistent, capability-gated
  namespaces (`cairn-demo`).
- **F3 multi-ISA** — byte-identical `.dzp` runs on RISC-V and x86_64; bytes
  pinned by a test.
- **F4 Pol** — a stock static Linux/RISC-V ELF runs capability-gated; the same
  bytes run on real Linux; translation overhead measured (`bench-pol`).

### Honest scope

QEMU/VirtualBox targets only; benchmarks that are emulated are labelled as such;
Pol is a small syscall subset; no runtime revocation, IOMMU, package signing, or
SMP yet. Full detail in `docs/STATUS.md`.

### Artifacts

RISC-V and x86_64 kernels, the bootable `dezh-<tag>-x86_64.iso`, a `.dzp` sample
package, a `RUN.txt`, the docs bundle, a manifest, and `SHA256SUMS`.

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

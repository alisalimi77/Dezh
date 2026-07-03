# Dezh OS Roadmap

## MVP — The Reviewable OS (current focus)

One sentence: **install Dezh, write an app for it, hand it an untrusted
program or agent — it can only do what you granted, and its effects are
rollbackable.**

MVP is done when a stranger, with no help from us, can:

1. Boot a downloadable Dezh image in a VM (QEMU one-liner; VirtualBox for x86).
2. Write and install their own app in about 10 minutes using the SDK.
3. Reproduce four flagship demos, one per differentiator (below).

Every claim follows D015: measured, honestly scoped, no bare superlatives.

### Flagship demos (one per differentiator)

| # | Differentiator | Demo a reviewer runs | Honest claim wording |
| --- | --- | --- | --- |
| F1 | Agent containment (D001/D013) | Install an "agent" app with narrow caps: it works inside its grant, is DENIED by the kernel beyond it, delegates an attenuated cap to a sub-task over IPC, and its damage is undone by rollback. | "Authority is explicit, unforgeable, attenuable — enforced by hardware privilege + paging, not by a sandbox policy file." |
| F2 | Cairn storage (D004/D005) | App state is versioned: write, snapshot, corrupt, roll back → restored across reboot. A second app is DENIED access to the first app's namespace. | "State recovery is structural (versioned objects + refs), not fsck. Per-app namespaces are capability-gated." |
| F3 | Multi-ISA apps (D003/D016) | The same byte-identical `.dzp` package (Dezh-IR payload) installs and runs on the RISC-V kernel and the x86_64 kernel. | "Apps are ISA-portable by construction; proven today on 2 ISAs (RISC-V, x86_64), designed for all." |
| F4 | Pol compatibility (D007/D011/D014) | An unmodified static Linux riscv64 ELF (built on stock Ubuntu) runs under the Linux personality, capability-gated; syscall-translation overhead is measured and published. | "Near-native compute for same-ISA binaries (no emulation); syscall translation overhead measured at N ns vs native Linux on the same substrate. Coverage is a small syscall subset today." |

### Workstreams

#### W1 — SDK, packages, install flow (foundation; everything rides on it)

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

#### W2 — Cairn v1 (differentiator F2)

- On-disk object store with a ref/commit log: rollback N steps, not just
  current/previous sectors; survives reboot.
- Per-app namespaces (`/app/<name>/...`) mediated by the storage service
  over typed IPC; namespace access is a manifest capability.
- `cairn-demo` console flow proving F2 end to end.
- Acceptance: F2 transcript reproducible by the demo runner.

#### W3 — Agent containment demo (differentiator F1; ties W1+W2 together)

- Agent app (Dezh-IR payload) with a narrow cairn namespace grant.
- Shows: in-grant work, kernel DENIED beyond grant, attenuated delegation
  over IPC (`granted = requested & sender_caps`), rollback of its writes.
- Publish alongside the capability-vs-syscall mediation benchmark.
- Acceptance: F1 transcript reproducible by the demo runner.

#### W4 — Pol: run a real foreign binary (differentiator F4)

- Extend the process ELF loader to load an unmodified static Linux
  riscv64 ELF (musl hello-world class), personality = Linux.
- Syscall subset: write, exit/exit_group, brk, set_tid_address (sane
  stubs); everything else → clean ENOSYS. No threads, no dynamic linking.
- Measure translation overhead vs native Linux on the same substrate;
  publish the number and method (D015).
- Acceptance: a binary compiled on stock Ubuntu runs on Dezh,
  capability-gated (no PRINT cap → denied).

#### W5 — x86_64 to parity for F3 (largest chunk)

- M2: IDT/exceptions + timer on the x86 kernel.
- Package runner on x86: execute the same `.dzp` Dezh-IR payload
  (print/arith hostcalls; cairn on x86 deferred until it has a disk).
- M3: real bootable ISO (Limine) → boots in VirtualBox/VMware, which also
  delivers the "install it like a real OS" feel.
- Acceptance: F3 — byte-identical package runs on both kernels; x86 ISO
  boots in VirtualBox.

#### W6 — Independence and release packaging

- Prebuilt release artifacts: `dezh-riscv.img` + one-line QEMU script,
  `dezh-x86.iso` for VirtualBox.
- Install/app state persists across reboot (app registry on disk).
- CI builds the images and runs the full demo transcript from a fresh
  clone.

#### W7 — Presentation hygiene (before any outreach)

- LICENSE (Apache-2.0 proposed).
- Honesty pass over all docs: QEMU-only status, emulated-vs-native
  benchmark caveats, syscall coverage, no IOMMU yet, revocation status.
- Revocation: at minimum a documented honest answer; implement cheap
  lease/revoke if it falls out of the registry work.
- Refresh REVIEWER_GUIDE / DEMO_SCRIPT around the four flagship demos.

Suggested order: W1 → W2 → W3 → W4 → W5 → W6 → W7 (W7 items can land
alongside any workstream; outreach only after all four flagship demos are
green in CI).

## Medium Term (post-MVP)

- Convert more services from embedded demos into separate ELF services.
- Add revocation and lease semantics for long-lived capabilities.
- Build a richer app lifecycle: install, update, rollback, remove, audit.
- ARM bring-up (third ISA) once x86 reaches parity.
- Signed package manifests.
- Per-client block queues and better storage concurrency.
- Reusable typed service interface definitions.

## Long Term

- IOMMU-backed DMA isolation.
- Production boot media and installer flow.
- Capability-aware GUI/compositor boundary.
- Strong package signing and measured boot integration.
- Formal verification of the smallest kernel authority rules.

## Non-Goals For MVP

- Claiming production readiness.
- Replacing an existing general-purpose OS.
- Full POSIX compatibility (small measured subset only).
- Full package ecosystem.
- Real-hardware driver support (VM targets only).
- Production cryptographic supply-chain infrastructure.

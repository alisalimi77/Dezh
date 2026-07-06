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

#### W3 — Agent containment demo (differentiator F1; ties W1+W2 together)

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
reboot. Transcript: `docs/demo-transcript-agent-f1.md`. Found and fixed a
latent W1 bug on the way: the storage daemon truncated every sector write to
511 bytes, corrupting any package larger than two sectors. Remaining polish:
fold the mediation benchmark numbers into the published F1 material.

#### W4 — Pol: run a real foreign binary (differentiator F4)

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

#### W5 — x86_64 to parity for F3 (largest chunk)

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
`-kernel` PVH path still works for CI. **Deferred (M2):** a full IDT/exception
handler + timer on x86 — the flagship demos are straight-line and do not need
it, but until it lands a CPU exception triple-faults the VM. Tracked as an
honest limitation (W7 / STATUS).

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

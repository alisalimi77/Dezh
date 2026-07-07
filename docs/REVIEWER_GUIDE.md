# Reviewer Guide

This guide is organized around the four flagship demos — one per differentiator.
Each is reproducible from a fresh clone and asserted in CI. For the honest scope
of what is and is not true, read [STATUS.md](STATUS.md) first; for the security
argument, [SECURITY_MODEL.md](SECURITY_MODEL.md).

## Setup

```sh
cargo test --locked --workspace
(cd dezh-core && cargo test --locked)       # shared IR engine + .dzp format
(cd dezh-boot && cargo build --locked)      # RISC-V kernel
(cd dezh-boot-x86 && cargo build --locked)  # x86_64 kernel
```

The fastest single check is the RISC-V smoke test, which drives every RISC-V
demo end to end and fails loudly if any capability, isolation, or storage signal
is missing:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

## The four flagship demos

### F1 — Agent containment (D001/D013)

An agent app works inside its grant, is DENIED by the kernel beyond it, delegates
an *attenuated* capability over IPC, and its damage is rolled back.

```sh
python tools/demo/run_agent_demo.py \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu-riscv qemu-system-riscv64
```

Or interactively at the `dezh>` prompt: `agent`, then `spy` (no-cap app is
denied by the kernel), then `cairn-rollback`.

**Claim:** authority is explicit, unforgeable, and attenuable — enforced by
hardware privilege + paging, not a sandbox policy file.

### F2 — Cairn storage (D004/D005)

Versioned state: commit, corrupt, roll back → restored, and restored *across a
reboot*. A second app is denied the first app's namespace.

Interactive: `cairn-demo`, `cairn-log note`, `cairn-rollback note 1`,
`cairn-verify note`. The smoke test also power-cycles the disk and re-checks the
rolled-back value.

**Claim:** state recovery is structural (versioned objects + refs), not fsck;
per-app namespaces are capability-gated by kernel-attested sender caps.

### F3 — Multi-ISA apps (D003/D016)

The same byte-identical Dezh-IR payload runs on both kernels.

```sh
# x86_64 kernel runs the .dzp agent package (pack -> parse -> verify -> run):
python tools/ci/qemu_smoke.py x86_64 \
  --kernel dezh-boot-x86/target/x86_64-unknown-none/debug/dezh-boot-x86 \
  --qemu qemu-system-x86_64
```

The byte-identity is pinned by `dezh-core`'s `demo_sum_bytes_are_pinned` test
(len + CRC-32). The RISC-V `agent` demo runs the same bytes.

**Claim:** apps are ISA-portable by construction; proven today on 2 ISAs.

### F4 — Pol compatibility (D007/D011/D014)

A real, unmodified static Linux/RISC-V ELF (built for
`riscv64gc-unknown-linux-musl`, no Dezh code) runs under the Linux personality,
capability-gated.

Interactive: `linux-elf` (serviced with the PRINT cap, DENIED without,
unsupported syscall → clean `-ENOSYS`), and `bench-pol` for the measured
translation overhead. The same ELF also runs on real riscv64 Linux
(`qemu-riscv64-static dezh-boot/linux-guest/target/.../linux-guest`).

**Claim:** near-native compute for same-ISA binaries (no emulation); syscall
translation overhead measured and honestly scoped in
[BENCH.md](../dezh-boot/BENCH.md). Coverage is a small subset today.

## Boot it like a real OS

`tools/x86/build-iso.sh` builds a GRUB Multiboot2 ISO that boots the x86 kernel
in QEMU `-cdrom` and in VirtualBox/VMware. See [QUICKSTART_VM.md](QUICKSTART_VM.md).

## Strong review questions

- Are capabilities checked at the right enforcement points, on both the syscall
  and the memory boundary?
- Does the driver grant model avoid hidden device authority?
- Is attenuation-plus-rollback an adequate substitute for runtime revocation for
  the agent use case? Where does it break?
- Are the benchmark caveats (emulated vs native) stated honestly enough?
- Which assumptions need formalization before any production claim?

## Public hygiene scan

```sh
python tools/review/scan_public.py
```

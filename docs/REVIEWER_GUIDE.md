# Reviewer guide

For someone judging whether the claims hold: what to check and in what order, how
to run each demo yourself, and the questions reviewers ask first.

Captured output from real runs is in [transcripts/](transcripts/) - that output is
produced by the kernel, not written by hand.

---

## What to check

<!-- was docs/REVIEWER_GUIDE.md until the 2026-07-23 consolidation -->

This guide is organized around the four flagship demos — one per differentiator.
Each is reproducible from a fresh clone and asserted in CI. For the honest scope
of what is and is not true, read [STATUS.md](STATUS.md) first; for the security
argument, [Enforcement model](SECURITY_MODEL.md#enforcement-model).

### Setup

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

### The four flagship demos

#### F1 — Agent containment (D001/D013)

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

#### F2 — Cairn storage (D004/D005)

Versioned state: commit, corrupt, roll back → restored, and restored *across a
reboot*. A second app is denied the first app's namespace.

Interactive: `cairn-demo`, `cairn-log note`, `cairn-rollback note 1`,
`cairn-verify note`. The smoke test also power-cycles the disk and re-checks the
rolled-back value.

**Claim:** state recovery is structural (versioned objects + refs), not fsck;
per-app namespaces are capability-gated by kernel-attested sender caps.

#### F3 — Multi-ISA apps (D003/D016)

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

#### F4 — Pol compatibility (D007/D011/D014)

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

### Boot it like a real OS

`tools/x86/build-iso.sh` builds a GRUB Multiboot2 ISO that boots the x86 kernel
in QEMU `-cdrom` and in VirtualBox/VMware. See [Running in a VM](GETTING_STARTED.md#running-in-a-vm).

### Strong review questions

- Are capabilities checked at the right enforcement points, on both the syscall
  and the memory boundary?
- Does the driver grant model avoid hidden device authority?
- Is attenuation-plus-rollback an adequate substitute for runtime revocation for
  the agent use case? Where does it break?
- Are the benchmark caveats (emulated vs native) stated honestly enough?
- Which assumptions need formalization before any production claim?

### Public hygiene scan

```sh
python tools/review/scan_public.py
```

---

## Running the demos

<!-- was docs/DEMO_SCRIPT.md until the 2026-07-23 consolidation -->

This script assumes the RISC-V kernel has been built and QEMU is available.

### Run The Automated Demo

```sh
python tools/demo/run_review_demo.py \
  --qemu-riscv qemu-system-riscv64 \
  --transcript docs/transcripts/riscv64.md
```

On Windows, pass the full QEMU path if it is not on `PATH`.

### Manual Command Sequence

At the `dezh>` prompt, run:

```text
version
about
ipc-typed-demo
ipcstat
services
install --dry-run
install run
apps installed
app-permissions lab
app-run lab
calc 7 + 5
calc-history
vault-put demo-secret
vault-get
app-deny vault
svc-stop virtio-block
read
svc-restart virtio-block
write recovered
read
svc-fault-demo virtio-block
read
svc-restart virtio-block
bench-all
halt
```

### Expected Signals

The transcript should include:

- `boot contract VALIDATED`
- `[typed-ipc] PASS`
- `VirtioBlock state=Running`
- `dry-run complete; disk not modified`
- `Install Report: Dezh Root v1`
- `[installed] lab`
- `[installed] calc`
- `[installed] vault`
- `Dezh Lab :: installable app system probe`
- `PASS: scheduler, IPC, installer launch, and UI path cooperated`
- `[calc] 7 + 5 = 12`
- `calc last = "7 + 5 = 12`
- `vault value = "demo-secret`
- `vault device/block direct access denied; console survived`
- `svc-stop virtio-block status=0 state=Stopped`
- `virtio-block unavailable; command failed cleanly`
- `svc-restart virtio-block state=Running`
- `svc-fault-demo virtio-block request_status=0 state=Faulted`
- `[bench-all] PASS`

### Short Review Path

For a shorter run, use:

```sh
python tools/demo/run_review_demo.py --mode short --qemu-riscv qemu-system-riscv64
```

The short run exercises boot, typed IPC, service startup, app install/run,
service stop/restart, service fault/restart, and halt.

---

## FAQ

<!-- was docs/FAQ.md until the 2026-07-23 consolidation -->

### Is Dezh production-ready?

No. Dezh is a working research prototype intended for architectural review. It
boots, runs isolated tasks, uses a user-space block driver, validates typed IPC,
and exercises a transactional package lifecycle in QEMU, but it is not a
production OS.

### Why publish it now?

The project is at the point where its core thesis can be inspected through
code, QEMU transcripts, and repeatable tests. Public review is useful before
the design becomes too large to change.

### What is the main technical thesis?

Dezh explores intent-scoped authority and effect accountability. A program
should receive the narrow authority needed for a specific effect, and important
state changes should be visible, recoverable, and tied to an explicit service
route or transaction.

### Is Dezh a Unix clone?

No. Dezh intentionally avoids starting from ambient files, ambient devices,
ambient process inheritance, or a global package registry. Compatibility layers
may exist later, but they should not define the core authority model.

### Is Dezh a microkernel?

It shares some microkernel instincts, especially user-space drivers and service
boundaries, but the current goal is not to fit a label. The important boundary
is explicit authority: kernel code should enforce isolation and routing, while
device and storage effects should be delegated through granted user-space
services.

### Why user-space virtio-block?

The block device is a useful proof point: persistent storage should not require
a hidden kernel I/O path. The current `virtio-block` daemon runs in U-mode and
receives explicit MMIO and DMA grants.

### How are apps installed?

The SDK builds `.dzp` packages. The package store writes registry, journal, and
blob sectors through the registered user-space block service. Only `Active`
packages are runnable.

### What prevents half-installed apps?

Package install/remove uses a journaled state machine. Interrupted installs are
rolled back, committed only when checks match, quarantined if suspicious, or
blocked when the journal is corrupt.

### What should reviewers focus on first?

The highest-value review areas are:

- capability boundaries
- user-space driver grants
- typed IPC status handling
- service stop/fault/restart semantics
- package journal recovery
- package capability escalation review
- denial proofs and failure behavior

### Why not build this on seL4 (or Genode)?

The most important question, answered honestly. seL4 is a formally verified
capability microkernel; Genode is a mature capability component OS with
user-space drivers and typed IPC. For a *product*, building the Dezh model on top
of one of them would be the right call — you would inherit verification, real
object-capabilities, and IOMMU support instead of re-deriving them.

So the from-scratch kernel is **not the contribution, and we do not claim it is**
(see [DECISIONS.md](DECISIONS.md) D021). The contribution is the *model*: intent
as the sole authority-derivation path, an effect ledger on the authorization
path with a reversibility class, and honest whole-mission rollback, aimed at
autonomous agents. We wrote a small kernel to prototype that model end to end
with nothing hidden underneath and full control of the substrate while the ideas
were still moving — the pedagogical and iteration reasons, not a claim that the
world needs another microkernel.

The honest consequence: several things seL4/Genode already do well (verification,
per-object capabilities, IOMMU) are gaps here, named in
[STATUS.md](STATUS.md) and [Threat model](SECURITY_MODEL.md#threat-model). A credible
productization path is to **port the intent→effect model onto seL4 or Genode**
and keep the model, not the kernel. If a reviewer's takeaway is "the ideas are
interesting but belong on a verified base," that is a conclusion we agree with.

### What is intentionally out of scope right now?

- production bootloader and installer media
- production networking (and with it, information-flow / exfiltration control)
- dependency solving
- real IOMMU integration
- graphics stack
- real hardware bring-up
- formal verification of the whole system
- online PKI / certificate-transparency for package signing (the signing
  *mechanism* now exists — see [Package signing](SUBSYSTEMS.md#package-signing) — but the
  key-distribution layer does not)

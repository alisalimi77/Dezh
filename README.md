# Dezh OS

Dezh OS is a capability-secure operating-system research prototype. Its core
rule is simple: no program, service, driver, or app starts with ambient
authority. Every effect requires an explicit capability or grant.

The current bare-metal prototype boots on QEMU RISC-V, validates a boot
contract, starts a capability-scoped console, runs isolated U-mode processes,
launches a long-lived user-space virtio-block driver, installs small apps
through a disk-backed registry, and exercises typed IPC plus service
supervision.

This repository is not a production operating system. It is a working research
artifact intended for technical review.

## What Works Today

- Bare-metal RISC-V boot on QEMU `virt` in S-mode through OpenSBI.
- Sv39 process isolation with U-mode page faults contained to the faulting task.
- Explicit task capabilities for print, time, IPC, device, and block access.
- User-space `virtio-block` daemon with explicit MMIO and DMA grants.
- Typed IPC v0 with status codes, request IDs, timeout support, and counters.
- Boot-managed service registry with stop, restart, and controlled fault demo.
- Disk-backed install/root marker and app registry v0.
- Installable demo apps: `note` and `lab`.
- Storage path through the registered user-space block service, not a hidden
  kernel block driver.
- x86_64 boot smoke that validates the shared Dezh IR path.

## Architecture Thesis

Dezh explores an OS design where authority is never inherited by default. The
kernel owns only the confinement boundary, address-space setup, syscall gates,
IPC, and service supervision. Device access and persistent storage are delegated
to user-space services through explicit grants.

The current prototype focuses on these pillars:

- **No ambient authority:** effects require explicit caps.
- **User-space drivers:** virtio-block runs as a separate U-mode process.
- **Typed IPC:** services reply with structured status rather than raw scalar
  success/failure conventions.
- **Service supervision:** critical daemons can stop, fault, and restart without
  killing the console.
- **Transactional install path:** app installation is registry-backed and
  service-mediated.
- **Rollback-oriented storage:** the v0 root path keeps current and previous
  durable values.

## Quick Review Path

Prerequisites:

- Rust stable with targets:
  - `wasm32-unknown-unknown`
  - `riscv64gc-unknown-none-elf`
  - `x86_64-unknown-none`
- QEMU:
  - `qemu-system-riscv64`
  - `qemu-system-x86_64`

Run host tests:

```sh
cargo test --locked --workspace
```

Build the bare-metal kernels:

```sh
cd dezh-boot
cargo build --locked
cd ../dezh-boot-x86
cargo build --locked
cd ..
```

Run the RISC-V smoke test with a real temporary disk image:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

Run the external-review demo and write a transcript:

```sh
python tools/demo/run_review_demo.py   --qemu-riscv qemu-system-riscv64   --transcript docs/demo-transcript-riscv64.md
```

Run the public hygiene scan:

```sh
python tools/review/scan_public.py
```

## Console Commands Worth Reviewing

Inside the RISC-V console:

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

These commands demonstrate typed IPC, service-mediated app storage, app launch,
multi-app installation, no-grant denial, clean service unavailability, explicit
restart, service fault recovery, and a small terminal control surface for
reviewing runtime state.

## Documentation

- [Whitepaper](docs/WHITEPAPER.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Security model](docs/SECURITY_MODEL.md)
- [Demo script](docs/DEMO_SCRIPT.md)
- [Reviewer guide](docs/REVIEWER_GUIDE.md)
- [Roadmap](docs/ROADMAP.md)
- [Architecture decisions](docs/DECISIONS.md)
- [Outreach templates](docs/OUTREACH.md)

## Current Limitations

- RISC-V is the primary bare-metal target; x86_64 currently has a smaller smoke
  path.
- The block driver uses virtio-mmio legacy mode in QEMU.
- DMA isolation is modeled through page-table discipline and fixed grants; a
  real IOMMU path is future work.
- App bundles are embedded in the kernel image for v0.
- Registry hashes are deterministic v0 markers, not production cryptographic
  package signatures.
- The installer initializes a simple disk layout; it is not a full production
  boot media installer yet.

## Review Goal

The goal of this repository state is technical feedback: architecture review,
security-model critique, demo reproducibility, and discussion of use cases such
as secure devices, cloud sandboxes, agent runtimes, and service-isolated systems.

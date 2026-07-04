# Getting Started

This guide is the shortest path to validating Dezh locally.

## Prerequisites

Install:

- Rust stable
- Python 3.10 or newer
- QEMU:
  - `qemu-system-riscv64`
  - `qemu-system-x86_64`

Install Rust targets:

```sh
rustup target add wasm32-unknown-unknown
rustup target add riscv64gc-unknown-none-elf
rustup target add x86_64-unknown-none
```

## Clone And Test

```sh
git clone https://github.com/alisalimi77/Dezh.git
cd Dezh
cargo test --locked --workspace
```

## Build The Bare-Metal Kernels

```sh
cd dezh-boot
cargo build --locked
cd ../dezh-boot-x86
cargo build --locked
cd ..
```

## Run The RISC-V Smoke Test

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

This boots the RISC-V kernel in QEMU with a real temporary disk image and
checks the console, service registry, typed IPC, storage path, package path,
denial proofs, and benchmark command.

## Run The SDK Package Acceptance Test

```sh
python tools/ci/sdk_test.py \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

This validates that a `.dzp` package can be built, installed, run, denied,
removed, recovered, updated, rolled back, pinned, unpinned, and garbage
collected through the service-mediated package store.

## Run The Public Hygiene Scan

```sh
python tools/review/scan_public.py
```

The scan checks public-facing files for private paths, secret-like tokens, and
non-neutral identity/geography markers.

## One-Command Review Runner

For a consolidated pass:

```sh
python tools/review/run_full_review.py --quick --qemu-riscv qemu-system-riscv64 --qemu-x86 qemu-system-x86_64
```

Use `--full` to include the longer SDK package lifecycle acceptance test.

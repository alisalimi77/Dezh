# Build And Run

This document describes repeatable local validation for Dezh OS.

## Toolchain

Required:

- Rust stable
- Python 3.10 or newer
- QEMU RISC-V and x86_64 system emulators

Rust targets:

```sh
rustup target add wasm32-unknown-unknown
rustup target add riscv64gc-unknown-none-elf
rustup target add x86_64-unknown-none
```

## Windows PowerShell

If QEMU is installed in the default Windows path:

```powershell
$QemuRiscv = "C:/Program Files/qemu/qemu-system-riscv64.exe"
$QemuX86 = "C:/Program Files/qemu/qemu-system-x86_64.exe"
```

Build:

```powershell
cargo test --locked --workspace
Push-Location dezh-boot
cargo build --locked
Pop-Location
Push-Location dezh-boot-x86
cargo build --locked
Pop-Location
```

RISC-V smoke:

```powershell
python tools\ci\qemu_smoke.py riscv64 `
  --kernel dezh-boot\target\riscv64gc-unknown-none-elf\debug\dezh-boot `
  --qemu $QemuRiscv
```

Interactive RISC-V boot with a local disk image:

```powershell
fsutil file createnew dezh-local.img 2097152
& $QemuRiscv `
  -machine virt `
  -nographic `
  -bios default `
  -kernel dezh-boot\target\riscv64gc-unknown-none-elf\debug\dezh-boot `
  -drive file=dezh-local.img,format=raw,if=none,id=dezhdisk `
  -device virtio-blk-device,drive=dezhdisk
```

At the prompt, try:

```text
help
status
services
ipc-typed-demo
install run
pkg-store
bench-all
halt
```

## Linux

Install QEMU using the distribution package manager. On Debian or Ubuntu:

```sh
sudo apt-get update
sudo apt-get install -y qemu-system-misc qemu-system-x86
```

Build:

```sh
cargo test --locked --workspace
(cd dezh-boot && cargo build --locked)
(cd dezh-boot-x86 && cargo build --locked)
```

Run:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

## macOS

Install QEMU with Homebrew:

```sh
brew install qemu
```

Build and smoke commands are the same as Linux.

## Review Validation

Run the consolidated quick review:

```sh
python tools/review/run_full_review.py --quick
```

Run the longer review path:

```sh
python tools/review/run_full_review.py --full
```

The full path runs public hygiene checks, host tests, RISC-V and x86_64 builds,
RISC-V QEMU smoke, review demo transcript generation, and SDK package lifecycle
acceptance.

## Troubleshooting

If QEMU is not found, pass the full path using `--qemu`, `--qemu-riscv`, or
`--qemu-x86`, depending on the script.

If the RISC-V console appears but the Enter key does not work in a terminal,
use the scripted smoke runner. The console accepts carriage return and newline,
but some terminal pipelines buffer input differently.

If package commands fail with `virtio-block unavailable`, confirm that QEMU was
started with:

```text
-drive file=...,format=raw,if=none,id=dezhdisk
-device virtio-blk-device,drive=dezhdisk
```

# Reviewer Guide

## What To Review First

1. Read [README.md](../README.md).
2. Read [SECURITY_MODEL.md](SECURITY_MODEL.md).
3. Run the RISC-V smoke test.
4. Run the review demo.
5. Inspect `dezh-boot/src/main.rs` and `dezh-boot/virtio-blk/src/main.rs` for
   the IPC, service, and grant paths.

## Build And Test

```sh
cargo test --locked --workspace
cd dezh-boot && cargo build --locked && cd ..
cd dezh-boot-x86 && cargo build --locked && cd ..
```

RISC-V smoke:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

x86_64 smoke:

```sh
python tools/ci/qemu_smoke.py x86_64 \
  --kernel dezh-boot-x86/target/x86_64-unknown-none/debug/dezh-boot-x86 \
  --qemu qemu-system-x86_64
```

Review demo:

```sh
python tools/demo/run_review_demo.py --qemu-riscv qemu-system-riscv64
```

Public hygiene scan:

```sh
python tools/review/scan_public.py
```

## Strong Review Questions

- Are capabilities checked at the correct enforcement points?
- Does the driver grant model avoid hidden device authority?
- Does typed IPC make failures observable enough for larger services?
- Does service stop/fault/restart behavior preserve console liveness?
- Is the app registry model a plausible basis for a real installer?
- Which assumptions need formalization before production hardening?

## Expected Limitations

The current prototype is intentionally narrow. It should be judged as an
architecture and isolation demo, not as a finished OS distribution.

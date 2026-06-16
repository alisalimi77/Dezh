# dezh-kernel - Step 10 Kernel Boot Contract Scaffold

This crate starts Step 10 without pretending the kernel has already booted.
It is a `no_std`-compatible contract model for the minimal QEMU-first boot path.

The v0 model defines:

- QEMU-first targets
- memory map validation
- init service launch plan
- user-space service specs for `init`, `cairn`, `wasm-runtime`, and `virtio-block`
- explicit capability seeds for each service
- rejection of ambient capability seeds for unknown services

## What This Validates Now

The tests validate the boot contract shape that the real boot path must satisfy:

- memory regions must be non-empty, non-overlapping, and contain usable memory
- init must exist
- every service capability must be seeded explicitly
- capability seeds for unknown services are rejected as ambient authority

## What Is Not Validated Yet

This is not a bootable image yet. It does not include a linker script, entry
assembly, page-table setup, interrupt handling, allocator, serial driver,
bootloader integration, or a QEMU run target.

Step 10 should only be marked `validated` after a real QEMU boot prints the
kernel contract banner.

Run the focused tests with:

```sh
cargo test -p dezh-kernel
```

# dezh-boot — Step 10: real bare-metal kernel boot

The first Dezh code that runs on bare metal. A `no_std` RISC-V 64 kernel that
boots on the QEMU `virt` board, runs its boot description through the validated
[`dezh-kernel`](../dezh-kernel) contract, and prints the banner + init service
plan over the UART before halting. This crosses the simulation → bare-metal
boundary every earlier spike ran around.

## What it proves

- Comes up in **S-mode** after OpenSBI (`-bios default`) jumps to `0x8020_0000`.
- The boot plan is checked by the **same `plan_boot` contract** that has unit
  tests — capability seeds are bound to declared services (no ambient authority)
  even at the first instruction after firmware.
- Exits QEMU cleanly via the SiFive test finisher.

## Layout

- `src/main.rs` — entry asm (zero `.bss`, set stack, call `kmain`), NS16550 UART
  driver, a tiny bump global allocator (`alloc` is needed by `dezh-kernel`), the
  `kmain` boot flow, and a panic handler.
- `linker.ld` — places `.text` at `0x8020_0000` (the OpenSBI hand-off address).
- `.cargo/config.toml` — defaults the build to `riscv64gc-unknown-none-elf`.
- `build.rs` — applies the linker script.

This crate is a **standalone workspace**, excluded from the root workspace,
because it cross-compiles to bare metal (no host linker, no MSVC needed).

## Build & run

Prerequisites (already set up on the dev machine):

```sh
rustup target add riscv64gc-unknown-none-elf
# QEMU with RISC-V system support (qemu-system-riscv64) on PATH
```

```sh
cd dezh-boot
cargo build
qemu-system-riscv64 -machine virt -nographic -bios default \
    -kernel target/riscv64gc-unknown-none-elf/debug/dezh-boot
```

Expected tail of the output:

```
[dezh-boot] alive on bare metal (qemu virt, riscv64, S-mode)
[dezh-boot] boot contract VALIDATED
[dezh-boot] banner: dezh-kernel-boot-v0:qemu-virtio-riscv64:services=4:usable_bytes=132120576
[dezh-boot] init services (each launched with explicit caps):
              - init
              - cairn
              - wasm-runtime
              - virtio-block
[dezh-boot] no ambient authority: capability seeds bound to declared services only
[dezh-boot] OK — halting
```

QEMU exits with code 0 on success.

## Not yet

No traps, timer, paging, or actual service launch. Next milestone: set up the
trap vector + timer, then launch a first capability-seeded user-space service —
keeping each step under the no-ambient-authority thesis.

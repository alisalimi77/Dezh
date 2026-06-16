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
- **Hardware event loop:** installs an S-mode trap vector (`stvec`), arms the
  SBI timer, enables supervisor interrupts, and counts a silent background
  uptime tick in `trap_handler`.
- **Dezh's own console** over the UART: a real read/eval/print loop where every
  command is gated by an explicit capability. The console holds a fixed,
  narrow capability set and **denies** any command whose capability it was not
  granted (`secret` in the demo) — no-ambient-authority, now interactive on
  bare metal.
- Exits QEMU cleanly via the SiFive test finisher when you run `halt`.

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

Interactive (type commands yourself; `halt` exits):

```sh
cd dezh-boot
cargo build
qemu-system-riscv64 -machine virt -nographic -bios default \
    -kernel target/riscv64gc-unknown-none-elf/debug/dezh-boot
```

Automated, reproducible transcript (boots, scripts the commands, drains output):

```sh
pwsh dezh-boot/scripts/console-test.ps1
```

Example console session:

```text
Dezh console. Every command requires an explicit capability.
Type 'help'. The console holds: INSPECT TIME ECHO HALT
dezh> caps
console capabilities: INSPECT TIME ECHO HALT
dezh> echo hello dezh
hello dezh
dezh> secret
denied: 'secret' requires capability SECRET (not held)
dezh> uptime
uptime: 19 ticks (~1.9 s)
dezh> halt
halting.
```

QEMU exits with code 0 after `halt`.

## Commands

`help`, `caps`, `mem`, `services`, `uptime`, `echo <text>`, `halt` — each gated by
a capability the console holds. `secret` requires a capability the console is
never granted, so it is always denied (the no-ambient-authority demo).

## Not yet

No paging/virtual memory, and no actual user-space service launch. Done so far:
boot contract + S-mode trap vector + SBI timer + a capability-gated console.
Next milestone: launch a first capability-seeded user-space task (drop to
U-mode, handle its `ecall` as a capability-checked request), keeping each step
under the no-ambient-authority thesis.

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
- **U-mode tasks** (`run`): drops a task to the **U privilege level** with zero
  authority of its own. The task can only reach the kernel through `ecall`s,
  each checked against the *task's* capabilities; a syscall it wasn't granted
  (`sys_uptime`, lacking `TIME`) is denied at the kernel boundary. A real
  S→U→S context switch returns control to the console afterward.
- **Hardware memory isolation** (Sv39 paging): kernel + MMIO pages are
  supervisor-only (U=0); only the task's own region is U=1. A task that touches
  anything else (`rogue` writes the UART directly) takes a **page fault** and is
  killed by the kernel — the console survives. The no-ambient-authority thesis
  is now enforced at both the **syscall** and the **hardware memory** boundary,
  not just by Rust types.
- **Multitasking + scheduler** (`multi`): several U-mode tasks share the CPU
  round-robin, each with its own stack and capability set. A full register
  context switch (`utrap`) saves/restores every task; tasks cooperate via a
  `yield` syscall and their output interleaves. (Cooperative for now; timer
  preemption is a planned refinement.)
- **Capability-passing IPC** (`ipc`): the microkernel keystone. One task sends
  another a message carrying a *delegated* capability; the kernel enforces that
  a sender can only delegate authority it holds (attenuation, never widening).
  Demo: a `service` task starts with no authority and is denied when it tries to
  print; an `agent` task then delegates its `PRINT` capability over IPC, and only
  then can the service print. This is how agents call services and spawn
  sub-agents with reduced authority — the foundation for the agent-first OS
  (D008/D013). (Zero-copy object handoff per D018 is a later optimization.)
- **Pol / Linux personality** (`linux`): a U-mode app speaking the real Linux
  riscv64 syscall ABI (`write`=64, `exit`=93) runs unmodified — the kernel's Pol
  layer translates each Linux syscall into a capability-checked Dezh action, and
  an unsupported syscall returns `ENOSYS`. The app has zero ambient authority;
  it only reaches the console because it holds the `PRINT` capability. A first
  taste of legacy compatibility *on the kernel* (D014).
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

`help`, `caps`, `mem`, `services`, `uptime`, `echo <text>`, `run`, `halt` — each
gated by a capability the console holds. `secret` requires a capability the
console is never granted, so it is always denied (the no-ambient-authority demo).
`run` spawns a U-mode task granted only `PRINT` (not `TIME`); watch `sys_uptime`
get denied at the kernel boundary, then control return to the console. `rogue`
spawns a task that writes the UART directly; watch it take a page fault and get
killed while the console survives. `multi` runs three cooperative tasks that
interleave via `yield`. `linux` runs a Linux-ABI app through the Pol layer
(watch `close()` come back as `ENOSYS`). `ipc` runs an agent that delegates its
`PRINT` capability to a no-authority service over a message (watch the service be
denied, then succeed once the capability is delegated). `bench` measures the
ecall round-trip cost (see [BENCH.md](BENCH.md) for the real-hardware comparison
vs Linux). `ipc`
shows an agent delegating a capability to a service over a message.

## Not yet

Scheduling is cooperative (no timer preemption yet) and tasks share one user
region; the kernel/user split uses coarse 1 GiB / 2 MiB pages (W^X not yet
enforced). Next milestones: timer preemption and per-task regions, then more of
the first real Pol personality (Linux) on the kernel — each step under the
no-ambient-authority thesis, now enforced at the hardware boundary.

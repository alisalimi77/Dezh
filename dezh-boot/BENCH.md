# Dezh — first comparative benchmark (backs D015)

D015 says performance claims must come from a specific architectural lever **and**
a measurement against a real baseline — never a bare "faster than X". This is the
first such measurement. It is deliberately narrow and honest about what is and is
not comparable.

## Numbers

| Measurement | Value | Where measured |
| --- | --- | --- |
| Dezh capability check (authority decision) | **0.98 ns** | native, real CPU (host process, `cargo run --release --bin cap-bench`, 100,000,000 iters) |
| Linux `getpid` raw syscall round trip | **49.0 ns** | native, real CPU (Ubuntu/WSL, `gcc -O2`, `syscall(SYS_getpid)`, 5,000,000 iters) |
| Dezh `ecall` round trip (kernel `bench`) | **~1041 ns** | **QEMU-emulated** RISC-V (`virt`), 500,000 iters, `time` CSR @ 10 MHz |
| Pol Linux-ABI translation overhead (kernel `bench-pol`) | **~0–80 ns/call** (within noise) | **QEMU-emulated** RISC-V (`virt`), 200,000 iters, delta vs native path |

CPU: 13th Gen Intel Core i7-13650HX. Linux: x86_64 Ubuntu under WSL2.

## What is — and isn't — comparable

- **Comparable (same physical CPU, native):** the Dezh **capability check**
  (0.98 ns) vs the Linux **syscall floor** (49 ns). These measure two different
  architectural primitives on identical hardware:
  - Linux gates access by trapping into the kernel — even the cheapest syscall
    pays ~49 ns for the privilege transition before any work.
  - Dezh's authority decision is an in-process capability check — ~1 ns, ~**50×**
    cheaper than even Linux's cheapest syscall. This is the architectural lever
    behind D015/D018: authority is checked inline and data is shared zero-copy
    via capabilities, instead of paying a syscall per mediated access.

- **NOT directly comparable:** the Dezh **`ecall` round trip** (~1041 ns) is
  measured **inside QEMU's TCG emulator**, not on real silicon. Emulated trap
  costs are far higher and non-representative of hardware. It is reported only to
  show the kernel's U→S→U path works and to track *relative* changes; it must not
  be compared against the native Linux number above.

## Pol translation overhead (flagship F4)

F4 (the Linux personality, D014) claims *near-native compute with minimized —
not zero — syscall-translation overhead*. `bench-pol` measures the translation
cost directly and honestly. It runs the **same zero-work syscall** 200,000 times
by two paths on the same kernel:

- **native** — the Dezh `SYS_PRINT` syscall with a zero-length buffer, and
- **Pol** — the real Linux `write(2)` ABI (`a7=64`) with a zero-length buffer,
  routed through the personality layer.

Both are capability-checked and neither touches the UART, so the only kernel-side
difference is the personality branch plus the Linux-ABI decode. The kernel times
each run and reports the delta:

```text
native SYS_PRINT round-trip:   ~766 ns/call (QEMU-emulated)
Pol Linux write(2) round-trip: ~812 ns/call (QEMU-emulated)
Pol translation overhead:      ~46 ns/call  (delta over native, emulated)
```

Across runs the delta ranges **~0–80 ns/call** — under ~10% of the (emulated)
~780 ns round trip, and often inside run-to-run noise. The honest
reading: **Pol adds a fixed, near-noise per-syscall dispatch; the round-trip
cost is dominated by the trap itself, not by translation.** The compute *between*
syscalls is the native binary running directly on the CPU — there is no
interpreter or JIT (the very same ELF also runs unmodified on real riscv64
Linux; see the F4 demo). Absolute numbers are QEMU-emulated and not comparable
to real silicon; the *delta* is the meaningful figure because both paths pay the
identical emulated trap cost, which cancels in the subtraction.

## Honest reading

This does **not** prove "Dezh is faster than Linux." It proves one specific,
real-hardware thing: **mediating access by capability is ~50× cheaper than
mediating access by syscall.** Whether that translates into end-to-end wins
depends on real-hardware kernel benchmarks we have not run yet (Dezh has no
real-silicon port). The microkernel's IPC cost (D015) still has to be paid back
by zero-copy capability passing (D018); measuring that end-to-end, against Linux
under identical conditions (ideally both under QEMU, or both on real hardware),
is the next benchmarking milestone.

## Reproduce

```sh
# Dezh capability check (native):
cargo run --release --bin cap-bench

# Dezh ecall round trip (QEMU): boot, then type `bench`
cargo build -p dezh-boot   # (from dezh-boot/, or build the standalone crate)
qemu-system-riscv64 -machine virt -nographic -bios default \
    -kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot
# dezh> bench
# Pol translation overhead (F4): boot, then type `bench-pol`
# dezh> bench-pol

# Linux syscall floor (real Linux):
gcc -O2 linux_syscall_bench.c -o lsb && ./lsb
```

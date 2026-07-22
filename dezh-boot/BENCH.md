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

## What this does and does not measure

Read the two native numbers as **two different primitives**, not as an
end-to-end "Dezh is faster than Linux" claim — which this benchmark explicitly
does **not** support:

- The **capability check** (0.98 ns, native) is the cost of the *authorization
  decision itself* — a bitmask test — in isolation. It is **not** the cost of
  performing an effect.
- The **Linux syscall floor** (49 ns, native) is the cost of the privilege
  transition for the cheapest possible syscall.

These are not like-for-like, and we do not claim they are. The 0.98 ns is an
in-process, likely-inlined check on the host; the 49 ns is a real `getpid`
through the glibc wrapper under WSL2. The honest conclusion is deliberately
narrow: **the authorization decision is nearly free relative to the trap any OS
pays to cross the privilege boundary.**

It is **not** that "a mediated effect is ~50× cheaper on Dezh." Performing a real
effect on Dezh also pays an `ecall` (the U→S→U round trip), exactly like a
syscall — the capability check rides *on top of* that trap, it does not replace
it. On the same platform both mediation paths pay a trap; what Dezh changes is
that the authorization on top of the trap is inline and cheap, and that data can
be passed zero-copy via capabilities (D018) instead of copied per crossing.

The comparison that would actually settle "capability mediation vs syscall
mediation" — Dezh `ecall` + capability check vs Linux syscall + access check on
the **same** platform — is **not yet run** (Dezh has no real-silicon port, and
the emulated `ecall` below is not comparable to native). Until it is, we make no
end-to-end speed claim; this benchmark supports only the narrow lever above.

- **NOT comparable:** the Dezh **`ecall` round trip** (~1041 ns) is measured
  **inside QEMU's TCG emulator**, not on real silicon. Emulated trap costs are
  far higher and non-representative of hardware. It is reported only to show the
  kernel's U→S→U path works and to track *relative* changes; it must not be
  compared against the native Linux number above.

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

This does **not** prove "Dezh is faster than Linux," and it does **not** prove
"a mediated effect is ~50× cheaper." It proves one narrow thing: **the
authorization decision (a capability check) is nearly free relative to the
privilege-boundary trap any OS pays.** A real Dezh effect still pays that trap
(an `ecall`), so the end-to-end mediation cost is trap-dominated on both systems.
Whether capability mediation wins end-to-end depends on real-hardware kernel
benchmarks we have not run (Dezh has no real-silicon port). The microkernel's IPC
cost (D015) still has to be paid back by zero-copy capability passing (D018);
measuring that end-to-end against Linux under identical conditions — Dezh
`ecall`+check vs Linux syscall+check on the same platform — is the next
benchmarking milestone and the only thing that could support an end-to-end claim.

## Per-effect ledger overhead (W8, Sand)

The effect ledger (Sand) is **not a second write.** Every effect *is* a Cairn v1
commit; the "ledger" is the same append-only commit record, enriched so it
carries its own provenance. So the honest question for D015 is: *what does the
enrichment cost over a plain durable commit?*

**Marginal cost = a handful of byte writes into a sector already being written,
and zero extra I/O.** The enrichment occupies previously-spare bytes in the
512-byte commit header — intent/Ahd id (`u32`), derived capability set (`u32`),
reversibility class (`u8`), status (`u8`), generation (`u16`) — alongside the
actor, parent ref and object hash the commit already stored. Concretely:

| Cost dimension | Plain durable commit | Sand effect commit | Delta |
| --- | --- | --- | --- |
| Block writes per effect | commit sector + superblock | commit sector + superblock | **0** |
| IPC messages per effect | 1 (commit request) | 1 (commit request) | **0** |
| Header bytes written | actor+parent+hash+len | + intent+derived+revclass+status+gen | **+12 bytes, same sector** |
| Kernel→daemon threading | request-id + status byte | request-id (Ahd) + status byte (derived+revclass) | **0 extra fields** |

The intent id and derived cap ride fields the commit IPC **already** carries
(the request-id word and the status byte), so there is no extra message and no
extra sector. The dominant, measured cost of recording an effect is therefore
the durable commit's block round trip itself — the same figure the storage path
already pays (see `bench-storage` / `bench-all`, routed through the user-space
`virtio-block` daemon). The provenance enrichment sits *below* the measurement
noise of the emulated block path because it adds no I/O.

**Why this matters for the differentiator.** A user-space effect log (write the
action, then separately append to an audit file) pays a *second* write and can
be bypassed by anything that reaches the resource around the logger. Here the
effect path goes *through* the ledger: the same record that authorizes and
persists the effect is the ledger entry, and on a kernel with no ambient
authority there is no path to the resource that skips it. The cost of that
property is ~zero marginal I/O, not a tax.

Rollback/forecast are reads: `sfar-plan` and `tbar` walk the live per-namespace
chains (bounded by the 255-slot commit log); `sfar-rollback` retracts a
contiguous reversible head-run with **one** atomic superblock write regardless of
how many effects are retracted, and appends one commit per compensation.

*(All storage figures are QEMU-emulated; the architectural claim — no extra I/O
per effect — holds independent of the platform because it is a property of the
record layout, not of the timing.)*

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

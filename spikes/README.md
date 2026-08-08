# Spikes — the questions answered before the kernel existed

These are Steps 1..9. Each was a host-side program that settled one design
question in user space, so that the bare-metal work could start from an answer
instead of a hope. They did their job. **None of them ships.**

Nothing on the bare-metal path depends on anything here — the RISC-V and x86_64
kernels build against `dezh-core` and `dezh-kernel` only. The Cairn, IPC,
Dezh-IR and Linux-personality code that actually runs is in `dezh-boot`, written
against real hardware constraints these spikes did not have.

They are kept, and kept honest, rather than deleted: this repository treats the
record of *why* a design is what it is as part of the work (`docs/DECISIONS.md`),
and a superseded prototype is evidence, not clutter. What they must not do is
masquerade as live code — which is what moving them here fixes.

## What each one proved, and what superseded it

| Spike | The question it answered | Where the answer lives now |
| --- | --- | --- |
| `dezh-host` | Can a WASM guest be given an unforgeable capability handle, and denied without a policy file? | The syscall boundary and per-task capability bits in `dezh-boot`, enforced by hardware privilege rather than by a runtime |
| `dezh-cairn` | Should durable state be immutable content-addressed objects plus small mutable refs, so rollback is structural? | Cairn v1 in `dezh-boot`: an on-disk commit log with parent refs, per-app namespaces and reboot-surviving rollback |
| `dezh-identity` | Can authority be delegated and attenuated with provenance recorded? | The Ahd / Sand / Sfar / Tbar runtime in `dezh-boot` + `dezh-core::ocap` |
| `dezh-runtime` | Do capability, storage and identity compose — can a guest reach a ref *only* through a granted handle? | The Dezh-IR engine in `dezh-core`, whose effects go through the `Host` trait each kernel implements |
| `dezh-ir` | What are the typed contracts for a portable intermediate representation? | `dezh-core::ir`, with the demo bytecode pinned byte-identical by a test so both ISAs provably run the same bytes |
| `dezh-ipc` | Does an actor model hold up — state behind actors, capabilities transferred only by attenuation, a crash not corrupting its neighbours? | Typed IPC v0 in `dezh-boot`: kernel-attested sender capabilities, status codes, timeouts |
| `dezh-scheduler` | What scheduling shape does the service model need? | The preemptive scheduler and shared run queue in `dezh-boot` |
| `dezh-linux` | What would a Linux personality have to translate? | Pol in `dezh-boot`, running a real unmodified static riscv64 ELF |

## Building them

They are their own workspace, deliberately outside the default CI path — they
pull `wasmtime` and roughly 150 crates that nothing shipping needs:

```sh
cd spikes && cargo test
```

CI still runs them, on a separate job, so they cannot rot into non-compiling
history. If one ever fails in a way that is not worth fixing, deleting it and
leaving its row above is a better outcome than repairing code nothing runs.

## The rule

Do not add to this directory. A new design question gets answered in the kernel,
against real constraints, or it does not get answered. These exist because they
predate the kernel, which is a fact about history and not a pattern to continue.

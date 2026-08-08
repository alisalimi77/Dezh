# dezh-ipc — Step 6: user-space actor IPC spike

This crate validates Dezh's microkernel/process direction before any kernel
work starts. It is a user-space actor model with copied messages, capability
transfer through attenuated `AuthorityGrant`s, and panic isolation.

It proves:

- actors exchange messages without shared mutable state;
- capabilities transfer only by delegating an existing grant;
- transferred authority can be narrowed but not widened;
- a panicking actor is reported as `Panicked` and does not stop other actors;
- the message path can be benchmarked independently.

## Out of scope for v0

No async runtime, no kernel IPC, no shared memory, no priority inheritance, no
scheduler policy, no persistent mailboxes, and no cross-process address spaces.

## Run

```sh
cargo test -p dezh-ipc
cargo run --release -p dezh-ipc --bin ipc-bench
```

Measured on the development machine (`--release`, Windows/MSVC):

```text
per message ~= 0.081 us
```

This is a user-space `std::sync::mpsc` actor path with an 8-byte payload. It is
not a kernel IPC number.

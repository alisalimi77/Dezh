# dezh-runtime — Step 4: capability + Cairn + identity integration

This crate connects the first three Dezh spikes:

- Step 1: WASM guests can only act through unforgeable capability handles.
- Step 2: Cairn stores durable state as immutable objects plus refs.
- Step 3: identity records who acted, with which attenuated authority, and what
  artifacts were produced.

Step 4 proves the secure mutation path:

```text
WASM guest -> capability handle -> Cairn ref/object -> Invocation provenance
```

The runtime exposes only `cap_read` and `cap_write` in the `dezh` import module.
`cap_read` requires `READ_REF | READ_OBJECT`; `cap_write` requires `UPDATE_REF`.
Writes create a new Cairn object, move the granted ref through a Cairn commit,
and record an invocation with object and commit artifacts.

## Out of scope for v0

No general filesystem API, no rollback host call, no delegation inside the guest,
no persistent invocation log, no IPC, no scheduler, and no replacement of the
original Step 1 demo backend yet. This is an integration spike.

## Run

```sh
cargo test -p dezh-runtime
cargo run --release -p dezh-runtime --bin runtime-boundary-bench
```

## Benchmark labels

`runtime-boundary-bench` measures the full Step 4 boundary, not just a native
table lookup:

- read: `run() -> cap_read -> capability check -> Cairn ref/object lookup ->
  guest memory copy`
- write: `run() -> cap_write -> capability check -> guest memory copy -> Cairn
  object+commit -> Invocation record`

The write path is intentionally much heavier because Cairn v0 calls `sync_data`
for every appended record. That makes the measurement honest about persistence,
not just host-call overhead.

Measured on the development machine (`--release`, Windows/MSVC):

```text
read per call  ~= 0.119 us
write per call ~= 870.600 us
```

The read number is the hot guest-host path through a Cairn lookup. The write
number is dominated by durable append-only log syncs in Cairn v0.

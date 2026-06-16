# dezh-ir - Step 8: Dezh IR/WASM Runtime Contract

This crate makes Dezh's native execution boundary explicit. Dezh's long-term
native format is a typed, verifiable IR; in the current spikes that practical IR
is core WebAssembly.

The v0 contract is intentionally small:

- the only allowed import module is `dezh`
- WASI, `env`, clocks, filesystem, network, and other ambient imports are denied
- allowed imports are:
  - `cap_read(i32 handle, i32 out_ptr, i32 out_cap) -> i32`
  - `cap_write(i32 handle, i32 src_ptr, i32 src_len) -> i32`
  - `cap_print(i32 handle, i32 src_ptr, i32 src_len) -> i32`
  - `cap_attenuate(i32 handle, i32 requested_ops) -> i64`
- the module must export linear memory as `memory`
- the module must export `run() -> i64`
- compiled cache keys are BLAKE3 hashes of `contract_version || wasm_bytes`

## What This Validates

Step 8 turns the earlier runtime assumptions into a testable contract. A module
can be rejected before instantiation if it tries to import WASI, import from
`env`, use an unknown Dezh host function, use the wrong signature, omit exported
memory, or expose the wrong entrypoint.

## What This Does Not Validate Yet

This is not a full installer, AOT cache, component model, custom metadata parser,
or Dezh-specific IR. It is the first stable ABI fence that later phases can use.

Run the focused tests with:

```sh
cargo test -p dezh-ir
```

# Dezh — Architecture Spikes

Dezh is a from-scratch operating-system architecture explored through focused
spikes. Each spike validates one irreversible decision before the project moves
deeper into kernel, GUI, compatibility, server fabric, or agent runtime work.

Current status:

- **Step 1 complete** — capability core over WebAssembly: no ambient authority,
  unforgeable handles, attenuation, and green tests.
- **Step 2 complete** — Cairn v0: disk-persistent content-addressed immutable
  object store with refs, commits, rollback, replay, truncated-log recovery, and
  basic provenance metadata.
- **Step 3 complete** — identity/delegation/provenance: principals, attenuated
  delegation chains, invocation records, authority-used checks, and produced
  artifacts.
- **Step 4 complete** — runtime integration: WASM guests can read/write Cairn
  refs only through granted capability handles, and writes record invocation
  provenance.
- **Step 5 complete** — runtime boundary benchmark: measured guest-host
  read/write paths through Cairn and invocation recording.
- **Step 6 complete** — user-space actor IPC spike: message passing, attenuated
  capability transfer, panic isolation, and message-path benchmark.
- **Step 7 complete** — scheduler/task-placement spike: policy-driven placement
  using workload hints, data locality, queue pressure, energy, and NUMA costs.
- **Step 8 complete** — Dezh IR/WASM runtime contract: explicit module
  validation for allowed imports, required memory, `run() -> i64`, and
  content-addressed compiled-cache keys.
- **Step 9 complete** — Linux personality server spike: legacy-style paths map
  to Cairn refs through a capability-mediated virtual filesystem view.
- **Step 10 started** — kernel boot contract scaffold: no_std-compatible boot
  info, memory-map validation, init service plan, and explicit capability seeds.
- **Architecture control plane added** — [`docs/DECISIONS.md`](docs/DECISIONS.md)
  tracks which decisions are validated, accepted, hypotheses, deferred, or
  rejected.

This is **not** a bootable OS yet. It runs as ordinary host-process prototypes.

## Step 1 — capability core

> **No code runs with ambient authority.** Every program — including AI agents —
> starts with **zero** access to anything. It can act on a resource **only** if it
> has been handed an explicit, unforgeable, attenuable capability for that specific
> resource and operation.

This spike proves, in running code, that a WebAssembly guest can do **nothing**
except through capability handles granted by the host, and that capabilities can be
**attenuated** (narrowed) but, by construction, **never widened**.

WASI is disabled. A guest's *entire* import surface is four host functions
(`cap_read`, `cap_write`, `cap_print`, `cap_attenuate`) under the module `dezh`.
There is no filesystem, clock, network, or `env` — nothing ambient.

## How it works

- **Capability** = an unforgeable host-side handle: an opaque integer token into a
  *per-guest* capability table. The guest only ever holds the integer; it never sees
  a host pointer and cannot construct a valid token it was not handed. Each
  capability records a target `resource_id` and an allowed operation set
  (`READ` / `WRITE` / `PRINT`).
- **Enforcement point** — [`CapTable::check`](dezh-host/src/lib.rs): every
  capability-gated host function calls it first. Unknown/out-of-range handle →
  denied; handle that lacks the requested op → denied.
- **Attenuation** — [`Capability::derive`](dezh-host/src/lib.rs): the only
  capability-producing path reachable (via `cap_attenuate`) from a guest. The child's
  ops are `parent.ops ∩ requested`, so **no input can yield more authority than the
  parent**. There is deliberately no setter/`with_ops`/`&mut` accessor anywhere —
  that absence *is* the never-widen guarantee.
- **Resource backend** — a fake in-memory `resource_id -> Vec<u8>` table. No real I/O.

## Layout

```
Cargo.toml                 workspace root (members: dezh-cairn, dezh-host, dezh-identity, dezh-ir, dezh-ipc, dezh-linux, dezh-runtime, dezh-scheduler)
docs/DECISIONS.md          architecture decision register
dezh-cairn/
  src/lib.rs               Cairn v0 persistent object store (+ unit tests)
dezh-identity/
  src/lib.rs               principals + delegation + invocation provenance (+ unit tests)
dezh-ir/
  src/lib.rs               Dezh IR/WASM runtime contract validator (+ unit tests)
dezh-ipc/
  src/lib.rs               user-space actor IPC + capability transfer (+ tests)
dezh-kernel/
  src/lib.rs               no_std kernel boot contract scaffold (+ tests)
dezh-linux/
  src/lib.rs               Linux personality filesystem bridge (+ tests)
dezh-runtime/
  src/lib.rs               WASM capability host backed by Cairn + identity (+ tests)
dezh-scheduler/
  src/lib.rs               task placement scoring engine (+ tests)
dezh-host/
  src/lib.rs               capability core + host functions + resource backend (+ unit tests)
  src/main.rs              dezh-demo: runs all 3 guests, prints outcomes
  src/bin/cap_bench.rs     cap-bench: microbenchmark of the capability check
  build.rs                 compiles the guests to wasm, embeds them via OUT_DIR
  tests/capability_tests.rs end-to-end tests driving real wasm guests
guests/                    separate wasm workspace (no_std, no allocator, zero deps)
  g_granted/  g_denied/  g_attenuate/
```

## Step 1 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | No capability ⇒ read/write/print all denied | `no_capability_denies_everything` + `g_denied` | ✅ |
| 2 | A guest reaches ONLY its granted resource | `granted_guest_reads_only_its_resource` + `g_granted` | ✅ |
| 3 | Forged / guessed handles rejected | `forged_handles_are_rejected`, `check_out_of_range_*` | ✅ |
| 4 | Attenuation is strictly narrower; no API widens | `attenuation_narrows_and_never_widens` + `g_attenuate` + `derive_never_widens_across_entire_op_space` | ✅ |
| 5 | All covered by `cargo test` | 6 unit + 4 integration tests, all green | ✅ |
| 6 | README: decision, measured overhead, deferred list | this file | ✅ |

## Measured per-call capability-check overhead

The capability model adds one table lookup plus one permitted-ops test
(`CapTable::check`) in front of every host operation. Measured by `cap-bench`
(100,000,000 iterations, `--release`):

```
per check ≈ 0.96 ns   (≈ 96 ms / 100,000,000 checks)
```

Measured on the development machine (x86_64, Rust 1.87, MSVC toolchain). This is the
in-process cost of the check itself — the authority decision — not the wasm
boundary-crossing cost of an entire host call (which is dominated by wasmtime's call
trampoline, the same for any host function). At ~1 ns, the capability check is
effectively free relative to the work any real operation would do.

## Step 2 — Cairn v0

Cairn is Dezh's storage/object layer, not the whole OS. It validates the next
architectural claim from the roadmap: state should be built from immutable,
content-addressed objects plus small mutable refs, making rollback and crash
recovery structural properties.

The v0 API covers:

```rust
put(bytes) -> ObjectId
get(ObjectId) -> Option<Bytes>
begin_tx() -> Tx
tx.put(bytes) -> ObjectId
tx.set_ref(name, ObjectId)
tx.commit(principal, reason) -> CommitId
get_ref(name) -> Option<ObjectId>
history(name) -> Vec<CommitId>
rollback(name, CommitId, principal, reason) -> CommitId
```

Cairn v0 intentionally supports one ref movement per transaction. Multi-ref
atomic commits are deferred until the log record format grows a grouped commit
record.

### Cairn Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Duplicate content yields the same `ObjectId` | `duplicate_content_has_same_object_id` | ✅ |
| 2 | Old objects survive later ref updates | `old_objects_survive_ref_updates` | ✅ |
| 3 | Committed objects/refs replay after reopen | `transaction_put_commit_and_reopen_replays_state` | ✅ |
| 4 | Rollback moves a ref back and records provenance | `rollback_moves_ref_back_and_records_provenance` | ✅ |
| 5 | Multi-ref transactions are rejected instead of being half-atomic | `v0_rejects_multi_ref_transactions` | ✅ |
| 6 | Incomplete trailing log records are ignored | `truncated_tail_is_ignored_on_replay` | ✅ |

### What is explicitly deferred from Cairn v0

Schema system, encryption, compression, distributed sync, garbage collection,
semantic graph directories, and high-performance indexing.

## Step 3 — identity, delegation, provenance

The capability core answers **what authority exists**. Step 3 answers **who held
that authority, on whose behalf, how it was attenuated, and what action it
produced**.

The v0 model covers:

```rust
Principal::new(kind, name)
AuthorityGrant::root(principal, scope, authority)
grant.delegate(child, narrower_scope, narrower_authority, reason)
Invocation::record(grant, required_authority, action, reason, outputs)
```

### Step 3 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Delegation can only narrow authority | `delegation_can_only_narrow_authority` | ✅ |
| 2 | Delegation without `DELEGATE` cannot create sub-grants | `delegation_rejects_authority_widening` | ✅ |
| 3 | Delegation cannot widen scope | `delegation_rejects_scope_widening` | ✅ |
| 4 | Sub-agents inherit the full delegation chain | `sub_agent_gets_complete_delegation_chain` | ✅ |
| 5 | Invocation records actor, authority, outputs, and chain | `invocation_records_actor_authority_outputs_and_chain` | ✅ |
| 6 | Invocation fails if required authority is not held | `invocation_rejects_authority_not_held` | ✅ |

### What is explicitly deferred from Step 3

Cryptographic signatures, key storage, revocation, persistence, Cairn commit
integration, and capability transfer over IPC.

## Step 4 — runtime integration

Step 4 connects the validated parts:

```text
WASM guest -> capability handle -> Cairn ref/object -> Invocation provenance
```

The runtime exposes `cap_read` and `cap_write` under the `dezh` import module.
`cap_read` requires `READ_REF | READ_OBJECT`; `cap_write` requires `UPDATE_REF`.
A successful write creates a new Cairn object, moves the granted ref through a
Cairn commit, and records an invocation with produced object/commit artifacts.

### Step 4 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Guest reads a Cairn ref only with a granted capability | `guest_reads_cairn_ref_only_with_granted_capability` | ✅ |
| 2 | Guest write creates a Cairn commit and invocation | `guest_write_creates_cairn_commit_and_invocation` | ✅ |
| 3 | Read-only grants cannot write Cairn | `read_only_guest_cannot_write_cairn` | ✅ |
| 4 | Forged handles are rejected before Cairn access | `forged_handle_is_rejected_before_cairn_access` | ✅ |

### What is explicitly deferred from Step 4

General filesystem APIs, guest-driven delegation, persistent invocation logs,
rollback host calls, IPC, scheduler integration, and replacing the original Step
1 demo backend.

## Step 5 — runtime boundary benchmark

Step 5 measures the full Step 4 runtime boundary, not just the native
capability-table lookup:

- read: `run() -> cap_read -> capability check -> Cairn ref/object lookup ->
  guest memory copy`
- write: `run() -> cap_write -> capability check -> guest memory copy -> Cairn
  object+commit -> Invocation record`

Measured on the development machine (`--release`, Windows/MSVC):

```text
read per call  ~= 0.119 us
write per call ~= 870.600 us
```

The write path is much heavier because Cairn v0 calls `sync_data` for each
append-only log record. That is an honest persistence measurement, not a pure
host-call overhead number.

## Step 6 — actor IPC spike

Step 6 validates the microkernel/process direction before any kernel work:
state lives behind actors, messages are copied through channels, capabilities
transfer only by attenuating existing grants, and actor panics are isolated.

Measured on the development machine (`--release`, Windows/MSVC):

```text
actor message path ~= 0.081 us per 8-byte message
```

This is a user-space `std::sync::mpsc` path, not a future kernel IPC number.

### Step 6 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Actors exchange messages without shared state | `actors_exchange_messages_without_shared_state` | ✅ |
| 2 | Capability transfer is attenuated | `capability_transfer_is_attenuated` | ✅ |
| 3 | Transfer cannot widen authority | `transfer_cannot_widen_authority` | ✅ |
| 4 | Panicking actor is isolated | `panicking_actor_is_isolated` | ✅ |
| 5 | Message path is benchmarked | `ipc-bench` | ✅ |

### What is explicitly deferred from Step 6

Kernel IPC, async runtime, shared memory, priority inheritance, scheduler
policy, persistent mailboxes, and cross-process address spaces.

## Step 7 — scheduler/task-placement spike

Step 7 validates the scheduler direction from the architecture discussion:
scheduling is task placement, not just CPU time slicing. The v0 model chooses a
resource for a task using policy, workload hints, queue pressure, energy cost,
NUMA distance, and Cairn-style data locality.

Measured on the development machine (`--release`, Windows/MSVC):

```text
placement decision ~= 491.454 ns
resources          = 16
```

This is only the scoring engine. It is not task execution, kernel scheduling,
or GPU/NPU dispatch.

### Step 7 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Mobile tensor tasks prefer low-energy NPU | `mobile_tensor_prefers_low_energy_npu` | ✅ |
| 2 | Server batch tasks move compute toward data | `server_batch_prefers_compute_near_data` | ✅ |
| 3 | Latency-sensitive tasks avoid busy queues | `latency_sensitive_avoids_busy_queue` | ✅ |
| 4 | Batch policy tolerates queue depth for throughput | `batch_policy_tolerates_queue_for_throughput` | ✅ |
| 5 | Invalid resource metrics are rejected | `invalid_resource_is_rejected` | ✅ |
| 6 | Placement decision path is benchmarked | `scheduler-bench` | ✅ |

### What is explicitly deferred from Step 7

Real task execution, CPU affinity APIs, GPU/NPU drivers, hard realtime, cluster
scheduling, PGO database, and kernel integration.

## Step 8 — Dezh IR/WASM runtime contract

Step 8 turns Dezh's native execution boundary from an informal runtime habit
into a validation contract. The long-term native format is a typed, verifiable
IR; the practical v0 substrate is core WebAssembly with a Dezh-owned ABI fence.

The v0 contract requires:

- imports only from module `dezh`
- allowed imports: `cap_read`, `cap_write`, `cap_print`, `cap_attenuate`
- no WASI, `env`, filesystem, clock, network, or ambient imports
- exported linear memory named `memory`
- exported entrypoint `run() -> i64`
- BLAKE3 compiled-cache key over `contract_version || wasm_bytes`

### Step 8 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Current Dezh import signatures are accepted | `accepts_current_dezh_import_contract` | ✅ |
| 2 | WASI imports are rejected before instantiation | `rejects_wasi_imports` | ✅ |
| 3 | Unknown `dezh` imports are rejected | `rejects_unknown_dezh_imports` | ✅ |
| 4 | Wrong host-function signatures are rejected | `rejects_wrong_import_signature` | ✅ |
| 5 | Exported memory is required | `rejects_missing_memory_export` | ✅ |
| 6 | `run() -> i64` is required | `rejects_wrong_run_signature` | ✅ |
| 7 | Cache key is stable, versioned, and content-addressed | `cache_key_is_stable_versioned_and_content_addressed` | ✅ |

### What is explicitly deferred from Step 8

A full installer, AOT object cache, component-model ABI, custom-section parser,
Dezh-specific IR beyond WASM, runtime integration enforcement, and kernel-safe
hot-path extensions.

## Step 9 — Linux personality server spike

Step 9 validates compatibility as a bridge, not a destination. A legacy-facing
server can expose Linux-like filesystem syscalls while Dezh still owns the real
authority boundary.

The v0 surface covers:

- `open(path, flags) -> fd`
- `read(fd, out) -> bytes`
- `write(fd, bytes) -> WriteReceipt`
- `close(fd)`
- unsupported syscall numbers return `ENOSYS`

Guest paths such as `/home/app/readme.txt` are mounted onto Cairn refs such as
`refs/legacy/home/readme.txt`. Reads require `READ_REF | READ_OBJECT`; writes,
creates, and truncates require `UPDATE_REF`. Writes create new Cairn objects and
commits and record provenance invocations.

### Step 9 Definition of Done — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Legacy app reads only a mounted authorized ref | `legacy_app_reads_only_mounted_authorized_ref` | ✅ |
| 2 | Unmounted paths are invisible | `unmounted_path_is_not_visible` | ✅ |
| 3 | `..` escape is rejected before Cairn lookup | `path_escape_is_rejected_before_cairn_lookup` | ✅ |
| 4 | Read requires both ref and object authority | `read_requires_read_ref_and_read_object_authority` | ✅ |
| 5 | Write updates Cairn and records provenance | `write_updates_cairn_ref_and_records_invocation` | ✅ |
| 6 | Write requires update authority | `write_requires_update_ref_authority` | ✅ |
| 7 | Create works only inside mounted view | `create_is_allowed_only_inside_mounted_view` | ✅ |
| 8 | Unsupported syscalls are explicit `ENOSYS` | `unsupported_syscalls_are_explicitly_enosys` | ✅ |

### What is explicitly deferred from Step 9

ELF loading, real syscall-number dispatch, process address spaces, directories,
permissions bits, `stat`, `mmap`, pipes, sockets, signals, `fork`, async I/O,
and binary translation.

## Step 10 — minimal kernel boot contract scaffold

Step 10 has started, but it is **not complete** yet. The current crate defines a
`no_std`-compatible boot contract for the future QEMU-first kernel path:

- QEMU-first boot targets
- memory region validation
- init service launch plan
- user-space service specs for `init`, `cairn`, `wasm-runtime`, and `virtio-block`
- explicit capability seeds for each service
- rejection of capability seeds for unknown services as ambient authority

### Step 10 Current Milestone — status

| # | Property | Where it is proven | Status |
|---|----------|--------------------|--------|
| 1 | Minimal QEMU plan includes init, Cairn, runtime, and virtio-block services | `qemu_minimal_plan_launches_expected_user_space_services` | ✅ |
| 2 | Overlapping memory regions are rejected | `overlapping_memory_regions_are_rejected` | ✅ |
| 3 | Memory map must contain usable memory | `memory_map_requires_usable_memory` | ✅ |
| 4 | Every service capability must be seeded explicitly | `every_required_service_capability_must_be_seeded_explicitly` | ✅ |
| 5 | Capability seed for unknown service is rejected as ambient authority | `capability_seed_for_unknown_service_is_ambient_authority` | ✅ |
| 6 | Boot banner is stable and versioned | `boot_banner_is_stable_and_says_this_is_contract_v0` | ✅ |

### What must happen before Step 10 is complete

A real bootable target must exist and QEMU must run it far enough to print the
kernel contract banner. Until then D012 remains a hypothesis, not a validated
kernel boot.

### What is explicitly deferred from the current Step 10 scaffold

Linker script, entry assembly, page-table setup, interrupt handling, allocator,
serial driver, bootloader integration, QEMU run target, and launching actual
user-space service binaries.

## What is still explicitly deferred

Complete kernel boot, bootloader integration, device drivers beyond the current
virtio contract, full filesystem semantics, networking, GUI, Windows/Android/
macOS compatibility, distributed fabric, and the full agent runtime. These stay
out of scope until earlier validated decisions are connected.

## Building and running

Requires the Rust stable toolchain and the `wasm32-unknown-unknown` target.

```sh
cargo build              # builds the host and (via build.rs) the three wasm guests
cargo test               # runs Cairn + capability unit/integration tests
cargo test -p dezh-cairn # runs only Cairn v0 tests
cargo test -p dezh-host  # runs only Step 1 capability tests
cargo test -p dezh-identity # runs only Step 3 identity/provenance tests
cargo test -p dezh-ir # runs only Step 8 IR/WASM contract tests
cargo test -p dezh-ipc # runs only Step 6 actor IPC tests
cargo test -p dezh-kernel # runs only Step 10 boot contract scaffold tests
cargo test -p dezh-linux # runs only Step 9 Linux personality tests
cargo test -p dezh-runtime  # runs only Step 4 runtime integration tests
cargo test -p dezh-scheduler # runs only Step 7 scheduler placement tests
cargo run --release --bin cap-bench   # prints the measured check overhead
cargo run --release -p dezh-runtime --bin runtime-boundary-bench # full Step 4 boundary benchmark
cargo run --release -p dezh-ipc --bin ipc-bench # Step 6 actor message benchmark
cargo run --release -p dezh-scheduler --bin scheduler-bench # Step 7 placement benchmark
cargo run --release --bin dezh-demo   # human-readable end-to-end sanity pass
```

### Windows note

This machine's default Rust host is the `-gnu` toolchain, which lacks MinGW
`dlltool` (needed by a `wasmtime` transitive dependency). The project is pinned to the
MSVC toolchain via a `rustup override`; build from a shell with the MSVC environment
loaded (e.g. a *Developer PowerShell for VS 2022*, or after sourcing `vcvars64.bat`)
so `link.exe` is on `PATH`.

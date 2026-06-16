# Dezh Architecture Decision Register

This register keeps Dezh out of the "build everything forever" trap. Each
entry states whether an architectural choice is already validated, accepted as
direction, still a hypothesis, deferred, or rejected.

## Status key

- `validated`: proven by running code and tests.
- `accepted`: chosen as architectural direction, but not fully proven yet.
- `hypothesis`: important enough to test with a focused spike.
- `deferred`: intentionally out of scope until an earlier decision is proven.
- `rejected`: explicitly not a target for the current architecture.

## Decisions

| ID | Status | Decision | Rationale | Validation |
| --- | --- | --- | --- | --- |
| D001 | validated | No ambient authority via explicit capabilities. | This is Dezh's security foundation: no program, service, app, or agent starts with default access. | Step 1 capability core plus Step 4 runtime integration: wasm guests can only act through granted handles. |
| D002 | accepted | Rust is the trusted-core language. | Memory safety without GC matches the security and latency goals; Rust ownership also mirrors capability transfer. | Keep core crates Rust; revisit only if a kernel proof strategy requires another path. |
| D003 | validated | WASM-like typed IR is the first native execution substrate. | A typed, verifiable IR supports multi-ISA deployment, local optimization, and sandboxing. | Step 8 defines the Dezh IR/WASM v0 contract (allowed `dezh` imports, denied ambient imports, required memory, required `run() -> i64`, content-addressed compiled-cache keys). The contract is **enforced on the execution path**: `dezh-runtime` validates every guest against its declared host surface (`HOST_SURFACE`) before instantiation, so a module with WASI/ambient or unoffered imports is rejected at the gate. |
| D004 | validated | Cairn is the content-addressed immutable object store. | Rollbackable agent actions, crash consistency, deduplication, and provenance all depend on immutable content-addressed objects. | Step 2 Cairn v0: persistent append-only store with refs, commits, rollback, and recovery. |
| D005 | validated | Agent actions must be rollbackable. | The main safety promise for autonomous agents is that important state changes are transactional and reversible. | Cairn proves object rollback; Step 4 shows guest writes become Cairn commits through capability checks. |
| D006 | validated | Provenance is first-class metadata. | Dezh must answer who acted, on whose behalf, with which authority, and what object or commit resulted. | Step 3 identity model records principals/delegation; Step 4 records invocations for guest-produced Cairn objects and commits. |
| D007 | validated | Compatibility is a bridge, not the destination. | Legacy app support should help migration without trapping Dezh in old APIs forever. | Step 9 validates a user-space Linux personality filesystem view: legacy paths map to Cairn refs, all access is capability-mediated, writes produce Cairn commits and provenance, and unsupported syscalls return `ENOSYS`. |
| D008 | validated | The final OS shape is microkernel-based. | User-space drivers/services improve isolation, restartability, and capability mediation. | Step 6 validates the user-space actor/message model, capability transfer discipline, panic isolation, and message-path benchmark before kernel work. |
| D009 | validated | Scheduling is task placement, not only thread time slicing. | Dezh must span mobile, desktop, server, accelerators, and eventually clusters. | Step 7 validates policy-driven placement using workload hints, data locality, queue pressure, energy, and NUMA penalties. |
| D010 | accepted | GUI access is mediated by compositor capabilities. | Apps must not read global input, screenshots, clipboard, or other surfaces by default. | GUI spike after core runtime integration. |
| D011 | validated | Linux compatibility comes before Windows and Android; macOS is not v1. | Linux ABI is the most stable first bridge; macOS frameworks and policy make it unrealistic for v1. | Step 9 starts with a Linux personality server spike before any Windows, Android, or macOS compatibility work. |
| D012 | validated | Kernel boot is QEMU-first with user-space services seeded by explicit capabilities. | Boot work should start on a narrow virtio/QEMU surface and preserve the capability model from the first instruction after init. | Step 10 boots for real: `dezh-boot` is a `no_std` RISC-V kernel that comes up in S-mode on QEMU `virt` (via OpenSBI), runs the boot description through the validated `dezh-kernel` contract, prints the banner + init service plan over UART, and exits cleanly. Crosses the simulation → bare-metal boundary. |

## Current phase

Step 1 through Step 10 are validated. Step 10 boots for real: `dezh-boot` comes
up on bare-metal QEMU `virt` (RISC-V), prints the validated kernel contract
banner, installs an S-mode trap vector + SBI timer (background uptime), and runs
**Dezh's own capability-gated console** over the UART — an interactive REPL where
each command requires an explicit capability and an ungranted command is denied
(no-ambient-authority, now interactive on bare metal). Next: launch a first
capability-seeded user-space task (drop to U-mode, service its `ecall` as a
capability-checked request), keeping every step under the thesis.

## Canonical authority model

`dezh-identity::Authority` (with `AuthorityGrant` / delegation chains) is the
**canonical** authority vocabulary; the live crates (`dezh-runtime`, `dezh-ipc`,
`dezh-linux`) all build on it. `dezh-host::Ops` is the **Step 1 proof
vocabulary** — it validated the unforgeable-handle + attenuation *mechanism* and
is intentionally not reused downstream. New work standardizes on `Authority`; a
shared `cap-core` crate is deliberately deferred until a second live vocabulary
exists (avoid premature abstraction).

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
| D008 | validated | The final OS shape is microkernel-based. | User-space drivers/services improve isolation, restartability, and capability mediation. | Step 6 validates the user-space actor/message model, capability transfer discipline, panic isolation, and message-path benchmark. On the bare-metal kernel (`dezh-boot`, `ipc`), capability-passing IPC is now the core mechanism: a task delegates a capability to another via a message, the kernel enforcing attenuation (a sender can only grant authority it holds). This keeps the kernel minimal — Cairn and Pol remain user-space *services* reached over IPC, not kernel code — and is the foundation for agents calling services and spawning attenuated sub-agents (D013). Zero-copy object handoff (D018) is a planned optimization on top. |
| D009 | validated | Scheduling is task placement, not only thread time slicing. | Dezh must span mobile, desktop, server, accelerators, and eventually clusters. | Step 7 validates policy-driven placement using workload hints, data locality, queue pressure, energy, and NUMA penalties. |
| D010 | accepted | GUI access is mediated by compositor capabilities. | Apps must not read global input, screenshots, clipboard, or other surfaces by default. | GUI spike after core runtime integration. |
| D011 | validated | Linux compatibility comes before Windows and Android; macOS is not v1. | Linux ABI is the most stable first bridge; macOS frameworks and policy make it unrealistic for v1. | Step 9 starts with a Linux personality server spike before any Windows, Android, or macOS compatibility work. |
| D012 | validated | Kernel boot is QEMU-first with user-space services seeded by explicit capabilities. | Boot work should start on a narrow virtio/QEMU surface and preserve the capability model from the first instruction after init. | Step 10 boots for real: `dezh-boot` is a `no_std` RISC-V kernel that comes up in S-mode on QEMU `virt` (via OpenSBI), runs the boot description through the validated `dezh-kernel` contract, prints the banner + init service plan over UART, and exits cleanly. Crosses the simulation → bare-metal boundary. |
| D013 | accepted | Dezh is an agent-first OS: AI agents are first-class principals, not bolt-ons. | An agent OS must make agent actions capability-bound, rollbackable, and provenance-tracked *by construction* — which is exactly what Dezh's core provides, so agents are a primary target, not an afterthought. | Partially backed already: identity principals/delegation (Step 3), guest actions → Cairn commits + invocation provenance (Step 4), Cairn rollback (Step 2). A dedicated agent runtime layer is deferred (name TBD, to be approved). |
| D014 | accepted | Legacy app compatibility is delivered by **Pol** — capability-mediated personality servers. | Each legacy app runs inside a capability sandbox; its syscalls are translated and gated, so Linux/Android/Windows apps get **zero ambient authority by construction** (compatibility as a security upgrade, not a hole). Order: Linux → Android → Windows; macOS not v1. Performance target: near-native compute, with minimized — not zero — syscall-translation overhead. | First Pol personality (Linux) spiked in user space at Step 9 (`dezh-linux`): legacy paths → Cairn refs, capability-mediated, writes record provenance, unsupported syscalls return `ENOSYS`. A minimal Pol/Linux personality now also runs **on the bare-metal kernel** (`dezh-boot`, `linux` command): a U-mode app speaking the real Linux riscv64 syscall ABI (`write`=64, `exit`=93) is serviced through capability checks, with unsupported syscalls returning `ENOSYS` — first legacy compatibility on the kernel itself. Android and Windows personalities not started. Refines D007 (bridge) and D011 (order). |
| D015 | accepted | Performance is delivered by architecture and proven by measurement against baselines. | "Faster than existing OSes" is meaningless as a bare assertion. Every performance claim must trace to a specific architectural lever *and* a benchmark against a real baseline (Linux/Windows) on the same workload. The microkernel's IPC cost is a real thing to engineer around (seL4-style fast paths + D018 zero-copy), not to assume away. We do not chase novelty that breaks the thesis or never ships. | First comparative benchmark done (`dezh-boot/BENCH.md`): on the same real CPU, Dezh's capability check (~0.98 ns) is ~50× cheaper than Linux's syscall floor (`getpid` ~49 ns) — the real-hardware basis for capability-mediated access over per-access syscalls. The kernel `ecall` round trip (~1041 ns) is QEMU-emulated and explicitly *not* compared to native. End-to-end same-substrate benchmarks vs Linux are the next milestone. |
| D016 | accepted | One program runs across ISAs via the typed IR. | Compiling to a verifiable typed IR instead of fixed machine code lets the same program target RISC-V, x86, and ARM and be optimized per host — portability and sandboxing together, without ambient authority. Order: RISC-V first (the kernel today), then x86/ARM. | IR contract validated and enforced in the runtime (Step 8 / D003). The bare-metal kernel runs on RISC-V only so far. Sharpens D003. |
| D017 | hypothesis | Heterogeneous execution (CPU efficiency/performance cores + GPU/NPU) under the capability thesis, with IOMMU-enforced device isolation. | Dezh must place work on the best core type (D009) AND preserve no-ambient-authority on accelerators. Accelerators use DMA = ambient memory access, so an IOMMU (device-side address translation) is required so a device can only touch memory it was explicitly granted — the hardware memory-boundary thesis (proven for U-mode via Sv39) extended to devices. | Placement scoring validated in user space (Step 7: energy/NUMA/locality/workload hints). No real GPU/NPU execution or IOMMU integration yet. |
| D018 | accepted | Cross-domain data sharing is zero-copy via content-addressed capabilities. | Instead of copying bytes between protection domains (the classic IPC cost), pass an unforgeable capability to an immutable content-addressed object; the receiver reads it in place. This preserves the thesis (no ambient access — only the handed capability) and offsets microkernel IPC overhead, supporting D015. | Cairn provides immutable content-addressed objects (Step 2); the runtime already passes ref capabilities to guests (Step 4). A zero-copy shared-object path on the bare-metal kernel is future work. |

## Current phase

Step 1 through Step 10 are validated. Step 10 boots for real: `dezh-boot` comes
up on bare-metal QEMU `virt` (RISC-V), prints the validated kernel contract
banner, installs an S-mode trap vector + SBI timer (background uptime), runs **Dezh's own
capability-gated console** over the UART, and from the console `run` drops a task
to **U-mode** (zero ambient authority) that can only reach the kernel via
`ecall`s checked against the *task's* capabilities — an ungranted syscall
(`sys_uptime`) is denied at the kernel boundary, then a real S→U→S context switch
returns to the console. **Sv39 paging** then confines each U-mode task to its own
region: kernel + MMIO pages are supervisor-only, so a task touching the UART
directly (`rogue`) takes a page fault and is killed while the console survives.
The no-ambient-authority thesis is now enforced at **both the syscall boundary
and the hardware memory boundary**, not just by Rust types. Next: multiple tasks
with scheduling and per-task regions, then the first real Pol personality (Linux)
on the kernel.

## Names (ours — user-approved)

Dezh's proper nouns are deliberate and **require the user's approval** before
use; proposed names are not adopted until approved.

- **Dezh** — the OS itself (دژ, "fortress/citadel": security by construction).
- **Cairn** — the content-addressed immutable object store (D004).
- **Pol** — the legacy-compatibility subsystem: capability-mediated personality
  servers (پل, "bridge"; D014). `dezh-linux` is the first Pol personality.

The agent-runtime layer (D013) is intentionally **unnamed for now**.

## Canonical authority model

`dezh-identity::Authority` (with `AuthorityGrant` / delegation chains) is the
**canonical** authority vocabulary; the live crates (`dezh-runtime`, `dezh-ipc`,
`dezh-linux`) all build on it. `dezh-host::Ops` is the **Step 1 proof
vocabulary** — it validated the unforgeable-handle + attenuation *mechanism* and
is intentionally not reused downstream. New work standardizes on `Authority`; a
shared `cap-core` crate is deliberately deferred until a second live vocabulary
exists (avoid premature abstraction).

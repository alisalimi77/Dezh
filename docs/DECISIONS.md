# Dezh Architecture Decision Register

This register records the architectural decisions behind the current prototype.
Each decision is either validated by running code, accepted as direction,
hypothesis, deferred, or rejected.

## Status Key

- `validated`: proven by code, tests, or QEMU demo output.
- `accepted`: chosen direction, not fully proven yet.
- `hypothesis`: important enough to test next.
- `deferred`: intentionally out of scope for the current prototype.
- `rejected`: not a target for this architecture.

## Decisions

| ID | Status | Decision | Rationale | Validation |
| --- | --- | --- | --- | --- |
| D001 | validated | No ambient authority via explicit capabilities. | Programs, services, drivers, and apps should start with no default access. | Host capability tests, runtime integration, and bare-metal syscall checks. |
| D002 | accepted | Rust is the trusted-core implementation language. | Memory safety without a garbage collector fits the kernel and service goals. | Core crates and bare-metal kernels are Rust. |
| D003 | validated | A typed, verifiable execution contract is the first portable app/agent substrate. | A small typed surface supports validation and multi-ISA direction. | `dezh-ir` and `dezh-runtime` validate imports, memory, and entry contracts. |
| D004 | validated | Cairn-style storage is content-addressed and rollback-oriented. | Immutable objects plus refs make recovery and rollback structural. | `dezh-cairn` tests and bare-metal current/previous sector flow. |
| D005 | validated | Important app and agent state changes should be rollbackable. | Recovery must be a first-class state property, not an afterthought. | Cairn tests and bare-metal rollback command. |
| D006 | validated | Provenance metadata is first-class. | Reviewable systems need to know actor, authority, action, and output. | Identity and runtime tests record delegation and invocation metadata. |
| D007 | validated | Compatibility should be a bridge, not the security baseline. | Legacy-style interfaces should map into capability-checked services. | Linux personality spike and bare-metal unsupported-syscall denial. |
| D008 | validated | The OS shape is microkernel-based. | Drivers and stateful services should be isolated and restartable. | User-space IPC spike and bare-metal user-space virtio-block daemon. |
| D009 | validated | Scheduling includes isolation and placement direction. | Runtime policy needs both task switching and future placement decisions. | Scheduler policy crate plus bare-metal preemptive round-robin demo. |
| D010 | accepted | GUI access will be mediated by compositor capabilities. | Apps should not receive global input, clipboard, screenshot, or surface access by default. | Deferred until GUI work starts. |
| D011 | validated | Linux-style compatibility is the first legacy personality path. | It is a practical first bridge with a clear syscall surface. | `dezh-linux` and bare-metal Linux ABI demo. |
| D012 | validated | Kernel boot is QEMU-first with explicit service capability seeds. | A narrow hardware surface keeps authority boundaries reviewable. | RISC-V QEMU boot contract and service registry. |
| D013 | accepted | Agent execution is a primary target. | Autonomous software needs capability-bound, rollbackable, provenance-aware effects. | Identity, runtime, Cairn, and IR layers partially validate the direction. |
| D014 | accepted | Legacy compatibility is delivered through capability-mediated personality services. | Compatibility should not reintroduce ambient authority. | Linux personality prototype; additional personalities are deferred. |
| D015 | accepted | Performance claims must be evidence-backed. | Architectural claims need measured support and clear baselines. | Existing microbenchmarks and QEMU benchmark suite; broader comparisons deferred. |
| D016 | accepted | One program should eventually run across ISAs through the typed contract. | Portability and sandboxing should share one validation path. | IR contract validated; bare-metal x86_64 smoke validates a second path. |
| D017 | hypothesis | Accelerator and DMA access require IOMMU-backed grants. | DMA can bypass CPU page tables unless device-side translation enforces grants. | Current DMA discipline is modeled; real IOMMU work is deferred. |
| D018 | accepted | Cross-domain data sharing should move toward zero-copy object capabilities. | Immutable object capabilities can reduce copy cost without broadening authority. | Cairn provides immutable objects; bare-metal zero-copy path is future work. |
| D019 | accepted | MVP scope is: installable, programmable, and four demonstrable differentiators. | External review requires independence (bootable VM images), an SDK (out-of-tree apps with capability manifests), and reproducible flagship demos for agent containment, Cairn rollback storage, multi-ISA app portability, and Pol foreign-binary execution — each with D015-compliant claim wording. Outreach waits until all four demos are green in CI. | Roadmap workstreams W1–W7; not yet validated. |
| D020 | accepted | Dezh is framed as intent-native and effect-accountable, bound to the MVP. | Intent must be the only authority-derivation path (derived capability narrower than or equal to the declared intent, enforced not annotated); effects are ledger records carrying authority provenance and a reversibility class; ledger/denial state lives in user-space services on Cairn, never the kernel. This narrative extends the D019 demos (F1/W2/W3) rather than forking the roadmap; namespace migration is post-MVP. | `docs/STRATEGIC_DIRECTION.md`; not yet validated. |

## Current Bare-Metal State

The RISC-V kernel boots in QEMU, validates its boot contract, runs a
capability-gated console, launches isolated U-mode ELF processes, and uses Sv39
to deny access outside each process grant.

The current storage path runs through a user-space `virtio-block` daemon. The
daemon receives explicit MMIO and DMA grants; clients communicate with it over
typed IPC. The service registry supports start, stop, controlled fault, and
explicit restart.

The app registry v0 supports embedded app bundles, install/remove state,
private app storage sectors, and no-grant denial demos. This is sufficient for
reviewing the authority model, not a production package ecosystem.

## Naming Policy

Public documentation uses the name `Dezh OS` only as a project label. Public
review material does not include origin stories, location claims, or personal
identity details. The review package is intentionally neutral and technical.

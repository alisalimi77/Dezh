# Dezh Architecture

Dezh is a bare-metal OS research prototype focused on explicit authority,
service-mediated effects, and recoverable lifecycle operations.

For visual diagrams, see [ARCHITECTURE_DIAGRAMS.md](ARCHITECTURE_DIAGRAMS.md).

## Design Center

The current prototype is built around four rules:

1. No ambient authority by default.
2. Device and storage access are service-mediated.
3. Persistent lifecycle changes are transactional and recoverable.
4. Runtime state should be inspectable by reviewers from the console and tests.

The strategic direction is to make **intent** and **effect** first-class OS
concepts. The current implementation is not fully intent-native yet, but the
package, service, IPC, and storage work is deliberately moving toward that
shape.

## Boot Flow

1. OpenSBI starts the RISC-V kernel in S-mode on QEMU `virt`.
2. The kernel validates the boot contract from `dezh-kernel`.
3. The kernel installs trap handling, timer support, and Sv39 paging.
4. A capability-scoped console starts over UART.
5. Services are declared from the boot plan and materialized in the service
   registry.
6. Long-lived services such as `virtio-block` are started explicitly or lazily
   from the registry.

## Kernel Responsibilities

The kernel owns the confinement boundary:

- address-space construction
- trap and syscall handling
- task scheduling
- IPC queues and typed receive timeout support
- service registry state
- explicit process launch grants
- frame ownership and reclaim
- fault containment for U-mode tasks

The kernel does not implement the current block I/O path directly.

## Process Model

Each ELF process receives:

- its own address space
- entry point and initial arguments
- task capabilities
- optional device mappings
- optional DMA mappings
- tracked frame ownership for reclamation

Foreground clients are reclaimed after exit or fault. Daemons remain alive until
they stop, fault, or are explicitly restarted.

## Capability Model

Task capability bits currently cover:

- print
- time
- IPC
- virtio-block device
- block read
- block write
- Cairn namespaces 0..7 (bits 8..15): one bit per named storage namespace

The important property is attenuation: a task can only transfer capabilities it
already holds. Manifest-declared package capabilities are separately translated
into runtime grants; a manifest `cairn-read`/`cairn-write` grant maps to the
app's **own** namespace bit only (matched by app name) — a manifest can never
name another app's namespace.

## IPC

The base IPC syscall sends a small payload, a scalar word, and an attenuated
capability grant. Service paths pack a typed v0 envelope into the scalar word:

```text
proto | service_id | op | request_id | status | arg
```

Storage, installer, app, and package paths use typed replies. Legacy demos can
still use raw scalar messages.

**Kernel-attested sender capabilities:** on every send, the kernel records the
sender's capability set in the message; on receive, the service gets that set
alongside the payload. A service therefore checks the *sender's* authority
against values a client cannot forge from user space. This is how the storage
daemon enforces per-namespace access, and why its denials can name the exact
missing capability (`why-denied` direction from the strategic plan).

## User-Space Block Driver

The `virtio-block` daemon is a separate U-mode ELF. It alone receives:

- the virtio MMIO page grant
- the DMA window grant
- IPC authority
- block read/write authority

Foreground clients do not receive MMIO authority. A no-grant process touching
the MMIO address faults and is killed without killing the console.

The daemon handles:

- disk probe
- block write/read
- root install marker and metadata
- Cairn v0 current/previous value operations (legacy demo path)
- Cairn v1 commit-log store with per-namespace capability checks
- embedded app registry operations
- package registry, journal, and blob sectors
- note/lab/calc/vault private storage
- stop and controlled fault demo

## Cairn v1 (Commit-Log Store)

Cairn v1 lives inside the storage daemon on sectors 1600..1855:

- a superblock holding the namespace table (`note`, `lab`, `calc`, `vault`,
  `agent`) with each namespace's head ref and commit count;
- append-only commit records, each carrying: parent ref, FNV-1a hash of the
  value object, actor task id, a reversibility flag, and the inline value.

Semantics:

- **Commit** appends a record and moves the namespace head ref.
- **Rollback N** walks the parent chain and moves the ref back; history is
  never erased, and the state survives reboot.
- **Verify** re-hashes the head object against its commit record.
- **Access** requires the namespace's capability bit, checked against the
  kernel-attested sender capability set; denials name the missing capability.

The commit record fields (actor, reversibility class, provenance chain) are
the seed of the effect ledger described in
[STRATEGIC_DIRECTION.md](STRATEGIC_DIRECTION.md) (decision D020).

Dezh-IR apps reach the store through the kernel's IR host, which routes
`cairn_put`/`cairn_get` host calls over typed IPC to the daemon with the app's
own namespace capability — there is no kernel-side block I/O shortcut.

## Service Registry

The service registry tracks:

- service name
- service kind
- state
- task id
- caps
- grants
- restart count
- last exit
- last started tick
- fault reason

Manual stop and controlled fault are not hidden by automatic restart. Review
commands use explicit `svc-restart` so service recovery remains visible and
deterministic.

## Package Store

The SDK builds `.dzp` packages. The OS stores them through the user-space block
service, not through a kernel block path.

Current package features:

- persistent registry on disk
- transaction journal
- active, previous, and stage blob areas
- install/remove/update/rollback
- recovery and quarantine
- pin/unpin
- cap-escalation review
- explicit physical cleanup through `pkg-gc run`

Only `Active` packages are runnable. `Removed`, `Corrupt`, `Pending*`, and
`Quarantined` packages do not run.

## Embedded Apps

The current embedded app set is intentionally mixed:

- `note`: persistent text app
- `lab`: UI-like multi-task app with cooperating workers
- `calc`: calculator app with stored last result
- `vault`: private-value app used to exercise storage and device-denial paths

These are review demos, not a production app ecosystem.

## Storage Path

The storage path is:

```text
console command -> foreground client -> typed IPC -> virtio-block daemon
               -> granted MMIO/DMA -> disk image
```

This path is central to the project. It proves that storage does not silently
fall back to a kernel block driver.

## Review Surface

Useful review commands:

- `services`
- `tasks`
- `ipcstat`
- `ipc-typed-demo`
- `pkg-store`
- `pkg-journal`
- `pkg-review <name>`
- `pkg-versions <name>`
- `pkg-gc`
- `cairn-demo` / `cairn-log <ns>` / `cairn-rollback <ns> [n]` / `cairn-verify <ns>`
- `agent`
- `bench-all`

Useful review tools:

- `tools/ci/qemu_smoke.py`
- `tools/ci/sdk_test.py`
- `tools/review/scan_public.py`
- `tools/demo/run_review_demo.py`
- `tools/demo/run_agent_demo.py` (F1 agent-containment transcript)

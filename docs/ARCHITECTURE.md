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

The important property is attenuation: a task can only transfer capabilities it
already holds. Manifest-declared package capabilities are separately translated
into runtime grants.

## IPC

The base IPC syscall sends a small payload, a scalar word, and an attenuated
capability grant. Service paths pack a typed v0 envelope into the scalar word:

```text
proto | service_id | op | request_id | status | arg
```

Storage, installer, app, and package paths use typed replies. Legacy demos can
still use raw scalar messages.

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
- Cairn-style current/previous value operations
- embedded app registry operations
- package registry, journal, and blob sectors
- note/lab/calc/vault private storage
- stop and controlled fault demo

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
- `bench-all`

Useful review tools:

- `tools/ci/qemu_smoke.py`
- `tools/ci/sdk_test.py`
- `tools/review/scan_public.py`
- `tools/demo/run_review_demo.py`

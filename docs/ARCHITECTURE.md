# Dezh OS Architecture v0

## Boot Flow

1. OpenSBI starts the RISC-V kernel in S-mode on QEMU `virt`.
2. The kernel validates the boot contract from `dezh-kernel`.
3. The kernel installs trap handling, timer support, and Sv39 paging.
4. A capability-scoped console starts over UART.
5. Services are declared from the boot plan and materialized in the runtime
   service registry.

## Kernel Responsibilities

The kernel is intentionally small in scope:

- page-table and address-space setup
- trap and syscall handling
- task scheduling
- IPC queues and typed receive timeout support
- service registry state
- explicit process launch grants
- fault containment for U-mode tasks

The kernel does not perform the current block I/O path directly.

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

Task capability bits currently include:

- print
- time
- IPC
- virtio-block device
- block read
- block write

The important property is attenuation: a task can only transfer capabilities it
already holds.

## IPC

The base IPC syscall sends a small payload, a scalar word, and an attenuated
capability grant. The typed service path packs a v0 envelope into the scalar
word:

```text
proto | service_id | op | request_id | status | arg
```

Existing demos can still use raw scalar messages. Storage, installer, and app
service paths use typed replies.

## User-Space Block Driver

The `virtio-block` daemon is a separate U-mode ELF. It alone receives the
virtio-mmio page and DMA window grants. Clients do not receive MMIO access. A
no-grant process touching the MMIO address faults and is killed without killing
the console.

The daemon handles:

- disk probe
- block write/read
- root install marker and metadata
- Cairn-style current/previous value operations
- app registry operations
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

The `virtio-block` service is intentionally not auto-restarted after manual stop
or fault. `svc-restart virtio-block` is explicit to keep review behavior
deterministic.

## App Registry

The v0 app registry is disk-backed and service-mediated. App commands resolve
the registered block service, launch foreground clients, and rely on typed IPC
status. The app binaries are embedded in the image for this prototype.

The current embedded app set is intentionally mixed:

- `note`: a small persistent text app.
- `lab`: a UI-like multi-task app that launches cooperating workers and writes
  completion state through the service path.
- `calc`: an installed calculator app that computes in U-mode and stores the
  last result through its private registry sector.
- `vault`: a private-value app used to exercise app storage and direct-device
  denial.

The console exposes review-oriented control-surface commands such as `install
run`, `apps installed`, `app-permissions`, `events`, `audit`, `calc`, and
`vault-get` so reviewers can inspect state transitions instead of reading only
boot logs.

## Storage Path

The storage path is:

```text
console command -> foreground client -> typed IPC -> virtio-block daemon
               -> granted MMIO/DMA -> disk image
```

This is the core review path for proving that storage does not silently fall
back to a kernel block driver.

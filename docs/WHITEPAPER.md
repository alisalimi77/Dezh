# Dezh OS Whitepaper v0

## Abstract

Dezh OS is a capability-secure operating-system research prototype. It explores
a strict baseline: tasks, apps, services, and drivers do not inherit ambient
authority. Every effect is mediated by an explicit capability, mapping, device
grant, DMA grant, or IPC permission.

The current RISC-V prototype boots on QEMU, runs isolated U-mode processes,
starts a user-space virtio-block driver, persists a root/install marker on a
real disk image, installs small apps through a service-mediated registry, and
validates typed IPC plus service supervision.

## Problem Statement

General-purpose systems often make compatibility and convenience the default
authority model. A process may inherit broad filesystem access, devices may
remain kernel-resident, and service contracts may rely on conventions rather
than explicit authority. These choices make isolation, recovery, and audit
harder.

Dezh tests the opposite default:

```text
No operation is authorized unless the caller holds the authority for that exact
operation and target.
```

## Security Thesis

Dezh separates four concerns:

- The kernel enforces privilege boundaries, address spaces, syscalls, IPC, and
  service lifecycle.
- Drivers and storage logic run as user-space services.
- Apps receive only the capabilities declared by their install/runtime contract.
- Persistent state changes flow through service-mediated paths that can be
  audited and rolled back at v0 granularity.

This is intended to reduce ambient access, contain faults, and make service
dependencies explicit.

## Current Architecture

The RISC-V kernel validates a boot plan, initializes Sv39 paging, and launches a
capability-gated console. Processes run in U-mode with task capabilities such as
print, time, IPC, device, block read, and block write.

The virtio-block device is not driven through a hidden kernel path. Instead, a
separate U-mode ELF receives:

- the virtio-mmio page grant
- a fixed DMA window
- IPC authority
- block read/write authority

Foreground clients and app/storage commands communicate with that daemon over
typed IPC.

## Typed IPC v0

Typed IPC v0 keeps the existing syscall shape and adds a structured scalar
envelope for service paths:

- protocol version
- service id
- operation
- request id
- status
- argument

The important statuses are:

- `OK`
- `DENIED`
- `UNAVAILABLE`
- `TIMEOUT`
- `BAD_REQUEST`
- `IO_FAILURE`
- `FAULTED`
- `BUSY`

The kernel also exposes receive-with-timeout support and IPC counters for
reviewability.

## Installer And App Registry v0

The current disk layout reserves sectors for:

- install marker
- root metadata
- current and previous durable value sectors
- app registry sectors
- private app data sectors

The `note` and `lab` apps are embedded bundles for v0. They are installed
transactionally into the registry and run without direct device, DMA, MMIO, or
block grants.

## Service Supervision

The service registry tracks declared services and runtime state. The
`virtio-block` service can be:

- started from the boot plan
- stopped explicitly
- faulted through a controlled demo
- restarted explicitly

When stopped or faulted, storage commands fail cleanly instead of hanging. The
console survives.

## Evidence

The review smoke and demo cover:

- boot contract validation
- typed IPC success, bad request, timeout, and denial
- service registry and daemon startup
- app install and run
- app storage through the user-space block daemon
- no-grant MMIO denial
- service stop/restart/fault/recovery
- benchmark and capability-denial suite

## Limitations

- QEMU RISC-V is the main bare-metal target today.
- virtio modern, interrupt-driven block I/O, and real IOMMU isolation are future
  work.
- The v0 app registry uses deterministic markers, not production signing.
- App bundles are embedded, not downloaded or dependency-resolved.
- The installer is a disk-layout initializer, not a full boot-media installer.
- The current benchmark suite is useful for regression and validation, not a
  complete performance comparison against production systems.

## Review Request

The requested review areas are:

- capability model clarity
- service and IPC contract shape
- driver isolation and DMA grant discipline
- installer/app registry direction
- fault handling and recovery behavior
- missing threat-model assumptions

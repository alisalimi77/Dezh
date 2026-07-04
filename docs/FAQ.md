# FAQ

## Is Dezh production-ready?

No. Dezh is a working research prototype intended for architectural review. It
boots, runs isolated tasks, uses a user-space block driver, validates typed IPC,
and exercises a transactional package lifecycle in QEMU, but it is not a
production OS.

## Why publish it now?

The project is at the point where its core thesis can be inspected through
code, QEMU transcripts, and repeatable tests. Public review is useful before
the design becomes too large to change.

## What is the main technical thesis?

Dezh explores intent-scoped authority and effect accountability. A program
should receive the narrow authority needed for a specific effect, and important
state changes should be visible, recoverable, and tied to an explicit service
route or transaction.

## Is Dezh a Unix clone?

No. Dezh intentionally avoids starting from ambient files, ambient devices,
ambient process inheritance, or a global package registry. Compatibility layers
may exist later, but they should not define the core authority model.

## Is Dezh a microkernel?

It shares some microkernel instincts, especially user-space drivers and service
boundaries, but the current goal is not to fit a label. The important boundary
is explicit authority: kernel code should enforce isolation and routing, while
device and storage effects should be delegated through granted user-space
services.

## Why user-space virtio-block?

The block device is a useful proof point: persistent storage should not require
a hidden kernel I/O path. The current `virtio-block` daemon runs in U-mode and
receives explicit MMIO and DMA grants.

## How are apps installed?

The SDK builds `.dzp` packages. The package store writes registry, journal, and
blob sectors through the registered user-space block service. Only `Active`
packages are runnable.

## What prevents half-installed apps?

Package install/remove uses a journaled state machine. Interrupted installs are
rolled back, committed only when checks match, quarantined if suspicious, or
blocked when the journal is corrupt.

## What should reviewers focus on first?

The highest-value review areas are:

- capability boundaries
- user-space driver grants
- typed IPC status handling
- service stop/fault/restart semantics
- package journal recovery
- package capability escalation review
- denial proofs and failure behavior

## What is intentionally out of scope right now?

- production bootloader and installer media
- production package signing
- dependency solving
- real IOMMU integration
- production networking
- graphics stack
- real hardware bring-up
- formal verification of the whole system

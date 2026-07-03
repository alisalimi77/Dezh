# Dezh OS Overview

Dezh OS is a capability-secure operating-system research prototype. It tests a
microkernel-shaped design where programs, apps, services, and drivers receive no
default authority. Authority is granted explicitly through capabilities,
address-space mappings, IPC permissions, device grants, and DMA windows.

## Why This Exists

Modern systems still carry many broad authority paths: inherited process
authority, global filesystem assumptions, kernel-resident drivers, and service
interfaces that blur ownership. Dezh explores a stricter baseline:

```text
No authority exists unless the boot plan, service registry, or caller grants it.
```

This rule is enforced in the current prototype at several layers:

- syscall capability checks
- U-mode page-table isolation
- explicit device and DMA mappings
- capability-gated IPC
- service-mediated storage
- app registry validation

## Current Demonstration

The RISC-V QEMU build demonstrates:

- boot contract validation
- capability-scoped console
- isolated U-mode ELF processes
- user-space virtio-block daemon
- typed IPC status and timeout behavior
- install/root marker on a real disk image
- app install, run, remove, and deny flows
- service stop, restart, and controlled fault recovery
- benchmark and denial suites

The x86_64 build demonstrates the shared Dezh IR path on a second ISA.

## What Makes The Prototype Interesting

- **No ambient authority:** there is no default device, filesystem, block, IPC,
  or time access for tasks.
- **Drivers outside the kernel:** the block device is serviced by a U-mode
  daemon that alone receives the MMIO and DMA grants.
- **Typed service contracts:** important storage and installer paths return
  structured statuses instead of raw ad hoc values.
- **Service supervision:** the console survives service stop and controlled
  service fault, then restarts the driver explicitly.
- **Install path discipline:** app install and app private storage go through
  the registered service path.
- **Reviewable evidence:** the smoke test and review demo exercise the path end
  to end under QEMU.

## Prototype Boundaries

Dezh is not production-ready. The current work is a research artifact with a
small kernel, embedded app bundles, a v0 registry format, and QEMU-centered
device support. The point of the current repository state is to make the
architecture concrete enough for serious review.

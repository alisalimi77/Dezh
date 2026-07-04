# Security Policy

Dezh OS is a research operating-system prototype. Security reports are welcome,
especially reports that affect capability enforcement, isolation boundaries,
package lifecycle integrity, service supervision, or user-space device grants.

## Supported Scope

Security review is currently focused on the public `main` and `develop`
branches.

| Area | Status |
| --- | --- |
| RISC-V QEMU bare-metal prototype | Supported for reports |
| x86_64 smoke target | Supported for build/smoke regressions |
| SDK package format and package store | Supported for reports |
| User-space `virtio-block` service path | Supported for reports |
| Production hardware deployments | Not supported |

## Reporting a Vulnerability

Send reports to:

```text
ali.salimi77@gmail.com
```

Please include:

- affected commit or branch
- reproduction steps
- expected vs actual behavior
- affected security boundary
- QEMU command or transcript, if available
- whether the issue allows authority widening, persistence corruption, package
  execution bypass, service compromise, or cross-task memory access

Reports should not be opened as public issues until there is a coordinated
disclosure decision.

## Security Boundaries Of Interest

High-priority reports include:

- a task using a capability it was not granted
- app execution from a non-`Active` package state
- package recovery widening capabilities
- hidden or direct kernel-side block I/O on a path that should use the
  user-space `virtio-block` daemon
- MMIO or DMA access without an explicit grant
- IPC send/receive behavior that bypasses capability gates
- service stop/fault state that causes hangs instead of clean failures
- disk registry, journal, or blob corruption that remains runnable

## Current Prototype Limitations

The current prototype intentionally does not claim production hardening. Known
limitations include:

- deterministic v0 package checksums rather than production signing
- modeled DMA isolation rather than real IOMMU enforcement
- QEMU-first hardware assumptions
- no production networking stack
- no formal verification of the full kernel or driver path
- small fixed-size package store and app registry

These limitations are documented so that reports can distinguish expected
prototype boundaries from defects.

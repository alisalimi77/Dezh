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

## Why not build this on seL4 (or Genode)?

The most important question, answered honestly. seL4 is a formally verified
capability microkernel; Genode is a mature capability component OS with
user-space drivers and typed IPC. For a *product*, building the Dezh model on top
of one of them would be the right call — you would inherit verification, real
object-capabilities, and IOMMU support instead of re-deriving them.

So the from-scratch kernel is **not the contribution, and we do not claim it is**
(see [DECISIONS.md](DECISIONS.md) D021). The contribution is the *model*: intent
as the sole authority-derivation path, an effect ledger on the authorization
path with a reversibility class, and honest whole-mission rollback, aimed at
autonomous agents. We wrote a small kernel to prototype that model end to end
with nothing hidden underneath and full control of the substrate while the ideas
were still moving — the pedagogical and iteration reasons, not a claim that the
world needs another microkernel.

The honest consequence: several things seL4/Genode already do well (verification,
per-object capabilities, IOMMU) are gaps here, named in
[STATUS.md](STATUS.md) and [THREAT_MODEL.md](THREAT_MODEL.md). A credible
productization path is to **port the intent→effect model onto seL4 or Genode**
and keep the model, not the kernel. If a reviewer's takeaway is "the ideas are
interesting but belong on a verified base," that is a conclusion we agree with.

## What is intentionally out of scope right now?

- production bootloader and installer media
- production networking (and with it, information-flow / exfiltration control)
- dependency solving
- real IOMMU integration
- graphics stack
- real hardware bring-up
- formal verification of the whole system
- online PKI / certificate-transparency for package signing (the signing
  *mechanism* now exists — see [PACKAGE_SIGNING.md](PACKAGE_SIGNING.md) — but the
  key-distribution layer does not)

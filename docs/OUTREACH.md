# Outreach Templates

These templates are for targeted technical review requests. Do not mass-send
them. Verify the appropriate public contact channel for each organization at
send time.

## OS And Systems Research

Subject: Technical review request: capability-secure OS prototype with user-space drivers

Hello,

I am sharing Dezh OS, a capability-secure operating-system research prototype.
The current RISC-V QEMU demo boots a small kernel, launches isolated U-mode
processes, runs virtio-block as a user-space service, and validates typed IPC
plus service supervision.

Review evidence:

- no ambient task authority; effects require explicit caps or grants
- block I/O flows through a user-space driver with MMIO/DMA grants
- service stop, controlled fault, and explicit restart are demonstrated

Repository: <review-repo-url>
Demo guide: <review-demo-url>

I would value technical feedback on the capability model, IPC/service contract,
and driver isolation path.

Thank you.

## Cloud Runtime And Sandbox Teams

Subject: Review request: capability-first runtime boundary prototype

Hello,

Dezh OS is a research prototype exploring strict capability boundaries for
services, apps, and drivers. The current demo focuses on explicit authority,
typed IPC, service-mediated storage, and clean failure when a required service
is stopped or faulted.

Review evidence:

- typed IPC statuses for service calls
- app install/run without direct device or block grants
- service fault recovery without console failure

Repository: <review-repo-url>
Demo guide: <review-demo-url>

I would appreciate feedback on whether this model is relevant to cloud
sandboxing, agent runtimes, or service-isolated execution.

Thank you.

## Embedded And Device Security Teams

Subject: Technical review request: small capability OS prototype for service isolation

Hello,

I am sharing Dezh OS for technical review. It is a small capability-secure OS
prototype that currently boots on QEMU RISC-V and demonstrates user-space
device service isolation, no-grant MMIO denial, app registry storage, and
supervised service restart.

Review evidence:

- U-mode task faults are contained
- virtio-block is isolated as a user-space daemon
- storage commands fail cleanly when the driver service is stopped or faulted

Repository: <review-repo-url>
Demo guide: <review-demo-url>

I would welcome feedback on the device isolation model and the path toward
IOMMU-backed DMA isolation.

Thank you.

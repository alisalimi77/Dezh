# Security Model v0

## Core Rule

No task receives authority by default. A task can only perform an effect if the
kernel, boot plan, service registry, or caller has explicitly granted the
required authority.

## Threat Model

The current prototype focuses on:

- untrusted U-mode tasks
- apps with limited declared capabilities
- service clients that should not touch devices directly
- faulty or stopped services
- malformed IPC requests
- no-grant MMIO access attempts

## Enforced Today

- Syscalls are gated by task capabilities.
- U-mode page tables deny access outside the task grant.
- MMIO is mapped only for tasks with explicit device grants.
- IPC send requires IPC capability.
- Transferred capabilities are attenuated to the sender's own authority.
- Foreground task faults kill only the faulting task.
- User-space block driver failure does not kill the console.
- Stopped or faulted block service causes clean command failure.

## Not Enforced Yet

- Real IOMMU-backed DMA isolation.
- Production package signatures.
- Multi-client block queues with per-client data windows.
- Full revocation model for long-lived delegated capabilities.
- Production installer and bootloader flow.
- Side-channel resistance.
- Formal verification.

## Reviewer Notes

The current security value is architectural discipline, not production
hardening. The relevant question is whether the authority boundaries are in the
right places and whether the demo proves those boundaries under fault and denial
scenarios.

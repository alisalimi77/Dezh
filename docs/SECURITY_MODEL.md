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

## Revocation (honest answer)

Reviewers ask this first, so here is the current stance plainly.

**What exists today.** Authority is *attenuable* and its *effects are
reversible*, which covers the common cases without a general revocation
mechanism:

- A delegated capability can never exceed the sender's own (`granted =
  requested & sender_caps`), so authority only ever narrows as it spreads.
- A capability is bound to a task; when the task exits or is killed on a fault,
  its authority is gone with it.
- Damage done through a granted capability is undone structurally: Cairn's
  commit log lets an operator roll a namespace back to a prior state (the F1/F2
  demos show exactly this — an agent's bad write is reverted after the fact).

**What does not exist yet.** There is no runtime *lease/revoke* for a
long-lived capability already delegated to a still-running task — you cannot,
today, reach into a live task and rescind one capability while it keeps running.
The honest reasons: capabilities are currently plain task-capability bits, not
first-class revocable objects with a revocation list, and the MVP prioritized
proving the grant/attenuation/rollback path end to end.

**How it is intended to work.** The direction (see
[STRATEGIC_DIRECTION.md](STRATEGIC_DIRECTION.md)) is a lease/generation scheme:
a delegated capability carries a generation stamp checked at use time, and
bumping the generation in the issuing service invalidates every outstanding
copy without tracking each holder. This falls out of the app-registry and
Cairn-ledger work rather than needing new kernel machinery. Until it lands,
revocation = drop the grant at the source, end the task, and roll back its
effects.

## Reviewer Notes

The current security value is architectural discipline, not production
hardening. The relevant question is whether the authority boundaries are in the
right places and whether the demo proves those boundaries under fault and denial
scenarios.

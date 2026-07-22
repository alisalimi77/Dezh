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

**What now exists (intent level).** An intent (`Ahd`) can be opened with a
**lease** (a bounded run count that auto-revokes on exhaustion) or **revoked**
explicitly; a revoked or exhausted intent authorizes nothing further, while the
effects it already produced keep their provenance (`tbar`/`sfar` still resolve).
This is the first realization of the generation/lease scheme, at the intent
layer — `lease-demo` proves it.

**What still does not exist (capability level).** There is no runtime
lease/revoke for a single, long-lived **task capability bit** already delegated
to a still-running task — you cannot reach into a live task and rescind one bit
mid-execution. The honest reason is the point below: task capabilities are
bitmask bits, not per-object revocable references.

## What kind of capability is this? (bitmask vs object-capability)

Being precise, because it is the most important honest caveat: a Dezh **task
capability is a bit in a per-task bitmask** (print, IPC, a Cairn namespace,
device, block), not an unforgeable reference to one specific object as in a true
object-capability system (seL4, CHERI). So the granularity is coarse: authority
is per *class/namespace*, not per *object instance*, and per-bit revocation and a
full delegation graph are not expressible yet.

Two things keep this from being "just Linux capabilities," though:

- **Not ambient, not inherited.** Linux capabilities are ambient process
  privileges that a child inherits by default. A Dezh task starts with **zero**
  authority; it holds only bits explicitly granted, and a spawned process
  inherits none.
- **Kernel-attested and attenuable per message.** The kernel stamps the sender's
  capabilities on **every** IPC message, and delegation is
  `granted = requested ∩ sender_caps` — you can pass a *narrower* subset of what
  you hold, checked by the kernel, and never more. Linux capabilities are not
  attenuable this way.

So Dezh sits **between** Linux capabilities and seL4/CHERI object-capabilities:
stronger than the former (no ambient authority, attenuable, kernel-attested),
coarser than the latter (bitmask classes, not per-object references). The honest
label is *capability-secure in the no-ambient-authority sense*, not *object-
capability*.

**The path (the one big change), now prototyped.** Turn a capability into a
first-class object — a generation-stamped handle to a specific resource — so that
(a) revocation of a single capability falls out (bump the generation; every
outstanding handle is invalidated at next use), and (b) delegation forms a real
provenance graph. This primitive is now **built and proven** in
`dezh_core::ocap` (`Cap` = object + rights + generation; `CapTable` holds the
live generation per object; `derive` attenuates rights along a delegation graph;
`revoke` bumps a generation to invalidate every outstanding handle to *that*
object). It is host-tested exhaustively and driven in the kernel by `cap-demo`:
mint a handle, derive an attenuated child, use both, then revoke the object and
watch the whole delegation subtree go stale at next use while a handle to a
*different* object keeps working — per-object revocation a bitmask cannot
express. A forged handle (guessed generation) is rejected.

Migration has **started on the live plumbing**, not just the primitive. The
Cairn **namespace** capability is now ocap-backed at the kernel chokepoint: the
console holds a generation-stamped handle per namespace, and the ocap gate is
enforced on **both** the operator console path (`cairn-commit`/`-get`/... via
`ns_authority_live`) **and the untrusted agent path** (`KHost::cairn_put`/
`cairn_get`). `ns-revoke` bumps a namespace's generation, and from that point a
commit or an agent's write to that namespace is refused until `ns-grant`
(`nsrevoke-demo`, `agentrevoke-demo`). So runtime revocation of a live namespace
capability is real for every kernel-side path today.

What **remains** is the deeper step: the daemon still attests authority per IPC
message with the coarse **bitmask** (kernel-attested, so unforgeable, but not
generation-checked at the object owner). Moving the daemon's own check to a
persisted per-object generation — so revocation survives reboot and is enforced
by the object owner itself — and migrating the remaining task-capability bits
onto `ocap` handles, is the largest remaining change. Until then, task-bit-level
revocation = drop the grant at the source, end the task, and roll back its
effects; the namespace capability already has full ocap revocation.

## Reviewer Notes

The current security value is architectural discipline, not production
hardening. The relevant question is whether the authority boundaries are in the
right places and whether the demo proves those boundaries under fault and denial
scenarios.

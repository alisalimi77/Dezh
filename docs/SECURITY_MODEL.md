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

Being precise, because it was the most important honest caveat. Dezh **started**
with authority as a bit in a per-task bitmask (print, IPC, a Cairn namespace,
device, block) rather than an unforgeable reference to one object as in seL4 or
CHERI. That is no longer the whole story: the authorities that name real objects
— **namespaces, devices, egress destinations** — are now generation-stamped
handles with per-object revocation and attenuated delegation (see the migration
below). What remains a plain bit is the process-level authority that names no
object (`print`, `time`, `ipc`), and the per-message attestation the storage
daemon uses.

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
far stronger than the former (no ambient authority, attenuable, kernel-attested,
and now per-object revocable for every authority that names an object), and still
short of the latter, whose object references are the *only* form authority takes
and are enforced by the kernel (or hardware) on every use rather than at
kernel-side chokepoints.

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

Revocation is now also **enforced by the object owner and survives reboot**: the
storage daemon records a per-namespace revoked flag in the Cairn superblock, so
`ns-revoke` persists on disk and the daemon refuses every operation on a revoked
namespace until `ns-grant` — independent of the in-memory kernel gate. A CI
reboot leg proves it: revoke a namespace, power-cycle, and the daemon still
refuses it from its superblock even though the kernel's in-memory handle is fresh.
So the Cairn namespace capability has full ocap revocation at three layers: the
console gate, the untrusted-agent (`KHost`) gate, and the persisted object-owner
check.

**Breadth: the object-like authorities are now all ocap-backed.** Beyond
namespaces, the two other authorities that name real objects have been migrated:

- **Devices.** Each device is an object with a generation-stamped handle
  (`dev-revoke` / `dev-grant`). Revoking one stops every use of that device
  regardless of finer authority — a kill-switch above the per-destination gate
  (`dev-demo`). The grants themselves are now **per-device**: the kernel finds
  the block device and the NIC and maps only their own pages, so neither daemon
  can reach the other's hardware. (The block grant previously mapped the whole
  virtio-mmio window.)
- **Egress destinations.** Authority names a destination, not "the network", and
  destinations are revoked individually (`marz-revoke <dest>`).

What deliberately stays a simple bit is the process-level authority that does not
name an object: `print`, `time`, `ipc`. These are ambient-style permissions of a
task, not references to a resource, so a generation-stamped handle would add
ceremony without adding a revocable object. If they ever name objects (a specific
console, a specific channel), they should migrate too.

## Reviewer Notes

The current security value is architectural discipline, not production
hardening. The relevant question is whether the authority boundaries are in the
right places and whether the demo proves those boundaries under fault and denial
scenarios.

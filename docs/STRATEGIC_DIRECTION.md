# Dezh Strategic Direction

## Position

Dezh should not be framed as a cleaner copy of existing operating-system ideas.
The long-term thesis is stronger:

**Dezh is an intent-native, effect-accountable operating-system prototype.**

The goal is not just to combine a microkernel, capability security, user-space
drivers, package rollback, and service supervision. Those are necessary
building blocks, but they are not the differentiator by themselves.

The differentiator should be that Dezh treats **intent** and **effect** as
first-class OS concepts.

## The Ground We Own (D021)

Dezh is **not** trying to be a better microkernel, a cleaner capability system,
or a kernel that compiles to more ISAs. Each of those has strong prior art
(seL4, KeyKOS, EROS, Barrelfish) and none is a defensible identity. Running on
both x86 and RISC-V is a portability property, not the point — **ISA is an
implementation backend, not identity**: the same mission bytes should produce
the same effect semantics on every backend, and if a new ISA appears in ten
years, Dezh's identity does not change.

**The real competitor is not another OS.** For the concrete job "contain an
untrusted agent and let it be productive," the incumbents are user-space
isolation layers: gVisor, Firecracker / microVMs, wasmtime / WASI,
seccomp+landlock, containers. They confine syscalls and resources well and they
ship today. Any honest positioning compares against *them*, not against a
research microkernel.

What none of them do:

- **Tie every effect to the intent that authorized it** as part of the
  execution model (not a bolt-on audit log an app can route around).
- **Reverse a whole agent mission atomically** — undo everything one intent
  caused, in one operation.
- Do both on a substrate where **the ledger cannot be bypassed**. On a
  conventional OS the ledger is a library sitting on top of ambient authority;
  a program can always reach the resource underneath. On Dezh there is no
  authority underneath to reach — the intent-derived path is the only path, so
  the ledger is not optional instrumentation, it is the execution itself.

**One-line differentiator** (the reviewer challenge): *Unlike seL4, Barrelfish,
Fuchsia, or Redox — which make **access** safe — Dezh makes **effect**
accountable: every action an agent takes is bound to its intent, attributable,
and reversible where possible, and because the kernel has no ambient authority
by construction, that ledger cannot be bypassed.*

### Value Is Only Visible Against An Adversary

A secure system that is never attacked is just an assertion. The proving demo
must carry a **villain**: an agent that actively *tries to escape* its intent —
read another namespace, write raw device MMIO, forge or amplify a capability,
act outside its declared intent, monopolize the CPU — and is stopped at a named
boundary each time, with `why-denied` explaining it. Happy-path demos (an app
acting inside its grant) do not make the value visible; the escape that fails
does.

### The Mission Is The Reversible Unit

The unit that makes effect-accountability compelling is not a single write but a
**mission**: the set of effects produced under one intent. Whole-mission atomic
rollback ("undo everything this agent's task did") is precisely what the
user-space sandboxes above cannot offer, because they have no structured notion
of which effects belonged to which authorized purpose. The ledger groups
effects by intent so a mission is a first-class, reversible object.

Honesty boundary: a mission may contain an **irreversible external effect** (a
network send, a physical output). Whole-mission rollback undoes the internal and
compensatable effects and **refuses the irreversible ones with an explanation**
— it never pretends an external effect was recalled. A separate
`docs/THREAT_MODEL.md` states what Dezh's trusted base is, what it defends, and
what it explicitly does not defend (side channels, a malicious kernel, hardware,
no-IOMMU DMA).

## Why This Matters

Traditional operating systems usually grant authority around processes, users,
files, devices, paths, package managers, or broad service APIs. That creates
common failure modes:

- Ambient authority that silently spreads through the system.
- Filesystem or registry state that accumulates unclear ownership.
- Package updates that change code, data, and permissions without enough
  reviewability.
- Service failures that turn into hangs, vague errors, or hidden recovery.
- Logs that describe what happened after the fact, but are not part of the OS
  authority model.

Dezh should avoid repeating these patterns.

## Core Thesis

Instead of asking only:

- Which process is running?
- Which file or device can it access?
- Which package is installed?
- Which service is reachable?

Dezh should also ask:

- What is the declared intent?
- Which authority was derived for that specific intent?
- Which namespace or service route was used?
- What effect did the operation create?
- Can the effect be verified, explained, rolled back, or quarantined?

## Competitive Advantages To Build Toward

### Intent-Scoped Authority

Authority should be issued for a declared purpose, not as a broad ambient grant.

Example:

- Avoid: "this app can write storage."
- Prefer: "this app can commit note update transaction #42 in its own namespace."

**Hard rule: intent is a mechanism, not metadata.** A narrow capability by
itself is not new — capability attenuation is decades old. Intent only becomes
a real OS concept if:

- deriving authority from a declared intent is the **only** way to obtain it,
- the kernel/runtime guarantees the derived capability is **narrower than or
  equal to** the declared intent,
- the intent, the derivation, and the resulting effects are linked in the
  ledger.

If intent is just a purpose string attached to a grant, it degenerates into
permission theater (the failure mode of macOS TCC purpose strings and loosely
checked OAuth scopes). Dezh must not ship that version.

### Effect Ledger

Important OS effects should be structured records, not loose logs:

- actor/component
- declared intent
- derived capability
- target namespace/service
- status
- **reversibility class**: `reversible` | `compensatable` | `irreversible`
- rollback or compensation handle (when the class allows one)
- generation/checkpoint metadata

Not every effect can be undone (a network send, a physical output). Claiming
universal rollback would violate D015 honesty; instead every ledger entry
declares its class up front, and `effect-rollback` refuses irreversible
entries with an explanation rather than pretending.

This should support commands such as:

- `effect-log`
- `effect-info <id>`
- `effect-rollback <id>`
- `why-denied <last|id>`

**Placement rule:** the ledger and denial-context store are user-space
services backed by Cairn, not kernel code. The kernel only emits minimal
structured events at authority boundaries; anything stateful lives outside it.
This keeps the microkernel minimal (D008) and makes the ledger itself
rollback-aware for free (D004).

### Reversible OS Boundary

Install, update, storage writes, service lifecycle changes, and namespace
migrations should be transaction-aware and preferably reversible or
compensatable.

Package lifecycle work already moves in this direction:

- transactional install/remove
- journaled recovery
- quarantine
- explicit GC
- update checkpoints
- rollback
- pin/unpin
- cap escalation review

The next step is to extend this model beyond packages into app data,
namespaces, services, and system generations.

### No Ambient Continuity

State should not silently carry forward forever.

Dezh should make generations explicit:

- boot generation
- service graph generation
- package generation
- namespace generation
- intent/effect generation

Rollback and audit should be generation-aware.

### Explainable Denial

"Permission denied" is not enough.

Dezh should explain:

- which intent was denied
- which capability was missing or too broad
- which component requested it
- which safer route is available
- whether review, migration, or explicit override is required

### Agent-Ready Without Blind Trust

Future systems will run more agents and automation.

Dezh should be designed so agents can operate productively without receiving
ambient authority:

- intent-scoped capability grants
- bounded namespaces
- structured effect ledger
- review gates for sensitive changes
- rollback/compensation where possible
- denial explanations that can guide safer retries

## Honest Novelty Accounting (D015)

Serious reviewers (seL4, Genode, CHERI communities) will immediately map each
piece to prior art. Dezh's public claims must do that mapping first:

Existing ideas Dezh builds on (never claim these as new):

- Capability security: KeyKOS, EROS, seL4, Capsicum.
- User-space drivers and minimal kernel: every serious microkernel.
- Generations and transactional packages: NixOS, ostree.
- Snapshot/rollback storage: ZFS, btrfs.
- Denial explanation: SELinux `audit2why` (bolted-on; ours is first-class,
  which is a UX differentiator, not a research one).

What is genuinely new in combination:

1. **Intent as the sole authority-derivation path**, enforced (derived
   capability ⊆ declared intent), not annotated.
2. **An effect ledger that ties each effect to its authority provenance**
   (actor → intent → derived capability → effect → rollback class/handle) as
   part of the OS authority model, not an audit afterthought.
3. **Agent-first framing**: the above two designed so untrusted agents are
   productive without ambient authority (D013).

Public wording pattern: "Dezh combines known building blocks X and Y; what is
new is 1–3 above." Anything stronger must be measured or demonstrated first.

## Architectural Guardrails

These should remain hard rules:

- No intent-as-metadata: authority is only derivable from a declared intent,
  and the derived capability must be provably narrower or equal.
- No ledger or denial-context state inside the kernel; those are user-space
  services on Cairn.
- No hidden kernel block I/O path.
- No global registry as an app-facing configuration dump.
- No Unix-style ambient filesystem authority as the default app model.
- No silent package update.
- No silent permission expansion.
- No automatic physical cleanup without explicit command and audit.
- No recovery path that widens authority.
- No service failure that causes indefinite hangs.
- No device/MMIO/DMA access without explicit grant.

## Relationship To The MVP (D019)

This document is the **narrative** over the already-defined MVP (D019,
`docs/ROADMAP.md` W1–W7), not a parallel roadmap. Rule: any work item here
must map onto an existing workstream or be explicitly marked post-MVP. Two
competing "what's next" documents would be strategic drift.

Mapping:

- Effect ledger → extends **W2** (Cairn v1 commit log is the ledger
  substrate) and the existing package journal.
- `effect-log` / `effect-info` / `effect-rollback` / `why-denied` →
  fold into **F1/W3** (agent containment demo) and **W2**.
- Intent-derivation rule → hardens **W1** (manifest cap grants become
  intent-derived grants).
- Capability attestation (`cap-audit`, `cap-tree`, `component-info`) →
  supports **F1** demo credibility; small enough to ride along W3.
- App storage namespace + migration (`ns-*`) → genuinely new scope;
  explicitly **post-MVP**. Recorded here so it is not lost, deliberately not
  started before the four flagship demos are green.

## Near-Term Milestones

These are now consolidated as roadmap **W8 (Intent + Effect Runtime)**, the one
workstream that turns D020/D021 from prose into a demonstrated differentiator.
W8 is not "add a feature"; it is the feature plus the three things that make its
value legible to a skeptical practitioner audience — an adversary, a
whole-mission rollback with an honest irreversible effect, and an owned cost.

### 1. Intent as mechanism (Ahd)

- `intent-open <kind>` issues an **Ahd** (an intent token: a ceiling of
  capabilities for a target namespace), `intent-run <ahd> <app>` runs an app
  whose derived capability is proven ⊆ the Ahd, `intent-list` enumerates open
  Ahds.
- Manifest grants (W1) become Ahd-derived; a request for authority beyond the
  Ahd is denied. This rides the existing IPC attenuation and per-task
  capability bits.

### 2. Effect ledger on Cairn (Sand) — built (W8 P2)

- **Sand is the same Cairn v1 commit log, enriched — not a parallel store.**
  The user-space storage daemon (which alone holds the disk capability) records
  each effect on the very commit that produces it: `actor → intent (Ahd) →
  derived capability → target namespace → status → reversibility class →
  generation`, alongside the pre-existing `parent → hash`. The intent id and
  derived cap are supplied by the kernel on the commit IPC; the daemon only
  records them.
- Commands: `sand-log <ns>`, `sand-info <ns>`, and `sand-demo` (open an intent →
  run an agent under it → read the effect back off the ledger). Provenance
  survives a reboot because it lives on the durable commit.

### 3. Mission (Sfar) + whole-mission rollback + honest external effect

- A **Sfar** groups the effects under one Ahd; `effect-rollback <sfar>` undoes
  them atomically; `effect-rollback <id>` undoes one.
- At least one `irreversible` external effect (simulated network/print) that
  rollback **refuses with an explanation**, and one `compensatable` effect with
  a registered compensation action.

### 4. The adversary

- A `redteam` scenario: a malicious agent that attempts cross-namespace reads,
  raw MMIO writes, capability forgery/amplification, out-of-intent actions, and
  CPU monopoly — each stopped at a named boundary (page fault / capability check
  / intent bound / preemption) with `why-denied`.

### 5. Explainable denial + provenance

- `why-denied <last|id>`, `cap-tree` / `cap-audit` / `component-info`, and
  **Tbar**, a queryable `actor → intent → effect` provenance graph
  ("everything this agent touched and why").

### 6. Credibility layer

- **Cost:** the per-effect ledger overhead measured and folded into
  `BENCH.md` (D015).
- **Head-to-head:** a documented scenario where gVisor / Firecracker /
  wasmtime cannot cleanly undo a whole mission but Dezh can (Dezh's side
  reproducible in CI even if the competitor is only described).
- **`docs/THREAT_MODEL.md`:** trusted base, what is defended, and what is
  explicitly not defended.

### 7. One flagship narrative

All of the above collapse into a single story — "leave a coding agent loose on
your machine overnight" — with a transcript and a CI smoke leg. This is the
final form of the F1 (D020) agent-containment demo, not a separate demo.

The first implementation maps a small set of intents onto existing package,
storage, and service operations, with the ledger stored in Cairn.

### 2. Capability Attestation v1 (rides along W3)

Make authority explainable at runtime.

Candidate commands:

- `cap-audit`
- `cap-tree`
- `why-denied`
- `component-info <id>`

### 3. App Storage Namespace + Migration v0 (post-MVP)

Package update is now stronger than data lifecycle. The next major gap after
the MVP demos is app data.

Build:

- per-app namespace identity
- namespace metadata
- migration-required flag
- migration transaction
- rollback-aware data contract
- namespace verification

Candidate commands:

- `ns-list`
- `ns-info <app>`
- `ns-migrate <app>`
- `ns-verify <app>`

### 4. Dezh Tooling MCPs

MCP should be used around Dezh, not inside the OS kernel/runtime.

Highest-value MCP candidates:

1. `dezh-qemu-mcp`
   - boot QEMU
   - send commands
   - preserve disk image across reboots
   - collect transcript
   - assert expected OS behavior

2. `dezh-image-mcp`
   - inspect raw disk image
   - decode install marker
   - decode package registry
   - decode journal
   - show package blobs, quarantine, GC state

3. `dezh-guard-mcp`
   - enforce architecture guardrails
   - detect kernel-side block I/O regressions
   - detect ambient capability paths
   - scan public docs/package for unsafe claims, secrets, local paths, or
     non-public identity markers

4. GitHub MCP
   - CI status
   - PR/release/review package workflow

5. Browser/Playwright MCP
   - docs/review kit/demo rendering checks

## Review Outcome (2026-07-04)

The direction was reviewed critically and accepted with three corrections,
now folded into the text above:

1. **Intent must be a mechanism, not metadata** — otherwise it is renamed
   audit logging. Added as a hard guardrail.
2. **This document binds to the MVP (D019)** instead of forking the roadmap;
   namespace migration is explicitly post-MVP.
3. **Novelty claims follow D015 honesty** — prior art is named; the genuinely
   new parts are the intent-derivation rule, the provenance-linked effect
   ledger, and the agent-first combination.

Answers to the open review questions:

- The strongest single differentiator is the **effect ledger tied to
  authority provenance**, not intent alone.
- The proving demo is **F1 extended**: give an untrusted agent an intent →
  show the derived narrow capability → agent acts → `effect-log` shows the
  record → `effect-rollback` undoes it → agent attempts something outside the
  intent → kernel denial → `why-denied` explains. One demo covers intent,
  ledger, rollback, explainable denial, and agent containment.
- The main drift risks are convenience pressure (granting the shell broad
  capabilities) and letting ledger/denial state creep into the kernel; both
  are now guardrails.

Registered as D020 in `DECISIONS.md`.

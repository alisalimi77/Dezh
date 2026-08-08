# Architecture

The layers and their authority boundaries, the diagrams that show them, and where
each piece lives in the tree.

---

## Design

<!-- was docs/ARCHITECTURE.md until the 2026-07-23 consolidation -->

Dezh is a bare-metal OS research prototype focused on explicit authority,
service-mediated effects, and recoverable lifecycle operations.

For visual diagrams, see [Diagrams](ARCHITECTURE.md#diagrams).

### Design Center

The current prototype is built around four rules:

1. No ambient authority by default.
2. Device and storage access are service-mediated.
3. Persistent lifecycle changes are transactional and recoverable.
4. Runtime state should be inspectable by reviewers from the console and tests.

The strategic direction is to make **intent** and **effect** first-class OS
concepts. The current implementation is not fully intent-native yet, but the
package, service, IPC, and storage work is deliberately moving toward that
shape.

### Boot Flow

1. OpenSBI starts the RISC-V kernel in S-mode on QEMU `virt`. The boot hart is
   whichever one firmware chose — it is never assumed to be hart 0.
2. The kernel validates the boot contract from `dezh-kernel`.
3. The kernel installs trap handling, timer support, and Sv39 paging.
4. The PLIC is programmed to route virtio interrupts to the boot hart's S-mode
   context, so device I/O can block instead of spin.
5. Secondary harts are started over the SBI HSM protocol, each with its own
   stack, trap stack, and per-hart `ApCtx` reached through `sscratch`.
6. A capability-scoped console starts over UART on the boot hart.
7. Services are declared from the boot plan and materialized in the service
   registry.
8. Long-lived services such as `virtio-block` and `marz` are started explicitly
   or lazily from the registry.

### Kernel Responsibilities

The kernel owns the confinement boundary:

- address-space construction
- trap and syscall handling
- task scheduling, on the boot hart and symmetrically across secondary harts
- device interrupt routing (PLIC) and blocking I/O, so a waiting driver costs no CPU
- multi-hart bring-up (SBI HSM) and the mutual exclusion that shared state needs
- IPC queues and typed receive timeout support
- information-flow gates on both axes: secrecy on export, integrity on ingress
- service registry state
- explicit process launch grants
- frame ownership and reclaim
- fault containment for U-mode tasks, including on a secondary hart

The kernel does not implement the block or network I/O path directly — both are
U-mode daemons holding nothing but the one device page and DMA window each was
granted.

### Process Model

Each ELF process receives:

- its own address space
- entry point and initial arguments
- task capabilities
- optional device mappings
- optional DMA mappings
- tracked frame ownership for reclamation

Foreground clients are reclaimed after exit or fault. Daemons remain alive until
they stop, fault, or are explicitly restarted.

### Capability Model

Task capability bits currently cover:

- print
- time
- IPC
- virtio-block device
- block read
- block write
- Cairn namespaces 0..7 (bits 8..15): one bit per named storage namespace
- egress destinations (bits 16+): one bit per *named destination*, not one bit
  for "the network" — revoking `vault-sync` leaves `ops` intact

Device authority is separately live-checked (`dev-grant` / `dev-revoke`), so a
daemon that already holds a mapped device page can still be refused at the gate.

Information-flow labels (secrecy taint, integrity endorsements) are **not**
capability bits: a task can hold every bit it needs and still be denied because
the flow itself is illegal. See [Information Flow](#information-flow-secrecy-and-integrity).

The important property is attenuation: a task can only transfer capabilities it
already holds. Manifest-declared package capabilities are separately translated
into runtime grants; a manifest `cairn-read`/`cairn-write` grant maps to the
app's **own** namespace bit only (matched by app name) — a manifest can never
name another app's namespace.

### IPC

The base IPC syscall sends a small payload, a scalar word, and an attenuated
capability grant. Service paths pack a typed v0 envelope into the scalar word:

```text
proto | service_id | op | request_id | status | arg
```

Storage, installer, app, and package paths use typed replies. Legacy demos can
still use raw scalar messages.

**Kernel-attested sender capabilities:** on every send, the kernel records the
sender's capability set in the message; on receive, the service gets that set
alongside the payload. A service therefore checks the *sender's* authority
against values a client cannot forge from user space. This is how the storage
daemon enforces per-namespace access, and why its denials can name the exact
missing capability (`why-denied` direction from the strategic plan).

### User-Space Block Driver

The `virtio-block` daemon is a separate U-mode ELF. It alone receives:

- the virtio MMIO page grant
- the DMA window grant
- IPC authority
- block read/write authority

Foreground clients do not receive MMIO authority. A no-grant process touching
the MMIO address faults and is killed without killing the console.

The daemon handles:

- disk probe
- block write/read
- root install marker and metadata
- Cairn v0 current/previous value operations (legacy demo path)
- Cairn v1 commit-log store with per-namespace capability checks
- embedded app registry operations
- package registry, journal, and blob sectors
- note/lab/calc/vault private storage
- stop and controlled fault demo

### User-Space Network Daemon (Marz)

The network edge is a second U-mode ELF, and deliberately not the same one. Marz
receives its **own** virtio-net MMIO page and its **own** DMA window: two devices,
two grants, so neither daemon can reach the other's hardware or corrupt the
other's virtqueue.

Authority to send is not "network access". It is a capability for a named
**destination** (address, port, and the secrecy label that destination is cleared
to receive), so egress can be revoked one destination at a time.

Both directions exist:

- **Egress** — the gate runs before a packet exists: device authority live, then
  destination capability held, then the secrecy check against that destination's
  label. Only then does a frame leave, and the transmission is recorded on the
  ledger as irreversible.
- **Ingress** — Marz offers the NIC receive buffers, blocks on the device
  interrupt, resolves the destination by ARP, and completes an ICMP echo
  exchange, matching the reply by id and sequence. What comes back is
  attacker-chosen, so consuming it lowers integrity.

### Interrupts And Blocking I/O

Device I/O is interrupt-driven. A driver submits a request, calls `sys_irq_wait`
with the interrupt count it last saw, and is parked if nothing new arrived — its
`SEPC` rewound so the `ecall` re-runs on wake. When no task is Ready but one is
waiting on a device, the scheduler idles on `wfi` instead of returning.

The kernel services the PLIC by hand in that idle path deliberately: hardware
clears `sstatus.SIE` on trap entry, so a pending interrupt would wake `wfi` and
never be taken, stranding the sleeping driver. Counters are visible from
`irq-stat`.

### SMP

Secondary harts come up over SBI HSM and pull U-mode tasks off one shared run
queue protected by a fair ticket spinlock. Tasks land wherever a hart is free and
several run in U-mode at the same instant.

Parallelism does not cost isolation: each task carries its own address space
(only its own stack region is U-mapped), so a task reaching into a concurrent
neighbour's memory page-faults and dies on its own hart while the neighbour runs
on. Per-hart trap state is reached through `sscratch`, never `tp` — a U-mode task
owns every integer register and will have clobbered `tp` by the time it traps.

Honest scope: tasks on secondary harts run to completion (no preemption or
migration there yet), and the console's own scheduler is still single-hart.

### Information Flow: Secrecy And Integrity

Capabilities answer "may this actor touch this object?". Information flow answers
the separate question "may these *bits* go there?", and it has two axes
(`dezh_core::difc`):

- **Secrecy** — reading a labelled namespace raises the actor's taint, and taint
  only ever rises. A tainted actor cannot write down into a less-secret sink or
  export to a destination not cleared for that label.
- **Integrity** — a sink may *require* endorsements; a value flows in only if it
  carries them (no write-up). Consuming unvalidated input can only ever lower an
  actor's integrity — the exact dual of taint only ever rising.

The escapes are explicit, privileged, and recorded: `declassify` for secrecy,
`endorse` for integrity. They stay separate on purpose — `declassify` does not
return lost integrity and `endorse` does not clear secrecy, so one privileged act
never grants two. The lattice rules are proven exhaustively over the 8-bit label
space, including that the two axes are independent so one gate cannot mask the
other.

Live paths: `ns=note` and `ns=vault` require an endorsement, `ns=lab` (scratch)
requires none; completing a network exchange lowers the operator's integrity, so
a commit into a demanding namespace is refused with an explainable denial until a
recorded endorsement (`taintflow-demo`, `ingress-demo`).

Honest scope: the ingress taint is at **operator granularity** — consuming any
network reply lowers integrity wholesale rather than tracking individual bytes —
and neither axis is enforced across the client→daemon IPC hop yet.

### Cairn v1 (Commit-Log Store)

Cairn v1 lives inside the storage daemon on sectors 1600..1855:

- a superblock holding the namespace table (`note`, `lab`, `calc`, `vault`,
  `agent`) with each namespace's head ref and commit count;
- append-only commit records, each carrying: parent ref, FNV-1a hash of the
  value object, actor task id, a reversibility flag, and the inline value.

Semantics:

- **Commit** appends a record and moves the namespace head ref.
- **Rollback N** walks the parent chain and moves the ref back; history is
  never erased, and the state survives reboot.
- **Verify** re-hashes the head object against its commit record.
- **Access** requires the namespace's capability bit, checked against the
  kernel-attested sender capability set; denials name the missing capability.

The commit record fields (actor, reversibility class, provenance chain) are
the seed of the effect ledger described in
[Strategic direction](ROADMAP.md#strategic-direction) (decision D020).

Dezh-IR apps reach the store through the kernel's IR host, which routes
`cairn_put`/`cairn_get` host calls over typed IPC to the daemon with the app's
own namespace capability — there is no kernel-side block I/O shortcut.

### Service Registry

The service registry tracks:

- service name
- service kind
- state
- task id
- caps
- grants
- restart count
- last exit
- last started tick
- fault reason

Manual stop and controlled fault are not hidden by automatic restart. Review
commands use explicit `svc-restart` so service recovery remains visible and
deterministic.

### Package Store

The SDK builds `.dzp` packages. The OS stores them through the user-space block
service, not through a kernel block path.

Current package features:

- persistent registry on disk
- transaction journal
- active, previous, and stage blob areas
- install/remove/update/rollback
- recovery and quarantine
- pin/unpin
- cap-escalation review
- explicit physical cleanup through `pkg-gc run`

Only `Active` packages are runnable. `Removed`, `Corrupt`, `Pending*`, and
`Quarantined` packages do not run.

### Embedded Apps

The current embedded app set is intentionally mixed:

- `note`: persistent text app
- `lab`: UI-like multi-task app with cooperating workers
- `calc`: calculator app with stored last result
- `vault`: private-value app used to exercise storage and device-denial paths

These are review demos, not a production app ecosystem.

### Storage Path

The storage path is:

```text
console command -> foreground client -> typed IPC -> virtio-block daemon
               -> granted MMIO/DMA -> disk image
```

This path is central to the project. It proves that storage does not silently
fall back to a kernel block driver.

### Review Surface

Useful review commands:

- `services`
- `tasks`
- `ipcstat`
- `ipc-typed-demo`
- `pkg-store`
- `pkg-journal`
- `pkg-review <name>`
- `pkg-versions <name>`
- `pkg-gc`
- `cairn-demo` / `cairn-log <ns>` / `cairn-rollback <ns> [n]` / `cairn-verify <ns>`
- `agent`
- `overnight` (the W8 intent → effect flagship, end to end)
- `irq-stat` (interrupt counts and sleeping drivers)
- `smp-sched` / `smp-isolate` (symmetric scheduling, isolation under parallelism)
- `marz-demo` / `marz-ping` (guarded egress, ARP + ICMP receive)
- `taintflow-demo` / `ingress-demo` / `taint` / `declassify` / `endorse`
- `why-denied` / `tbar`
- `bench-all`

Useful review tools:

- `tools/ci/qemu_smoke.py`
- `tools/ci/sdk_test.py`
- `tools/review/scan_public.py`
- `tools/demo/run_review_demo.py`
- `tools/demo/run_agent_demo.py` (F1 agent-containment transcript)

---

## Diagrams

<!-- was docs/ARCHITECTURE_DIAGRAMS.md until the 2026-07-23 consolidation -->

These diagrams are part of the review surface. They show the current prototype,
not a production promise.

### System Overview

```mermaid
flowchart TB
    subgraph Kernel["Kernel boundary"]
        Trap["Trap + syscall handling"]
        VM["Address-space builder"]
        Sched["Task scheduler (boot hart)"]
        SMP["SMP: ticket lock + shared run queue"]
        IRQ["PLIC: device interrupts -> S-mode"]
        IPC["IPC queues + typed timeout"]
        DIFC["Information flow: secrecy + integrity"]
        Services["Service registry"]
        Frames["Frame ownership + reclaim"]
    end

    Console["Console task"] --> Trap
    Console --> Services

    subgraph User["U-mode processes"]
        VBlk["virtio-block daemon"]
        Marz["Marz egress daemon"]
        Client["Foreground clients"]
        Apps["Installed apps"]
        Bench["Benchmark app"]
    end

    Trap --> User
    IPC --> VBlk
    Client -->|typed IPC| VBlk
    Apps -->|declared caps only| IPC
    SMP -->|dispatch onto secondary harts| User

    VBlk -->|explicit MMIO grant| MMIO["virtio-mmio page"]
    VBlk -->|explicit DMA window| DMA["DMA bounce window"]
    DMA --> Disk["QEMU raw disk image"]

    Marz -->|its OWN NIC page + DMA| NIC["virtio-net page"]
    NIC --> Wire["the wire"]
    Wire -.->|reply lowers integrity| DIFC

    IRQ -.->|wakes a sleeping driver| VBlk
    IRQ -.->|wakes a sleeping driver| Marz
```

Every arrow into a device is an explicit grant, and the two daemons hold
*different* ones: neither can reach the other's hardware or DMA window.

### Blocking I/O Sequence

Device I/O is interrupt-driven, not polled. A driver that is waiting occupies no
CPU, and the kernel has somewhere to idle when nothing is runnable — without
which blocking on I/O is impossible, since the scheduler would simply return.

```mermaid
sequenceDiagram
    participant D as Driver (U-mode)
    participant K as Kernel
    participant P as PLIC
    participant Dev as virtio device

    D->>Dev: submit request (queue notify)
    D->>K: sys_irq_wait(last_seen)
    Note over K: count unchanged -> park the task,<br/>rewind SEPC so the ecall re-runs
    K->>K: nothing Ready, but a task waits on a DEVICE
    K->>K: wfi + service the PLIC by hand
    Dev-->>P: raises its interrupt line
    P-->>K: claim
    K->>Dev: ACK (InterruptStatus / InterruptACK)
    K->>P: complete
    K->>D: mark Ready
    D->>K: ecall re-runs, returns the new count
```

The kernel services the PLIC by hand in its idle path deliberately: the hardware
clears `sstatus.SIE` on trap entry, so a pending interrupt would wake `wfi` and
never be taken, stranding the sleeping driver.

### SMP: Symmetric Scheduling

The boot hart runs the console; secondary harts pull U-mode tasks off a shared
queue and run them in parallel. Each task carries its own address space, so
parallelism does not cost isolation.

```mermaid
flowchart LR
    Boot["Boot hart<br/>(console, service registry)"] -->|fills| Q[("Shared run queue<br/>(ticket lock)")]
    Q --> H1["Secondary hart 1"]
    Q --> H2["Secondary hart 2"]
    Q --> H3["Secondary hart 3"]

    H1 --> T1["Task A<br/>own satp, own stack"]
    H2 --> T2["Task B<br/>own satp, own stack"]
    H3 --> T3["Task C<br/>own satp, own stack"]

    T1 -. "cross-task write<br/>page-faults" .-> T2

    H1 --> AP1["per-hart ApCtx<br/>frame + trap stack + kctx"]
    H2 --> AP2["per-hart ApCtx"]
    H3 --> AP3["per-hart ApCtx"]
```

Per-hart state is reached through `sscratch` (whose value is that hart's `ApCtx`,
frame first), never through `tp` — a U-mode task owns every integer register and
will have clobbered `tp` by the time it traps.

### The Network Edge, Both Directions

Egress names a *destination*, not "the network", and is checked against secrecy
before a packet exists. Ingress is the mirror: what arrives is unvalidated, so
consuming it lowers integrity until an explicit endorsement.

```mermaid
flowchart TB
    Op["Operator / agent"]

    Op -->|"send to <dest>"| G1{"device capability live?"}
    G1 -->|no| D1["DENIED"]
    G1 -->|yes| G2{"destination capability held?"}
    G2 -->|no| D2["DENIED"]
    G2 -->|yes| G3{"secrecy: taint fits<br/>the destination?"}
    G3 -->|no| D3["DENIED: would exfiltrate"]
    G3 -->|yes| TX["Marz transmits"]
    TX --> L["irreversible effect on the ledger"]

    Op -->|"probe <dest>"| RX["Marz: ARP + ICMP echo,<br/>parses the reply"]
    RX --> I["integrity LOWERED<br/>(input is unvalidated)"]
    I --> G4{"write into a namespace<br/>requiring endorsement?"}
    G4 -->|not endorsed| D4["DENIED: would become trusted state"]
    G4 -->|after endorse| W["write permitted"]

    classDef deny stroke:#e5534b,stroke-width:2.5px;
    classDef ledger stroke:#8250df,stroke-width:2.5px;
    class D1,D2,D3,D4 deny
    class L ledger
```

Across every diagram here a red border marks a refusal and a purple one marks
something the ledger now carries. Only the stroke is set, so both borders keep
their meaning whichever theme GitHub renders the page in.

### Boot And Service Graph

```mermaid
flowchart LR
    OpenSBI["OpenSBI"] --> Boot["dezh-boot"]
    Boot --> Contract["Validate boot contract"]
    Contract --> Paging["Install traps + Sv39"]
    Paging --> Registry["Build service registry"]
    Registry --> Console["Start console"]

    Console -->|lazy start| VBlk["virtio-block service"]
    VBlk --> Running["Running"]
    Running -->|svc-stop| Stopped["Stopped"]
    Running -->|svc-fault-demo| Faulted["Faulted"]
    Stopped -->|svc-restart| Running
    Faulted -->|svc-restart| Running
```

### Storage Authority Path

```mermaid
sequenceDiagram
    participant C as Console command
    participant K as Kernel launch gate
    participant F as Foreground client
    participant D as virtio-block daemon
    participant Disk as Raw disk image

    C->>K: request storage operation
    K->>F: launch with IPC + DMA, no MMIO
    F->>D: typed IPC request
    D->>Disk: block I/O through granted MMIO/DMA
    Disk-->>D: status/data
    D-->>F: typed status
    F-->>C: command result
```

Important property: clients do not receive device MMIO authority. The daemon is
the only process with the virtio MMIO page grant.

### Package Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Empty
    Empty --> PendingInstall: pkg-recv
    PendingInstall --> Active: commit verified blob
    PendingInstall --> Quarantined: suspicious recovery
    Active --> PendingRemove: pkg-remove
    PendingRemove --> Removed: commit remove
    Removed --> Empty: pkg-gc run
    Active --> Active: pkg-update commit
    Active --> Active: pkg-rollback
    Active --> Corrupt: blob/registry verify failure
    Corrupt --> Quarantined: explicit recovery
    Quarantined --> [*]
```

Lifecycle rules:

- Only `Active` packages are runnable.
- New capabilities during update require explicit `--allow-new-caps`.
- Pins block update and rollback until explicit review.
- GC never touches `Active`, `Corrupt`, or `Quarantined` slots.

### Disk Layout

```mermaid
flowchart TB
    S0["sector 0<br/>install marker"] --> S2["sector 2<br/>Cairn v0 current"]
    S2 --> S3["sector 3<br/>Cairn v0 previous"]
    S3 --> S4["sector 4<br/>root metadata"]
    S4 --> S5["sectors 5..7<br/>app registry v0"]
    S5 --> S24["sector 24<br/>package marker"]
    S24 --> S25["sectors 25..31<br/>package registry"]
    S25 --> S32["sectors 32..39<br/>package journal"]
    S32 --> S64["sectors 64..575<br/>active package blobs"]
    S64 --> P["sectors 576..1087<br/>previous blobs"]
    P --> ST["sectors 1088..1599<br/>stage blobs"]
    ST --> C1["sector 1600<br/>Cairn v1 superblock"]
    C1 --> C2["sectors 1601..1855<br/>Cairn v1 commit log"]
```

The package store is intentionally small and inspectable in v0:

- 8 package slots
- 32 KiB per slot
- active, previous, and stage blob areas
- journaled recovery before package execution

### Cairn v1 Commit Log

Each namespace is a ref into an append-only chain of commit records. Rollback
moves the ref; nothing is erased.

```mermaid
flowchart RL
    subgraph Super["Superblock (sector 1600)"]
        NSnote["ns=note head"]
        NSvault["ns=vault head"]
        Next["next free slot"]
    end

    C2["commit slot 2<br/>value: bad-write<br/>parent: 1<br/>hash + actor"] --> C1["commit slot 1<br/>value: note-v2<br/>parent: 0<br/>hash + actor"]
    C1 --> C0["commit slot 0<br/>value: note-v1<br/>parent: none<br/>hash + actor"]

    NSnote -. before rollback .-> C2
    NSnote == after rollback 1 ==> C1
```

Commit record fields — parent ref, object hash (FNV-1a), actor task id, and a
reversibility flag — are the on-disk seed of the effect ledger direction in
[Strategic direction](ROADMAP.md#strategic-direction) (D020).

### Namespace Capability Attestation (F1/F2 core mechanic)

The storage daemon never trusts what a client *says*; it checks what the
kernel *attests* the sender holds.

```mermaid
sequenceDiagram
    participant A as Agent app (holds ns=agent bit)
    participant K as Kernel (SYS_SEND / SYS_RECV)
    participant D as Storage daemon

    A->>K: send commit request (ns=note)
    Note over K: kernel records sender's<br/>capability set in the message
    K->>D: deliver request + attested sender caps
    Note over D: check bit for ns=note<br/>in attested caps
    D-->>A: DENIED: ns=note requires CAIRN_NS_0,<br/>sender holds caps=0x...

    A->>K: send commit request (ns=agent)
    K->>D: deliver request + attested sender caps
    D-->>A: OK: commit slot N, parent P, hash H
```

### Multi-ISA Execution (F3 direction)

The same Dezh-IR bytecode runs on every Dezh kernel; only the thin host
bindings differ per ISA.

```mermaid
flowchart TB
    Source[".dzs source (SDK assembler)"] --> IR["Dezh-IR bytecode<br/>(verified, capability-gated)"]
    IR --> Engine["dezh-core engine<br/>(one shared no_std crate)"]
    Engine --> RV["RISC-V kernel host<br/>print → UART, cairn → storage daemon"]
    Engine --> X86["x86_64 kernel host<br/>print → COM1"]
```

### Authority And Denial

```mermaid
flowchart TB
    Request["Operation request"] --> Intent["Declared operation / intent"]
    Intent --> CapCheck["Capability check"]
    CapCheck -->|allowed| Route["Service route / namespace"]
    Route --> Effect["Effect record or command result"]
    CapCheck -->|denied| Denial["Structured denial"]
    Denial --> Explain["why-denied direction"]

    classDef deny stroke:#e5534b,stroke-width:2.5px;
    classDef ledger stroke:#8250df,stroke-width:2.5px;
    class Denial deny
    class Effect ledger
```

The current implementation has capability-gated operations and audit events.
The strategic direction is to make intent and effect records first-class OS
objects.

---

## Repository layout

<!-- was docs/REPO_STRUCTURE.md until the 2026-07-23 consolidation -->

This repository mixes a bare-metal OS prototype, host-side research crates, SDK
tooling, QEMU test harnesses, and public review documentation. This file is the
map for reviewers.

### Bare-Metal Targets

| Path | Role |
| --- | --- |
| `dezh-boot/` | Main RISC-V QEMU `virt` boot target. Contains kernel entry, console, task model, service registry, package store, package lifecycle, embedded apps, and user-space process launch. |
| `dezh-boot/virtio-blk/` | User-space `virtio-block` daemon. It receives explicit MMIO and DMA grants and performs the prototype disk I/O path. |
| `dezh-boot/marz/` | User-space network daemon. It receives its own virtio-net MMIO page and its own DMA window — separate from the block daemon's — and performs guarded egress plus the ARP/ICMP receive path. |
| `dezh-boot/linux-guest/` | Static Linux/RISC-V ELF used by Pol (`linux-elf`); the same bytes run on real riscv64 Linux. |
| `dezh-boot/userprog/` | Small user program used by legacy demos and process-launch smoke paths. |
| `dezh-boot/bench-app/` | U-mode benchmark app used by `bench-all`. |
| `dezh-boot/note-app/` | Embedded note demo app. |
| `dezh-boot/lab-app/` | Embedded multi-task lab demo app. |
| `dezh-boot/calc-app/` | Embedded calculator demo app. |
| `dezh-boot/vault-app/` | Embedded private-value demo app. |
| `dezh-boot-x86/` | Smaller x86_64 boot/smoke target for multi-ISA validation. |

### Shared Crates

| Path | Role |
| --- | --- |
| `dezh-core/` | Shared `.dzp`, base64, and Dezh-IR support used by the boot target and SDK-adjacent code. |
| `dezh-kernel/` | Boot contract, kernel plan, install manifest, and plan validation logic. |
| `spikes/` | The Step 1..9 host-side prototypes, superseded and **not shipping** — nothing on the bare-metal path depends on them. Kept as the record of which design question each one settled; see [spikes/README.md](../spikes/README.md). |

### Tools

| Path | Role |
| --- | --- |
| `tools/ci/qemu_smoke.py` | Boots RISC-V or x86_64 QEMU targets and asserts expected console behavior. |
| `tools/ci/sdk_test.py` | End-to-end SDK/package lifecycle acceptance test across multiple QEMU reboots. |
| `tools/sdk/build_pkg.py` | Builds `.dzp` packages from app directories. |
| `tools/sdk/install_pkg.py` | Boots Dezh in QEMU and streams packages through the console upload protocol. |
| `tools/sdk/dzas.py` | Tiny Dezh-IR assembler for SDK apps. |
| `tools/demo/run_review_demo.py` | Runs the review demo and captures a transcript. |
| `tools/demo/run_agent_demo.py` | Runs an agent-containment demo transcript. |
| `tools/review/scan_public.py` | Public hygiene scan for review-package readiness. |
| `tools/review/make_review_package.py` | Builds a clean review package snapshot. |

### Documentation

| Path | Role |
| --- | --- |
| `README.md` | Public landing page and quick review path. |
| `docs/ARCHITECTURE.md` | Architecture explanation. |
| `docs/ARCHITECTURE.md#diagrams` | Mermaid diagrams for the current prototype. |
| `docs/SECURITY_MODEL.md#enforcement-model` | Threat model and enforced/not-yet-enforced boundaries. |
| `docs/ROADMAP.md#strategic-direction` | Intent-native/effect-accountable direction and open review questions. |
| `docs/SDK_GUIDE.md` | How to build, install, update, and run `.dzp` packages. |
| `docs/REVIEWER_GUIDE.md` | Short path for external technical review. |
| `docs/ROADMAP.md` | Roadmap and current milestone direction. |
| `docs/DECISIONS.md` | Architecture decision notes. |
| `docs/REVIEWER_GUIDE.md#running-the-demos` | Manual demo script. |
| `docs/WHITEPAPER.md` | Technical whitepaper draft. |
| `docs/OUTREACH.md` | Draft outreach templates. |

### Generated/Local Artifacts

These should not be committed:

- `target/`
- `dist/`
- `graphify-out/`
- raw QEMU disk images (`*.img`)
- Python bytecode caches

The repository intentionally keeps reproducible tools and transcripts, but not
local generated build output.

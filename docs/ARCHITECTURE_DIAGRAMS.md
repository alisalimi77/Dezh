# Dezh Architecture Diagrams

These diagrams are part of the review surface. They show the current prototype,
not a production promise.

## System Overview

```mermaid
flowchart TB
    subgraph Kernel["Kernel boundary"]
        Trap["Trap + syscall handling"]
        VM["Address-space builder"]
        Sched["Task scheduler"]
        IPC["IPC queues + typed timeout"]
        Services["Service registry"]
        Frames["Frame ownership + reclaim"]
    end

    Console["Console task"] --> Trap
    Console --> Services

    subgraph User["U-mode processes"]
        VBlk["virtio-block daemon"]
        Client["Foreground clients"]
        Apps["Installed apps"]
        Bench["Benchmark app"]
    end

    Trap --> User
    IPC --> VBlk
    Client -->|typed IPC| VBlk
    Apps -->|declared caps only| IPC

    VBlk -->|explicit MMIO grant| MMIO["virtio-mmio page"]
    VBlk -->|explicit DMA window| DMA["DMA bounce window"]
    DMA --> Disk["QEMU raw disk image"]
```

## Boot And Service Graph

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

## Storage Authority Path

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

## Package Lifecycle

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

## Disk Layout

```mermaid
flowchart LR
    S0["sector 0\ninstall marker"] --> S2["sector 2\nCairn v0 current"]
    S2 --> S3["sector 3\nCairn v0 previous"]
    S3 --> S4["sector 4\nroot metadata"]
    S4 --> S5["sectors 5..7\napp registry v0"]
    S5 --> S24["sector 24\npackage marker"]
    S24 --> S25["sectors 25..31\npackage registry"]
    S25 --> S32["sectors 32..39\npackage journal"]
    S32 --> S64["sectors 64..575\nactive package blobs"]
    S64 --> P["sectors 576..1087\nprevious blobs"]
    P --> ST["sectors 1088..1599\nstage blobs"]
    ST --> C1["sector 1600\nCairn v1 superblock"]
    C1 --> C2["sectors 1601..1855\nCairn v1 commit log"]
```

The package store is intentionally small and inspectable in v0:

- 8 package slots
- 32 KiB per slot
- active, previous, and stage blob areas
- journaled recovery before package execution

## Cairn v1 Commit Log

Each namespace is a ref into an append-only chain of commit records. Rollback
moves the ref; nothing is erased.

```mermaid
flowchart RL
    subgraph Super["Superblock (sector 1600)"]
        NSnote["ns=note head"]
        NSvault["ns=vault head"]
        Next["next free slot"]
    end

    C2["commit slot 2\nvalue: bad-write\nparent: 1\nhash + actor"] --> C1["commit slot 1\nvalue: note-v2\nparent: 0\nhash + actor"]
    C1 --> C0["commit slot 0\nvalue: note-v1\nparent: none\nhash + actor"]

    NSnote -. before rollback .-> C2
    NSnote == after rollback 1 ==> C1
```

Commit record fields — parent ref, object hash (FNV-1a), actor task id, and a
reversibility flag — are the on-disk seed of the effect ledger direction in
[STRATEGIC_DIRECTION.md](STRATEGIC_DIRECTION.md) (D020).

## Namespace Capability Attestation (F1/F2 core mechanic)

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

## Multi-ISA Execution (F3 direction)

The same Dezh-IR bytecode runs on every Dezh kernel; only the thin host
bindings differ per ISA.

```mermaid
flowchart TB
    Source[".dzs source (SDK assembler)"] --> IR["Dezh-IR bytecode\n(verified, capability-gated)"]
    IR --> Engine["dezh-core engine\n(one shared no_std crate)"]
    Engine --> RV["RISC-V kernel host\nprint → UART, cairn → storage daemon"]
    Engine --> X86["x86_64 kernel host\nprint → COM1"]
```

## Authority And Denial

```mermaid
flowchart TB
    Request["Operation request"] --> Intent["Declared operation / intent"]
    Intent --> CapCheck["Capability check"]
    CapCheck -->|allowed| Route["Service route / namespace"]
    Route --> Effect["Effect record or command result"]
    CapCheck -->|denied| Denial["Structured denial"]
    Denial --> Explain["why-denied direction"]
```

The current implementation has capability-gated operations and audit events.
The strategic direction is to make intent and effect records first-class OS
objects.

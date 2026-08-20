# Roadmap and direction

Where the work is going. The roadmap is the near-term plan; the strategic
direction is the reasoning that produced it and is the slower-moving document.

---

## Roadmap

<!-- was docs/ROADMAP.md until the 2026-07-23 consolidation -->

### MVP — The Reviewable OS (current focus)

One sentence: **install Dezh, write an app for it, hand it an untrusted
program or agent — it can only do what you granted, and its effects are
rollbackable.**

MVP is done when a stranger, with no help from us, can:

1. Boot a downloadable Dezh image in a VM (QEMU one-liner; VirtualBox for x86).
2. Write and install their own app in about 10 minutes using the SDK.
3. Reproduce four flagship demos, one per differentiator (below).

Every claim follows D015: measured, honestly scoped, no bare superlatives.

#### Flagship demos (one per differentiator)

| # | Differentiator | Demo a reviewer runs | Honest claim wording |
| --- | --- | --- | --- |
| F1 | Agent containment (D001/D013) | Install an "agent" app with narrow caps: it works inside its grant, is DENIED by the kernel beyond it, delegates an attenuated cap to a sub-task over IPC, and its damage is undone by rollback. | "Authority is explicit, unforgeable, attenuable — enforced by hardware privilege + paging, not by a sandbox policy file." |
| F2 | Cairn storage (D004/D005) | App state is versioned: write, snapshot, corrupt, roll back → restored across reboot. A second app is DENIED access to the first app's namespace. | "State recovery is structural (versioned objects + refs), not fsck. Per-app namespaces are capability-gated." |
| F3 | Multi-ISA apps (D003/D016) | The same byte-identical `.dzp` package (Dezh-IR payload) installs and runs on the RISC-V kernel and the x86_64 kernel. | "Apps are ISA-portable by construction; proven today on 2 ISAs (RISC-V, x86_64), designed for all." |
| F4 | Pol compatibility (D007/D011/D014) | An unmodified static Linux riscv64 ELF (built on stock Ubuntu) runs under the Linux personality, capability-gated; syscall-translation overhead is measured and published. | "Near-native compute for same-ISA binaries (no emulation); syscall translation overhead measured at N ns vs native Linux on the same substrate. Coverage is a small syscall subset today." |

#### Workstreams

##### W1 — SDK, packages, install flow (foundation; everything rides on it)

- `app.toml` manifest: name, version, entry, payload type (`elf-riscv64` |
  `dezh-ir`), requested capabilities (print, uptime, cairn namespace, ...).
- `.dzp` package format: header + manifest + payload, built by
  `tools/sdk/build-pkg.py` from an out-of-tree app directory.
- App template + "write your first Dezh app in 10 minutes" guide
  (becomes the heart of REVIEWER_GUIDE).
- Package ingestion into a live system: UART upload command
  (`install-pkg`, chunked/base64) first; disk-image staging as fallback.
- Grants happen at install time from the manifest (mobile-permission
  feel, but kernel-enforced and unforgeable); recorded in the app
  registry; visible via `app-permissions`.
- Dogfood: port `calc`, `vault`, `lab` from embedded bundles to `.dzp`.
- Acceptance: an out-of-tree hello app builds on the host, installs into a
  running Dezh, runs; an undeclared cap use is DENIED.

##### W2 — Cairn v1 (differentiator F2)

- On-disk object store with a ref/commit log: rollback N steps, not just
  current/previous sectors; survives reboot.
- Per-app namespaces (`/app/<name>/...`) mediated by the storage service
  over typed IPC; namespace access is a manifest capability.
- `cairn-demo` console flow proving F2 end to end.
- Acceptance: F2 transcript reproducible by the demo runner.

Status (2026-07-04): DONE. Commit-log store on sectors 1600..1855
(superblock + append-only commit records carrying parent ref, FNV-1a object
hash, actor task id, and a reversibility flag — the D020 effect-ledger seed).
Namespace access is enforced by kernel task-capability bits 8..15: the kernel
attests the sender's caps on every IPC recv and the storage daemon checks the
requested namespace's bit, with an explainable denial message. Console:
`cairn-commit/get/log/rollback/verify/status` + `cairn-demo`; rollback moves
the head ref and keeps history. Manifest wiring: a `cairn-read`/`cairn-write`
grant maps to the app's OWN namespace only (matched by app name); IR apps
reach the store through the kernel Host routed over IPC to the user-space
daemon (no kernel block I/O path). Covered by CI smoke (including a
second-boot persistence phase) and the review demo runner.

##### W3 — Agent containment demo (differentiator F1; ties W1+W2 together)

- Agent app (Dezh-IR payload) with a narrow cairn namespace grant.
- Shows: in-grant work, kernel DENIED beyond grant, attenuated delegation
  over IPC (`granted = requested & sender_caps`), rollback of its writes.
- Publish alongside the capability-vs-syscall mediation benchmark.
- Acceptance: F1 transcript reproducible by the demo runner.

Status (2026-07-04): first full pass DONE via `tools/demo/run_agent_demo.py`
(in CI): SDK-built out-of-tree `agent` app uploaded over the UART, installed
with manifest-scoped grants (own namespace only), does durable in-grant
commits then a bad write; the operator undoes the damage with a one-step
rollback (hash-verified, history kept); a no-capability `spy` app is DENIED
by the kernel; attenuated delegation shown over IPC; state re-checked after
reboot. Transcript: `docs/transcripts/agent-f1.md`. Found and fixed a
latent W1 bug on the way: the storage daemon truncated every sector write to
511 bytes, corrupting any package larger than two sectors. Remaining polish:
fold the mediation benchmark numbers into the published F1 material.

##### W4 — Pol: run a real foreign binary (differentiator F4)

- Extend the process ELF loader to load an unmodified static Linux
  riscv64 ELF (musl hello-world class), personality = Linux.
- Syscall subset: write, exit/exit_group, brk, set_tid_address (sane
  stubs); everything else → clean ENOSYS. No threads, no dynamic linking.
- Measure translation overhead vs native Linux on the same substrate;
  publish the number and method (D015).
- Acceptance: a binary compiled on stock Ubuntu runs on Dezh,
  capability-gated (no PRINT cap → denied).

Status (2026-07-06): DONE. `dezh-boot/linux-guest` is a genuine static
riscv64 musl ELF (no Dezh code) issuing the raw Linux syscall ABI via `ecall`;
the console `linux-elf` command loads it under the Linux personality — `write`
serviced by Pol with the PRINT cap, denied `-EACCES` without it, unsupported
`getpid` returns a clean `-ENOSYS`. The very same bytes also run unmodified on
real riscv64 Linux (verified under `qemu-riscv64-static`). Translation overhead
is measured by the `bench-pol` command (native vs Pol path, kernel-timed):
~0–80 ns/call, within noise of the ~780 ns emulated round trip — a fixed,
near-noise dispatch (BENCH.md, F4). Both legs are in CI smoke.

##### W5 — x86_64 to parity for F3 (largest chunk)

- M2: IDT/exceptions + timer on the x86 kernel.
- Package runner on x86: execute the same `.dzp` Dezh-IR payload
  (print/arith hostcalls; cairn on x86 deferred until it has a disk).
- M3: real bootable ISO (Limine) → boots in VirtualBox/VMware, which also
  delivers the "install it like a real OS" feel.
- Acceptance: F3 — byte-identical package runs on both kernels; x86 ISO
  boots in VirtualBox.

Status (2026-07-06): F3 and the bootable ISO are DONE; M2 is partial. The
x86_64 kernel installs and runs a real `.dzp` agent package
(pack → parse → verify → run) — the same architecture-independent format the
SDK builds and the RISC-V kernel installs. The agent bytecode is pinned
byte-identical by dezh-core's `demo_sum_bytes_are_pinned` test (in CI), so both
ISAs provably execute the same bytes. A Multiboot2 header + `tools/x86/build-iso.sh`
(GRUB `grub-mkrescue`) produce a BIOS ISO that boots in QEMU `-cdrom` **and in
VirtualBox** (screenshot: docs/assets/dezh-x86-virtualbox.png); output is
mirrored to the VGA text buffer so it is visible on the VM screen. The QEMU
`-kernel` PVH path still works for CI. **M2 (DONE):** the x86 kernel installs a
256-vector IDT. The first 32 route every CPU fault to a handler that reports
vector/error/RIP and halts — the boot deliberately raises a breakpoint to prove
faults are caught, not silent triple-faults. Vectors 32..255 are the returnable
path (W16.1): they save every general-purpose register, dispatch, restore and
`iretq`, and a Local APIC timer armed at 100 Hz from a PIT-measured rate proves
it by leaving an interrupted work loop intact across the ticks. **W16.2** adds
the other half: the dispatcher may hand back a *different* saved frame, which is
the whole context switch, and three kernel tasks with no yield in them are
round-robined by the tick. **W16.3** makes that containment: per-task address
spaces, a GDT and TSS that can describe an untrusted task, ring 3, one DPL3
syscall gate, and a fault that kills the faulting task instead of the machine.
**W16.4** makes the authority derived rather than ambient: syscalls are
capability-checked and the grant is `requested ∩ ceiling` through the shared
`dezh_core::mcap`, so two tasks with identical code and manifest hold different
authority because their intents differ. Still future work: device IRQs, storage,
and an effect ledger on x86.

##### W6 — Independence and release packaging

- Prebuilt release artifacts: `dezh-riscv.img` + one-line QEMU script,
  `dezh-x86.iso` for VirtualBox.
- Install/app state persists across reboot (app registry on disk).
- CI builds the images and runs the full demo transcript from a fresh
  clone.

##### W7 — Presentation hygiene (before any outreach)

- LICENSE (Apache-2.0 proposed).
- Honesty pass over all docs: QEMU-only status, emulated-vs-native
  benchmark caveats, syscall coverage, no IOMMU yet, revocation status.
- Revocation: at minimum a documented honest answer; implement cheap
  lease/revoke if it falls out of the registry work.
- Refresh REVIEWER_GUIDE / DEMO_SCRIPT around the four flagship demos.

Suggested order: W1 → W2 → W3 → W4 → W5 → W6 → W7 (W7 items can land
alongside any workstream; outreach only after all four flagship demos are
green in CI).

##### W8 — Intent + Effect Runtime (the differentiator made visible; D020/D021)

The MVP (W1–W7) proves the *mechanism* — no ambient authority, capability-gated
storage, rollback, multi-ISA, Pol. W8 turns that mechanism into the thing the
project is actually *about*: an unbypassable intent-to-effect ledger, and it is
scoped so the value is legible to a skeptical practitioner audience (not another
happy-path demo). It is the final form of the F1 demo, not a new differentiator.

Real competitor to beat: not another OS, but user-space agent isolation
(gVisor, Firecracker, wasmtime/WASI, seccomp+landlock). W8 must show something
they structurally cannot — attributing and reversing a whole agent mission.

- **Intent as mechanism (Ahd). — DONE (P1).** `intent-open <kind>` mints an
  **Ahd** (a capability ceiling), `intent-run <ahd> <app>` runs an app whose
  derived capability is proven ⊆ the Ahd — the *only* path to authority — and
  `intent-list` enumerates open Ahds. `intent-demo` is the self-contained proof
  (same agent under two Ahds). A request for authority beyond the Ahd is DENIED
  in a CI smoke leg.
- **Effect ledger on Cairn (Sand). — DONE (P2).** Sand is the **same** Cairn v1
  commit log (user-space, never kernel), enriched so every commit *is* an effect
  record: the commit header now carries `intent (Ahd id) → derived cap →
  reversibility class → status → generation` alongside the existing
  `actor → parent → hash`. It is **not** a parallel store. The intent id and
  derived cap are threaded kernel→daemon on the commit IPC (request-id +
  status byte) and recorded by the daemon that owns the disk. Commands
  `sand-log <ns>`, `sand-info <ns>`, and the self-contained `sand-demo` (open a
  writer intent → run the built-in agent under it → read the effect back off the
  ledger). CI proves effects are recorded, carry their intent, and survive a
  reboot with the provenance intact.
- **Mission (Sfar) + whole-mission rollback + honest external effect. — DONE
  (P3, first slice).** A **Sfar** = the effects under one Ahd (found by the
  intent id stamped on each Sand commit). `sfar-plan <ahd>` is the rollback
  **forecast** — it walks the *live* per-namespace chains and reports how many of
  the mission's effects are `reversible` / `compensatable` / `irreversible` /
  `unknown`, with an honest confidence (never "full" if anything cannot be
  undone). `sfar-rollback <ahd>` retracts the contiguous reversible head-run per
  namespace with a single atomic superblock write and **refuses** the rest with
  an explanation. A fourth reversibility class `unknown` exists so a connector
  that does not declare semantics is never optimistically treated as reversible.
  `sfar-demo` is the self-contained proof: a mission with one MODELED
  irreversible external send + two reversible writes → forecast "partial" →
  rollback undoes the two writes and refuses the send ("already happened in the
  outside world"). CI proves the outcome and that the refused effect + its
  provenance survive a reboot. **Slice 2 — DONE.** `comp-demo` proves a
  `compensatable` effect with a *registered compensating action* is undone by
  running and recording that action (`status=compensation` on the ledger) rather
  than refused; `sfar-cross-demo` proves mission authority spans every namespace
  a mission touched (a rollback holding authority over only one of two namespaces
  is refused, naming the missing one).
- **The adversary (`redteam`). — DONE (P4).** A malicious agent *tries to escape*
  five ways — cross-namespace read, raw MMIO write, capability forgery/
  amplification, out-of-intent action, CPU monopoly — each stopped at a named
  boundary (storage capability check / hardware paging / kernel syscall check /
  intent-derivation ceiling / preemptive scheduler); the system survives every
  one. CI asserts all five named boundaries.
- **Explainable denial + provenance. — DONE (P5).** `why-denied` walks the event
  ring and names the boundary that produced the last denial; **Tbar** (`tbar
  <ahd>`) renders the queryable `actor → intent → effect` provenance graph,
  unforgeable because the intent id + derived cap are stamped kernel→daemon.
- **Credibility layer. — DONE (P6).** Per-effect ledger overhead documented in
  BENCH.md (D015: the enrichment is +12 header bytes in the same commit sector,
  zero extra I/O); `docs/SECURITY_MODEL.md#threat-model` states the trusted base, what is
  defended (with the mechanism for each), and the explicit non-goals (side
  channels, malicious kernel, hardware, no-IOMMU DMA), plus the head-to-head
  where a user-space sandbox cannot cleanly undo a whole mission but Dezh can
  (Dezh side reproducible in CI).
- **One flagship narrative. — DONE (P7).** `overnight` collapses P1–P5 into a
  single story — "leave a coding agent loose on your machine overnight" — with a
  captured transcript (`docs/transcripts/overnight.md`) and a CI smoke leg.

**W8 is complete:** every part above is green in `tools/ci/qemu_smoke.py`.

##### W9 — Hardware maturity: interrupts and SMP

The bottleneck that most kept Dezh from reading as a real OS was that it drove no
hardware asynchronously: all device I/O was polled and only one hart ever ran.
Both are now addressed on RISC-V, in order:

- **Interrupt-driven I/O. — DONE.** A PLIC routes virtio device interrupts to the
  boot hart's S-mode context; drivers block on `sys_irq_wait` (a restartable
  blocking syscall) and are woken by the device rather than by spinning; the
  scheduler idles with `wfi` for a device when nothing else is runnable and
  services the PLIC by hand (the hardware clears `sstatus.SIE` on trap entry, so a
  pending interrupt must be taken explicitly). `irq-stat` reports interrupts
  serviced and driver waits woken by hardware; CI asserts both.
- **SMP bring-up + parallel proof. — DONE.** Secondary harts are started through
  the standard **SBI Hart State Management** call, each given its own stack and
  `tp` = hart id; a parallel round has every secondary hammer one shared atomic
  counter and the coherent total proves genuine concurrent execution on shared
  memory (`smp-demo`; asserted at boot under `-smp 4`). The boot hart is read from
  the firmware, not assumed to be hart 0 — which surfaced and fixed a latent PLIC
  bug (interrupts were hardcoded to hart 0's context).
- **Mutual-exclusion lock. — DONE.** Symmetric scheduling needs a run queue shared
  by more than one hart, which is impossible without a lock — and the kernel had
  none (single-hart discipline covered everything until now). A fair **ticket
  spinlock** (`TicketLock`, FIFO order so no hart starves) is now in the kernel and
  proven: all four harts hammer a **non-atomic** counter under it and the total
  lands exactly on `contributors x work` (`smp-demo` reports `MUTEX-OK`; CI asserts
  it at boot and interactively). Atomics alone cannot prove this — the hardware
  serialises them regardless — so the non-atomic counter is the point.
- **Shared run queue. — DONE.** The structural core of a symmetric scheduler: ONE
  queue of work, every hart popping the next item under the lock and running it in
  parallel. 48 jobs are enqueued and drained concurrently by all harts, and the
  correctness property a run queue must have is checked — **every item runs exactly
  once** (none lost to a torn dequeue, none run twice by two harts). `smp-demo`
  reports `QUEUE-OK`; CI asserts it at boot and interactively.
- **A U-mode task on a secondary hart. — DONE.** A real U-mode task is now
  dispatched onto a secondary hart: it switches into the task's address space,
  drops to U-mode through a **separate AP trap path** (its own trap stack + saved
  kernel context, kept isolated from the boot hart's `utrap`/`KCTX` so the console
  scheduler is untouched), services the task's syscalls **on that hart**, and
  longjmps back to the hart's loop when the task exits — all while the boot hart
  keeps running the console. `smp-task` reports `U-MODE-ON-AP`; CI asserts the
  task's own output appears and it runs to completion on a hart other than the boot
  hart. Landing this surfaced a real bug worth recording: the AP trap path must not
  read `tp` to find its stack/context, because a U-mode task owns every integer
  register and clobbers `tp` before it traps.
- **Symmetric scheduling, with isolation intact. — DONE.** The previous step ran
  *one* task *pinned* to a hart the boot hart chose. Now the boot hart fills a
  single task queue and **every** secondary hart pulls from it, so tasks land
  wherever a hart is free and several execute in U-mode **at the same instant**
  (`smp-sched`: 4 tasks placed across 3 harts, each run exactly once, peak 3 live
  concurrently → `SCHED-OK`).
  - Per-hart state is found **through `sscratch`, not `tp`**: each hart's `ApCtx`
    begins with its trap frame, so the trap entry lands on it and reads that hart's
    trap stack and saved kernel context at fixed offsets. Several harts can be in a
    trap simultaneously.
  - Parallelism did **not** cost isolation. Each task gets its **own address
    space** — a private copy of the page tables in which only that task's stack
    region carries the U bit — so two tasks running concurrently on two harts cannot
    touch each other's memory. `smp-isolate` proves it: a task that reaches into a
    neighbour's stack page-faults and is killed on its own hart while the neighbour
    runs on undisturbed (`ISOLATION-OK`).
- **A receive path on the network. — DONE.** A transmit-only stack cannot be
  checked against reality — nothing answers it. The Marz daemon now arms the NIC's
  receive queue, blocks on the device interrupt, resolves its destination with
  **ARP**, and completes a real **ICMP echo** exchange, matching the reply by id and
  sequence (`marz-ping <dest>` → `NET-RX-OK`). Ingress is gated by the same
  authority as egress: a revoked device or destination refuses the probe. CI now
  **decodes** the packet capture instead of scanning it — necessary because the
  host answers with ICMP errors that quote our datagram, which a substring count
  would misread as extra egress. Landing this also fixed a real bug: the internet
  checksum dropped the final byte of an odd-length body, so our echo request was
  silently discarded by the host and no reply ever came.
- **Information flow on ingress. — DONE.** Having a receive path creates a new
  hole, and it is not the one secrecy solves. Bytes off the wire are not *secret*;
  they are *unvalidated*, and the danger is that they quietly become trusted state.
  `dezh_core::difc` gained the **integrity** axis (Biba's dual: a sink may
  *require* endorsements, and reading untrusted input can only lower an actor's
  integrity, never raise it), proven exhaustively over the label space. In the
  kernel, `ns=note` and `ns=vault` require an endorsement, talking to the network
  lowers the operator's integrity, and a write into a demanding namespace is
  refused until a privileged, recorded `endorse` (`ingress-demo` → `INGRESS-OK`).
  The two escapes stay separate on purpose: `declassify` does not restore
  integrity and `endorse` does not clear secrecy, so one privileged act cannot
  grant two.
- **Remaining SMP work. — NOT started.** Tasks run to completion on the hart that
  picked them: there is no preemption or migration on a secondary hart (no timer
  armed there yet), and the *console's* own scheduler — task table, IPC mailboxes,
  frame allocator — is still single-threaded on the boot hart and not yet under the
  lock. Merging the two schedulers into one lock-protected structure, so every task
  in the system (daemons included) is dispatchable on any hart, is the next step.

##### W10 — A foundation that can be developed on

W1–W9 proved the mechanisms. W10 does not add one: it removes the four things
that made the *next* ten commits cost more than the last ten did.

The problem was measurable. The kernel that ships had **no unit tests** — all
110 host tests belonged to crates that ran nowhere. `dezh-boot/src/main.rs` was
**8,722 lines** in one file with 44 `static mut`, 184 `unsafe` blocks and a
~200-arm console. The root workspace carried **eight superseded prototypes** and
194 crates of `wasmtime` that nothing shipping needed. And **nothing linted the
kernel at all**, so its 103 warnings could only grow.

Guardrail for the whole workstream: **no behaviour change**. Every step is
verified by the *existing* QEMU legs passing unchanged. A step that needs a
smoke-test edit is a step that got something wrong.

- **Lint ratchet. — DONE.** `cargo clippy -- -D warnings` runs over all five
  trees (host workspace `--all-targets`, `dezh-core`, `spikes`, `dezh-boot`,
  `dezh-boot-x86`), and the 103 existing warnings are gone. Most were mechanical
  (45 function-item-to-integer casts now go through `*const ()`). Four were
  judgement calls kept with a reason at the site rather than silently "fixed":
  `AP_OFF_KCTX` and `DEV_OBJ_BLOCK` look dead to the compiler but record the
  ApCtx layout and the device enumeration; `COM1 + 0` on x86 is the UART
  register map written out in order; three record-encoding signatures are wide
  because they *are* the record's fields.
- **`static mut` → pointer access. — DONE.** Taking `&`/`&mut` to a static a
  second hart can reach is undefined behaviour, not a style preference, and
  secondary harts have run real U-mode tasks since W9. All 37 reference-creating
  sites sat in four clusters — device authority, namespace authority,
  information flow, and the event ring — each now a `Global<T>` whose only
  accessor returns `*mut T`, with the hart that may touch it stated at the
  declaration. Clusters were converted whole; converting one static of a
  three-static ring buffer would have been worse than either end state.
- **Superseded spikes moved out. — DONE.** `spikes/` is its own workspace, off
  the default CI path but still built, with a README recording what each of the
  eight proved and which subsystem superseded it. The root lockfile went from
  194 crates to 1. Deleting them was the other option and the wrong one: this
  repository treats the record of *why* a design is what it is as part of the
  work. What it must not do is sit in the shipping tree pretending to be live.
- **First unit tests for kernel logic. — DONE (first slice).** Manifest
  capability derivation — the narrowest, most load-bearing decision in the
  system — moved to `dezh_core::mcap` and the kernel now calls it rather than
  keeping a copy. Nine tests, two exhaustive over the whole manifest bit space
  and every app name: granted authority never exceeds the manifest, an app
  reaches its own Cairn namespace and no other, an unknown app name yields no
  namespace rather than a default, and `cap_delta` reports escalation exactly.
  Plus four `crc32` tests against the published IEEE 802.3 vectors — it had only
  ever been exercised in a loop closed on itself. dezh-core: 39 → 52 tests.
- **Pinned toolchain. — DONE.** The lint gate went red on its first real CI run,
  on code nobody had touched: the contributor's machine had Rust 1.94, CI
  resolved `stable` to 1.97, and 1.97 ships lints 1.94 does not know. Three
  sites were genuinely better fixed than allowed (two `checked_div`, one
  `sort_by_key`), but the fix is `rust-toolchain.toml`: new lints now arrive when
  the version is deliberately bumped, and local matches CI by construction.
  `-D warnings` on a floating `stable` turns Rust's release calendar into a
  source of red builds, and a gate that fails for reasons unrelated to the
  change under review is a gate people learn to route around.
- **Split `main.rs` into modules. — NOT started.** See W11; it is P1.
- **Edition 2024. — PARTIAL.** Two blockers cleared early because they are valid
  in 2021 (`gen` is now a reserved keyword; five `extern "C"` blocks are
  `unsafe extern`). The rest is measured, not guessed: 81 `#[unsafe(no_mangle)]`
  conversions and 65 `unsafe_op_in_unsafe_fn` sites where an `unsafe fn` body is
  no longer implicitly an unsafe block. See W15.

---

#### Cross-ISA status, and where the two kernels actually diverge

W5 and W16 are the only workstreams with x86 in the title, and reading the rest
of this file it would be easy to conclude that everything else is ISA-neutral.
It is not. Every workstream from W8 onward has landed on RISC-V alone, and this
section exists so that fact is stated once, in a table, rather than inferred
from silence.

**What is genuinely shared.** `dezh-core` — 2,228 lines of `mcap`, `dzp`, `ir`,
`sig`, `ocap`, `difc`, `b64`. Both kernels execute the same pinned `.dzp` bytes
(`demo_sum_bytes_are_pinned`, in CI), and both derive authority through
`mcap`'s `requested ∩ ceiling`. That is the whole of the shared surface: x86
reaches `dezh_core::{mcap, dzp, ir}` and nothing else, and it does not depend on
`dezh-kernel` at all — even though `dezh-kernel` already models
`BootTarget::QemuVirtioX86_64`.

| Capability | RISC-V | x86_64 | Why the gap |
|---|---|---|---|
| Boots, long/S-mode, own page tables | yes | yes | — |
| Runs the pinned `.dzp` / Dezh-IR package | yes | yes | the F3 claim; the point of `dezh-core` |
| Authority as `requested ∩ ceiling` | yes | yes | shared `mcap` |
| Timer, returnable IRQ path | yes | yes | W9 / W16.1 |
| Preemptive scheduler, ring 3, per-task address space | yes | yes | W9 / W16.2–3 |
| Bootable ISO (GRUB, VirtualBox) | no | yes | the one place x86 is ahead; RISC-V boots via `-kernel` |
| Console (941 lines vs 56) | yes | no | never built on x86 |
| Device IRQs (PLIC), blocking, `irq_wait` | yes | no | W16 remainder |
| Disk, virtio-block driver | yes | no | W16 remainder; blocks Cairn and the ledger |
| IPC (typed, timeouts, mailboxes) | yes | no | no counterpart in x86's task model |
| Cairn (commit log, namespaces, rollback) | yes | no | needs a disk first |
| Effect ledger (Sand), mission (Sfar) | yes | no | needs Cairn first |
| DIFC taint, Marz egress, ocap tables | yes | no | never built on x86 |
| Pol (foreign Linux binary) | yes | no | RISC-V-specific by nature (Linux syscall ABI per ISA) |
| Package install lifecycle (`pkg.rs`, 3,059 lines) | yes | no | needs a disk first |
| SMP: several harts, one scheduler (W13) | in progress | no | no x86 AP bring-up at all |

**The divergence that actually costs money is not the missing features — it is
the data model underneath them.** These two schedulers were derived twice,
independently, and they do not agree on what a task *is*:

| | `dezh-boot/src/sched.rs` | `dezh-boot-x86/src/sched.rs` |
|---|---|---|
| size | 1,203 lines | 437 lines |
| task state | `Unused / Ready / Blocked / Done` | `Idle / Runnable` |
| the running task | `[usize; MAX_HARTS]`, `NO_TASK` doubling as the run claim | one `AtomicUsize` |
| saved frame | 33 slots (32 registers + the dispatching hart) | 22 qwords |
| blocking, IPC, resource accounting | yes | none |

So a step like W13 cannot be *ported* to x86; it would have to be re-derived,
because there is no `Blocked` state to teach about harts and no claim to make
per-hart. Every deep change from here is paid for twice unless something
changes.

**What we are choosing, deliberately.** Not parity. D021's claim is that the ISA
is an implementation backend, and what that claim needs is for x86 to have a
*runtime* — which W16.1–W16.4 delivered — not for x86 to have Cairn. So x86 is
a second-class backend until W16 completes, and this file says so rather than
implying otherwise by omission.

**The rule going forward.** Every workstream below carries a `*Cross-ISA:*` line
saying which of three it is: **shared** (lands once, in `dezh-core` or
`dezh-kernel`), **RISC-V first** (will need re-deriving on x86, and W16 owns
that debt), or **RISC-V only by design** (nothing to port). An entry with no
such line is a gap in this ledger, not an ISA-neutral workstream.

**The extraction question, and its answer for now.** The obvious fix to the
double-payment is to lift the arch-independent half of the scheduler — task
table, state machine, IPC, the run claim, the capability checks — into a crate
behind a trait, leaving frame layout, trap assembly, `satp`/`cr3`, PLIC/APIC and
SBI/ACPI on the arch side. That is the right end state and it is **not** the
right next move: W13 is mid-surgery on exactly the interface such a trait would
have to name, and an abstraction extracted from code that is still changing
freezes the wrong shape. Order: finish W13, then extract, then make both kernels
prove the same contract. Doing it in the other order costs the extraction twice.

#### The order after W10, and why

W11–W17 are ranked by one criterion: **how much other work each unblocks per
unit of cost**, with a second look at where a serious reviewer actually pushes.
Each entry states what it costs and what it is blocked on, because a roadmap
that hides those is a wish list.

One honest caveat about the ranking itself. W11 (the split) adds no capability
and supports no new claim. It is first because every deep change after it lands
in the code it cleans. **If the near-term goal is a funding pitch rather than a
codebase**, swap W11 and W12: W12 is the only item that closes a gap in the
thesis, and W11 can wait a cycle. That is a real fork, not a hedge — pick one
deliberately.

##### W11 — Split the kernel into modules (P1)

`dezh-boot/src/main.rs` is 8,776 lines and 236 functions. The next three
workstreams are each deep surgery on the task table and the trap path, and
attempting them here is how a prototype of this quality stalls.

The file already carries 32 section banners; they are the seams. Current sizes:

| Module | Lines | Module | Lines |
| --- | --- | --- | --- |
| `console/` (the ~200-arm dispatcher) | 1,920 | `net/marz.rs` | 300 |
| `sched.rs` (tasks, IPC, mailboxes) | 1,645 | `proc/loader.rs` | 295 |
| `cairn/console.rs` | 1,340 | `mm/` (paging, frames) | 235 |
| `smp/` (HSM, per-hart trap, run queue) | 1,155 | `difc.rs` | 210 |
| `arch/entry.rs` (boot, trap, switch) | 380 | `ocap/device.rs` | 190 |
| `demos/` | 370 | `cairn/service.rs` | 145 |
| `syscall.rs` (ABI + task caps) | 200 | `ocap/ns.rs` | 120 |
| | | `dev/plic.rs`, `dev/uart.rs`, `time.rs`, `mm/bump.rs` | 215 |

Order: leaves first (`uart`, `plic`, `time`, `frames`), then single-inbound-edge
(`marz`, `loader`, `difc`, `ocap/*`), then `cairn`, `smp`, `sched`, and
`console` last — it depends on everything, so it falls out once the rest have
real interfaces.

**Status: done**, with three gaps named below. `main.rs` went from 8,776
lines to 724 across 23 commits; the kernel is now 26 modules. Every step kept
the clippy gate and all three QEMU legs green, with the smoke transcript's 26
PASS lines byte-identical throughout.

Against the acceptance criteria:

| Criterion | Outcome |
| --- | --- |
| `main.rs` is the boot sequence and nothing else | met — assembly, trap path, syscall ABI, capability bits, `kmain` |
| No file over 1,200 lines | met except `pkg.rs` (3,059) |
| All QEMU legs byte-identical | met |
| Zero bare `static mut` in the moved modules | met except `smp` (4) |
| Console dispatcher becomes a table | partial — the table carries name, capability, group and help; the handler is still a match arm |

The three gaps are real and none of them is bookkeeping. `pkg.rs` is the
virtio-block daemon; it was already its own module before W11 and was never on
the split list, so the cap catches it by accident. `smp`'s four `static mut`
carry a comment arguing they are single-threaded — that argument is precisely
what W13 has to revisit, so converting them now would settle by fiat a question
W13 needs to ask. And putting handlers in the command table needs one uniform
handler signature where the arms currently take four shapes; that is a rewrite,
not a move.

**What the work turned up**, beyond the line count:

- **`plic` is not a leaf.** It reaches into `TSTATE` and `MAX_TASKS` to wake
  drivers blocked on `sys_irq_wait`, so it had to come out *after* `sched`.
  Recorded at step 3 and acted on at step 23.
- **Moving a demo does not make state private.** `pub(crate)` is crate-wide, so
  relocating a caller changes nothing. What closes a module is giving it narrow
  accessors so demos stop reaching into state — a logic change, and its own
  commit. An earlier version of this note claimed otherwise and was wrong.
- **A const used as a match pattern is a silent trap.** If it is not in scope it
  becomes an irrefutable binding that swallows every arm below it, and the build
  succeeds. This bit the syscall dispatch twice — `sched` (step 16) and the AP
  trap path (step 21) — and only `-D warnings` caught it either time. `cargo
  fix` proposed renaming the constant to `_sys_exit`, which would have made it
  permanent. This is the single strongest argument for the W10 clippy gate.
- **Banners are not subjects.** The "cooperative multitasking scheduler" heading
  held an event ledger, an IPC/block ABI, a service registry and a block-daemon
  client. The "Cairn v1 console front-end" heading was 1,335 lines of which 130
  were Cairn. Sections had been growing by chronology.
- **An explicit import list is a measurement when it names coupling, and noise
  when it names vocabulary.** `proc::loader` opened at 31 crate-root imports with
  a falsifiable prediction that it should shrink twice; it went 31 → 19 → 7, and
  what remains is the loader's own job. `abi` and the console dispatcher got
  globs, because enumerating a shared vocabulary measures nothing.

*Cross-ISA:* RISC-V only by design — x86 is 2,598 lines across 21 files and was
never the monolith this splits.
*Cost:* large but mechanical; one module per commit. Actual: 23 commits.
*Blocked on:* nothing.
*Acceptance:* no file in `dezh-boot/src/` over 1,200 lines; `main.rs` is the
boot sequence and nothing else; all QEMU legs byte-identical; zero remaining
bare `static mut` in the moved modules.

##### W12 — A real external effect (P2)

The whole W8 argument is that Dezh attributes and reverses effects. Today every
effect it can attribute lives inside Dezh's own storage. `email.send`,
`prod.deploy` and the compensatable `api-key` are **modeled**. The repository
says so honestly in three places — and that honesty does not remove the gap.

This is the one item that closes a hole in the **thesis** rather than a
limitation beside it, and it is what a funder or a programme reviewer attacks:
*"a beautiful accounting system for effects that only exist inside your toy."*

**Scope correction, recorded so it is not underestimated again.** An in-OS
connector (git, HTTP) needs TCP, DNS and probably TLS. Dezh has ARP, ICMP and
UDP egress. That is a workstream, not a step.

The tractable design is a **host-side gateway**: a small daemon outside Dezh
that Dezh reaches over existing UDP egress. It performs the real effect — a git
commit, an HTTP call — and reports the outcome. The effect genuinely leaves the
machine, carries a declared schema and a registered compensation, and lands on
the Sand ledger like any other. The honesty boundary is stated up front: the
connector is outside the TCB, and a compromised gateway can lie about what it
did. That is a smaller and much more defensible claim than pretending the OS
speaks git.

**Status: done.** `marz-effect <dest> <verb> <arg>` performs a real git commit
on a real repository outside Dezh, over the UDP egress that already existed, and
`git.revert` undoes it. Three commits: the gateway and its standalone proof, the
daemon's request/response path, and the registered compensation.

Against the acceptance criteria:

| Criterion | Outcome |
| --- | --- |
| An effect that changes state on a real external system | met — a git commit, verified by `git`, not by Dezh's transcript |
| Recorded with its intent | met — the console opens a mission; the commit message carries `Ahd#n` so the external system holds the attribution too |
| Forecast by `sfar-plan` | met — and the forecast now names the registered compensating action rather than only counting it |
| Undone by a compensating action that really runs | met — the file is gone and history is kept |
| Reproducible in CI against a local gateway | met — `tools/ci/effect_test.py`, twelve checks |

The honesty boundary is in the gateway's own header, not a footnote: **the
gateway is outside the TCB and can lie about what it did.** Dezh proves the
request was authorized for a named destination, left on the wire, was answered,
and was recorded; and that the compensation ran. It does not prove the gateway
was honest. That is a smaller claim than "the OS speaks git" and it is the true
one.

Two things the work turned up:

- **"Not recorded" and "did not happen" are different.** The first end-to-end
  run had an off-by-ten in the reply parser (`rx_wait` already steps past the
  virtio header; the new parser added it again). The gateway committed and Dezh
  saw nothing. Refusing to record an unobserved effect is right, but the
  external system had still changed — which is the exact failure this workstream
  exists to make visible, arrived at by accident.
- **An effect record needs the undo, not just the class.** `sfar-plan` reported
  `compensatable=1` while naming no compensation, which is a promise rather than
  a plan. The daemon already persisted a registered compensation; the forecast
  simply never printed it. It does now, and says so explicitly when one is
  missing.

Still modeled, and still labelled as such: `email.send` and `prod.deploy`. What
changed is that the ledger now holds at least one effect that is not.

*Cross-ISA:* RISC-V first. The connector is arch-neutral but the ledger it
records into is Cairn, which x86 has no disk for.
*Cost:* medium. Effect schema, one connector, compensation registration, and the
`marz` request/response path (which already receives).
*Blocked on:* nothing — UDP egress and the ICMP receive path exist.
*Acceptance:* an effect that changes state on a real external system, recorded
with its intent, forecast by `sfar-plan`, and undone by a registered
compensating action that also really runs — reproducible in CI against a local
gateway process.

##### W13 — One scheduler across all harts (P3)

W9's own closing note. Tasks on secondary harts run to completion — no
preemption, no migration, no timer armed there — and the console's scheduler,
task table, IPC mailboxes and frame allocator are still single-hart and not
under the lock. Merging them into one lock-protected structure, so every task in
the system (daemons included) is dispatchable on any hart, is what moves Dezh
from "several convincing demos" to "an operating system".

*Cross-ISA:* RISC-V first, and the most expensive entry in that column — x86 has
no AP bring-up and no `Blocked` state, so this is a re-derivation, not a port.
See the extraction note above.
*Cost:* large, and genuinely hard — this is real concurrency work.
*Blocked on:* W11 in practice; the tables must be modules with owners first.
*Acceptance:* a daemon migrates between harts under load; a task on a secondary
is preempted by that hart's own timer; `smp-*` and every existing demo unchanged.

**Step 1 — done: a secondary hart's own timer.** `ap_execute` arms the timer and
sets `sie.STIE` around the U-mode window, and the per-hart trap path services the
tick and resumes. `smp-preempt` is the evidence and is deliberately narrow: it
counts ticks for the *specific* hart the task ran on, and prints `INCONCLUSIVE`
rather than success if that hart was the boot hart. The negative control was run
— with the arming line removed the same demo reports zero ticks and `FAILED`.
Asserted in `tools/ci/qemu_smoke.py`.

This buys the second half of the acceptance and none of the first. The tick
resumes the interrupted task; it does not choose another, because choosing means
reading a task table that is still `Global<T>` on the boot hart with no lock.

**Step 2 — done: the tables are private, and the reachable surface is locked.**
`sync::TicketLock` is one lock for the kernel and masks the acquiring hart's
interrupts, because `plic_handle` writes scheduler state from interrupt context.
The task table is private to `sched`; the five accessors other modules call and
`wake_irq_waiters` take the lock.

**Step 3a — done: the scheduler entry is lock-safe.** `schedule_or_return` and
`idle_until_device` take the lock in scopes rather than across the sleep, since
the sleep services the PLIC and reaches the same lock.

**Step 3b — next, and it needs a decision before it needs code.** What is left
is `utrap_handler`: 280 lines, ~35 table accesses, 29 return points, 8 of which
call `schedule_or_return` — which now takes the lock itself, so a guard held
across the handler would deadlock on them.

Two shapes, and the obvious one is wrong:

- *One lock across the syscall dispatch.* Mechanical to write, and it puts
  `SYS_PRINT` — a byte-at-a-time UART write — inside a critical section with
  this hart's interrupts masked. A long line would hold off every device
  interrupt on the hart for the length of the print. Correct and unusable.
- *Fine-grained, one critical section per table touch.* Keeps the sections
  short, but a syscall stops being atomic against another hart: read `TSTATE`,
  release, act on a value that has changed. Which of those reads actually need
  to be atomic together is the design question, and it is answerable — the
  syscall paths that matter are IPC send/receive and the capability checks.

**Step 3b — done.** The atomic unit is one syscall's table work. `SYS_SEND`,
`SYS_RECV`/`_TIMEOUT` and `SYS_IRQ_WAIT` each take one section; the rest are
short reads. `SYS_PRINT` turned out to touch no table at all, so the argument
against a coarse lock was aimed at the wrong line. Guard placement is checked by
a script that walks brace depth, because the failure mode is a hang.

**Step 3c — the trap-path merge, and the crux.** Two routes, both real:

- *The boot hart becomes per-hart.* `ktrap_stack` and `KCTX` are singletons and
  a second hart entering `utrap` clobbers both. Fixing it means widening the
  saved frame past its 32 slots (index 31 is `sepc`, all are used) so each
  dispatch can record the running hart's stack and context, then changing
  `utrap`, `run_first`, `enter_user` and `restore_kernel_ctx`. High risk: it
  edits the proven trap path.
- *The AP adopts the real handler.* `smp` already has per-hart trap state
  (`ApCtx` via `sscratch`) and its own kernel context, but `ap_trap_handler` is
  74 lines serving two syscalls against `utrap_handler`'s 348. Lower risk,
  because the boot path is untouched.

Both meet the same wall: `restore_kernel_ctx` is wired to one saved context, so
no hart but the boot hart can return from `schedule_or_return`. Choosing the
right context per hart starts with a hart being able to ask which it is — and
until now the boot hart was the one that could not, because `_start` never set
`tp`. It does now (`smp::current_hart`), verified by a boot-time check against
the id SBI passes, with a negative control: remove the register write and `tp`
reads as garbage and the kernel refuses to continue.

`current_hart` is kernel-context only — **was**. Inside a U-mode trap the task
owns every register including `tp`, so the handler read whatever the task left
behind. That is now closed from the other side: the saved frame carries a slot
33 (`F_HART`) that the dispatching hart stamps with its own id, and `utrap`
loads it into `tp` on the way in. Both first-dispatch paths stamp it too, and
because it is written every time a task is chosen, it survives migration by
construction.

Every trap now checks the restored identity against the hart that dispatches
tasks and halts on a mismatch, since a wrong answer would send a hart at another
hart's per-hart state — a corruption rather than a crash. Negative control:
remove the load and the handler reports hart 0 while the boot hart is 2.

`KCTX` and `ktrap_stack` are per-hart now, indexed by `tp` in all four places
that touch them. Verified at non-zero indices — runs landing on boot harts 1, 2
and 3 exercise `KCTX[1..3]` — but **not** in the case the split exists for: a
control pointing every hart back at hart 0's stack still passes, because nothing
puts two harts in a trap at once yet.

**What is left, and the constraint that shapes it.** A secondary hart cannot run
just any task. `set_active_task_mem` flips `PTE_U` in the *shared* kernel page
table so exactly one task's stack is reachable from U-mode — one global view. Two
harts running two such tasks would race, the last writer would win, and the
loser's task would fault on its own stack: corruption, not a crash. Only tasks
with a private `satp` are free of it, which is why `smp` already builds one per
AP slot.

That is now enforced in `schedule_or_return` rather than left as a comment: a
hart other than the boot hart picking a task that shares the kernel address
space halts the kernel. The guard costs nothing today, because only the boot
hart dispatches — it is there so the piece that changes that cannot land
quietly wrong.

`CURRENT` is per-hart now too, and it was the last singleton in the dispatch
path. It carries two jobs — whose syscall `utrap_handler` is serving, and where
`pick_next` resumes its round-robin — and one cell for both across two harts
would charge a syscall to the wrong task's capability set. That is an authority
bug, not a lost tick, which is why it moves before anything starts dispatching.
Reads and writes go through one accessor pair so the hart index cannot be
dropped at one of the seven sites.

Indexing by `tp` is now bounded, once, where the boot hart's identity is already
checked: three tables (`KCTX`, `ktrap_stack`, `CURRENT`) are indexed by it with
no check at the use site, and two of those indexings are in assembly where a
check is not available. Negative control: with `MAX_HARTS` temporarily at 2, the
runs QEMU lands on harts 2 and 3 print the FATAL and halt while harts 0 and 1
boot normally — the guard fires exactly at the boundary and nowhere else.

And two harts can no longer pick the same task. `Ready` means runnable, not
idle — a task stays `Ready` for the whole time it runs — so `pick_next` would
have handed the same slot to both, and the second hart would have resumed from a
register frame the first was still saving into. The claim is the `CURRENT` entry
itself rather than a `Running` state or a second table: one cell cannot disagree
with itself, and a `Running` state would have to be got right by all eleven
places that write `TaskState`, none of which are about this. `NO_TASK` is the
other half — a hart on the console holds no claim, and the claim is dropped
inside the same locked section that reads claims, on the way out through
`restore_kernel_ctx`, because that path never returns.

Negative control, and this one does exercise the case the mechanism exists for:
a phantom claim on task 1, planted at run entry as if a second hart held it,
makes task 1 undispatchable — `ipc-typed-demo` reaches the console, prints its
banner and then never prints `PING -> 0`, because the server task can no longer
be chosen. Every leg before it is unaffected. Remove the claim and the same run
is green.

The run entries are closed too, which was the gap named here a commit ago. They
built the table and took the first claim unlocked, and step 2's note said why:
the ticket lock is not reentrant and `reclaim_task_resources` is called both
from outside this module and from within it. So it splits — a public wrapper
that locks, a `_locked` inner for callers that already hold it — and the four
entries now hold the lock across their table setup and their first claim, and
drop it before `run_first`, which never returns to them.

`build_address_space` stays outside on purpose. It loads an ELF and walks page
tables, and this lock masks the hart's interrupts, so a section that long would
hold off every device interrupt for the length of a program load. It touches the
frame allocator, not the table, so the unit stays one task's row at a time — the
same unit step 3b chose for syscalls.

Getting guard placement wrong hangs the hart instead of crashing it, and CI would
show only a QEMU timeout. So it is checked rather than reviewed:
`tools/ci/check_sched_lock.py` walks brace depth, computes which functions take
the lock transitively, and fails if any call inside a guard reaches one. Negative
control: a `task_state()` call planted inside the `run_tasks` guard is reported
by file and line, naming both the callee and the guard it sits in.

**The last shared write is gone too.** `set_active_task_mem` was called on every
pick, and it writes `PTE_U` into the one L1 that the kernel root *and* every
process root point at. That is the state a second hart in `schedule_or_return`
would have raced on — last writer wins, and the losing hart's baked task faults
on its own stack. It is now called only when the picked task shares the kernel
address space, which is the only case that needs it.

The same edit closes something that was already true: calling it for a loaded
process wrote `PTE_U` onto baked stack region `i` inside that process's address
space, exposing 2 MiB of kernel RAM for as long as the process ran. No run mixes
baked tasks with processes — `run_tasks` wipes every slot to baked, `run_processes`
wipes every slot to loaded, and the daemon at slot 0 is a loaded process — so the
region held no task's data and nothing was leaked. It was slack, and it is closed.

Negative control: invert the condition, so the call is made for processes and
skipped for baked tasks, and `ipc-typed-demo` never reaches `PING -> 0` — the
baked tasks fault on stacks that are no longer mapped for U-mode. The call is
load-bearing exactly where it was kept.

**The trap guard now names the invariant, not the boot hart.** It read
`current_hart() != BOOT_HART`, which was true only because the boot hart was the
only dispatcher — a secondary joining would have had to weaken the check that
exists to catch exactly that hart being wrong. The property that has to hold is
that the trapping hart *holds a claim*: a trap from U-mode means it is running a
task, so `CURRENT[hart]` must name that task. A restored `tp` pointing at another
hart fails that for free, since that hart's claim is either `NO_TASK` or some
other task, and the check keeps holding once a second hart dispatches — with no
list of permitted harts to maintain beside the claim it would duplicate. `tp` is
bounded against `MAX_HARTS` first, because it is the subscript.

Negative control, the same one that proved the stamp: delete the `ld tp, 256(sp)`
that restores kernel identity in `utrap`, and the runs QEMU lands on harts 1 and 3
print `FATAL: trap on hart 0 which holds no task` and halt, while runs on hart 0
pass — there the wrong answer and the right one coincide. The guard fires exactly
when the identity is actually wrong.

**The address-space rule became a filter instead of a halt.** It had been a
guard in `schedule_or_return` that stops the kernel when a secondary picks a task
sharing the kernel address space — right for a rule nothing was meant to reach,
useless for a secondary that has to keep going. `pick_next` now skips such a task
and looks at the next one, and the halt stays as a backstop for a task arriving at
dispatch by some path that did not come through the filter. Negative control:
invert the predicate so the boot hart is the one refused a baked task, and
`ipc-typed-demo` never reaches `PING -> 0` — the filter is what chooses, not
decoration next to the choice.

**The merge was attempted, and it has a defect. Here is exactly what is known.**
A `secondary_serve` was written — pick under the lock honouring claims and the
address-space filter, install `stvec = utrap`, SUM, the task's satp and this
hart's own timer, `run_first`, and undo all of it on the way back — plus a
`CONSOLE_SMP_ON` switch (off by default, because W13's acceptance requires every
existing demo to be unchanged) and an `smp-console` demo. It is **not** in the
tree, because it hangs. What was measured before backing it out:

- With no prior demo, it works, five runs out of five: three loaded processes,
  three different harts, clean exit, `MERGED-OK`. A console task really does run
  on a secondary through `utrap` — all 348 lines of it — not the AP path's
  74-line handler.
- After any demo that has run a **U-mode task on a secondary via the AP path**
  (`smp-task`, `smp-sched`, sometimes `smp-preempt`), the next `smp-console`
  wedges. `smp-demo`, which runs no U-mode task, does not poison it.
- The hang is in the boot hart's `run_processes`, between installing the trap
  vector and returning — the first task never prints.
- Replacing the U-mode entry with a pick-and-release, so a secondary claims a
  task and never `sret`s, is clean in every case. **The defect is in a secondary
  entering U-mode on the console trap path, not in the pick.**
- Not the secondary's timer: the hang survives with `sie.STIE` left clear.
- Not the UART lock: the hang survives with the macros not taking it.
- Not `run_processes` itself: after `smp-task`, the existing `procs` command —
  the same entry with the switch off — is fine.

The leading suspicion, unproven, is per-hart CSR state the AP path leaves behind
and `secondary_serve` does not reset — `sscratch` is the one both trap paths use
for different structures, and `frame_restore` only sets it at the very end of
`run_first`. A trap taken in the window between `csrw stvec, utrap` and that
store would enter `utrap` with the AP path's `sscratch` and save a console task's
registers into an `ApCtx`. Proving or refuting that is the next step's first job.

So the last piece is: let a secondary pull from the console task table, limited
to tasks with their own address space, and then make a daemon migrate under
load. Everything under it is in place — identity in both contexts, per-hart
context, stack and current task, the table private and locked on every path
including entry, the run claim enforced, syscalls atomic per call — and the
entry itself is written and known to work from a cold console. What is left is
one defect with a bounded search space, not a design question.

Migration needs one more thing the demo made obvious: a hart keeps its claim
across preemption, and with as many harts as runnable tasks it re-picks its own.
A task moves hart only after it **blocks** and is woken, so the daemon — which
blocks on `sys_irq_wait` — is the case that shows it, and the loop tasks never
will.

##### W14 — Object-capabilities as the live substrate (P4)

`docs/STATUS.md` calls this "the single largest planned change", and it is still
ahead of us. Two steps landed before W10 — the Cairn namespace gate and the
device gate are real `dezh_core::ocap` tables with generation-stamped
revocation, proven at runtime by `nsrevoke-demo`, `agentrevoke-demo` and
`dev-demo`. W10 only changed how those statics are *stored*; it moved the
migration forward by nothing, and it would be easy to misread the diff as
progress here.

What remains is the substrate itself: the **per-task capability bitmask**. It is
kernel-attested on every IPC message and attenuable on delegation — so not
Linux-style ambient authority — but it is a bit per class, not an unforgeable
per-object reference in the seL4/CHERI sense. The ocap tables today are a gate
layered above it, not the thing authority is made of.

*Cross-ISA:* shared, and this is the strongest candidate for it — `ocap` already
lives in `dezh-core`, and x86 already derives authority through `mcap`.
*Cost:* large; it touches every syscall check and the IPC attestation path.
*Blocked on:* W11 and W13 (the task table is the thing being changed).
*Acceptance:* a task holds object handles, not a bitmask; delegation is a graph
edge; `redteam`'s forgery escape still fails, now against a generation check.

##### W15 — Edition 2024 and the rest of the kernel's tests (P5)

**The edition half is done.** Every live `Cargo.toml` is on edition 2024 — the
two shared crates, both kernels, the nine embedded user programs, the three wasm
guests and the eight superseded spikes — with no `#[allow]` added to get there.
The only 2021 left in the tree is inside `dist/`, a published release snapshot
rather than source.

It was done by reading rather than by `cargo fix --edition`, for the reason
recorded at W11: the last time that tool was pointed at this repository it
proposed renaming a constant to `_sys_exit`, which would have made a match-arm
bug permanent and silent.

Three things it turned up that were not bookkeeping:

- **`unsafe_op_in_unsafe_fn`, denied an edition early**, named all 99 sites while
  both kernels still built either way. Wrapping the bodies produced exactly one
  `unnecessary unsafe` in return, and that one is a finding: `BumpHeap::alloc`
  performs no unsafe operation at all — `UnsafeCell::get` is safe and so is every
  atomic under it. The `unsafe` on that signature is `GlobalAlloc`'s contract
  with its caller, not the body's with the hardware.
- **`gen` is a reserved word now**, and both `dezh_core::ocap` and the
  `virtio-blk` daemon used it. The rename is where the only real hazard was: a
  whole-word pass also rewrote `sys_print(b" gen=")` into `b" generation="` — a
  string literal, in output CI asserts against. Caught and reverted. Renaming an
  identifier and renaming everything spelled like one are different operations.
- **A workspace edition bump can break a crate the workspace does not list.** The
  `guests/` wasm crates are built by `dezh-host`'s build script, so their errors
  arrived as that script exiting 101 rather than as a compile failure anyone
  could read.

What remains under W15 is the second half: the tests. The measured list is
unchanged — Cairn commit-record encode/decode, the **255-slot boundary with no
GC**, the ticket lock's arithmetic, run-queue push/pop under simulated
interleaving, and the Marz checksum. All five need `dezh-boot` to be host-
testable first, which it is not: it is a `no_std`, `no_main` binary crate for one
target. That is the next piece of work here, and it is a structural change rather
than a mechanical one.

The measured remainder: 81 attribute conversions and 65 `unsafe fn` bodies to
wrap. Both are mechanical and both are much easier to review once W11 has split
the trap and boot paths into their own files. The second half of W10.4 rides
along here: Cairn commit-record encode/decode, the **255-slot boundary with no
GC** (currently untested), the ticket lock's arithmetic, run-queue push/pop under
simulated interleaving, and the Marz checksum — whose odd-length-body bug was
found by hand in W9 and is exactly what a three-line test catches.

*Cross-ISA:* both, separately, and x86 is the cheaper half — it is on edition
2021 like the rest of the workspace, but it carries zero `static mut` against
RISC-V's 16, so only the attribute conversions apply to it.
*Cost:* medium, entirely mechanical.
*Blocked on:* W11.
*Acceptance:* every `Cargo.toml` on edition 2024 with no new `#[allow]`;
`cargo test` inside `dezh-boot` runs in CI.

##### W16 — x86_64 to system parity (P6)

Roughly 900 lines against 12,359. F3 proves the *program format* is portable; it
does not prove the system is. Until x86 has a runtime, "ISA is an implementation
backend, not the identity" (D021) is a RISC-V thesis.

**W16.1 through W16.4 are done:** a 256-vector IDT, a Local APIC timer measured
against the PIT and armed at 100 Hz, an interrupt entry path that saves and
restores every general-purpose register, a round-robin scheduler over kernel
tasks that never yield, per-task address spaces, ring 3 — a CPL3 task that
touches memory it was not given is killed alone while its neighbour finishes —
and capability-checked syscalls whose grants are `requested ∩ ceiling` through
the shared `dezh_core::mcap`. All asserted in CI on both x86 boot paths. Still
missing: no disk, no drivers, and no effect ledger, so an x86 task's authority
is derived but its effects are not yet accounted for.

This is also **the only practical route to an IOMMU** — see W17.

*Cross-ISA:* this workstream **is** the cross-ISA debt. Everything in the
"RISC-V first" column above is owed here.
*Cost:* very large. Timer and returnable IRQ path (done, W16.1), scheduler
(done, W16.2), paging and ring-3 containment (done, W16.3), intent-derived
capability checks (done, W16.4), then a virtio-pci disk driver, then Cairn and
the effect ledger.
*Blocked on:* nothing technically; competes with everything for time.
*Acceptance:* the x86 kernel runs the console, the scheduler and Cairn; the same
`.dzp` installs and persists on both ISAs.

##### W17 — IOMMU-enforced DMA isolation (P7)

The most-attacked gap, and deliberately last, because it is **blocked rather
than hard**. The investigation, recorded so it is not repeated:

- **RISC-V: two prerequisites, neither about DMA.** QEMU 8.2 (the version CI and
  the dev containers use) models no `riscv-iommu` device at all — only
  `virtio-iommu`; the RISC-V IOMMU landed in QEMU 9.1. And every IOMMU in QEMU
  sits on the **PCI** root complex, while Dezh drives legacy **virtio-mmio**
  (`VIRTIO_BLK_MMIO_PA = 0x1000_1000`). Reaching an IOMMU here means first
  migrating the block and NIC drivers to virtio-pci with MSI-X — a full
  workstream that buys nothing on its own.
- **x86: the hardware is there, the system is not.** `intel-iommu` (VT-d) and
  `amd-iommu` are available and mature even on QEMU 8.2. But with no scheduler,
  no disk and no drivers, **there is no DMA to protect.** An IOMMU on x86 today
  means writing translation tables for a device that does not exist.

So it is a leaf of the dependency tree, and the route is through W16, where VT-d
is waiting with no QEMU upgrade required.

**A note on how this gap is weighted.** It is the first thing a systems audience
attacks, and the repository already concedes it by name in `STATUS.md`, the
threat model, and the comparison matrix — which a serious reviewer accepts. What
they do not accept is a claim that was never true. W12 closes a gap of that
second kind, which is why it ranks above this one despite being less famous.

*Cross-ISA:* x86 first, uniquely — VT-d is mature on QEMU 8.2 while the RISC-V
IOMMU needs QEMU 9.1 and a virtio-pci migration.
*Cost:* large, after a larger prerequisite.
*Blocked on:* W16 (x86) or a virtio-pci migration plus QEMU 9.1+ (RISC-V).
*Acceptance:* the block daemon's DMA is confined by hardware, and a deliberately
malicious driver programming the device to write outside its window is stopped
by the IOMMU rather than by convention.

Post-MVP horizon (recorded, deliberately not started in W8): explicit system
generations / time-travel, multi-agent attenuated sub-delegation with
provenance chains, full saga/compensation for external effects, human-approval
gates for sensitive intents, cross-ISA effect-semantics identity, and
non-storage typed effects (network/service/install). See
`docs/ROADMAP.md#strategic-direction`.

### Beyond W17

Everything with a named workstream above has an owner, a cost and an acceptance
test. What is listed here does not yet, and saying so is the point - these are
directions, not commitments.

Three items that used to live here have graduated and should not be re-listed:
intent leases and revocation shipped in W8 (`lease-demo`), signed package
manifests shipped as the `DZSP` envelope (`sig-demo`), and IOMMU-backed DMA
isolation is now W17 with its blockers written down.

- Convert the remaining embedded demo apps into separate ELF services.
- A richer app lifecycle: staged rollout, audit queries over the ledger.
- Per-client block queues and real storage concurrency (today one daemon
  serialises every request).
- Reusable typed service interface definitions, so a service contract is
  declared once rather than hand-matched on both sides.
- ARM bring-up as a third ISA - but only after W16, and only if a third backend
  would teach something the second did not.
- Production boot media and an installer flow.
- A capability-aware GUI / compositor boundary.
- Measured boot, and a root-signed trust store loaded from disk with key
  rotation (today it is kernel-embedded - see the signing limitation in STATUS).
- Formal verification of the smallest kernel authority rules. The authority rule
  is already machine-checked by exhaustive enumeration in `dezh-kernel`; this
  would be the real thing, and it is honestly a research project.
- Ledger integrity against a *malicious* storage daemon. Records are
  parent-linked and hashed for corruption detection, not signed; today the
  daemon that owns the disk is trusted. The commit log is also a fixed 255 slots
  with no GC.

### Non-Goals For MVP

- Claiming production readiness.
- Replacing an existing general-purpose OS.
- Full POSIX compatibility (small measured subset only).
- Full package ecosystem.
- Real-hardware driver support (VM targets only).
- Production cryptographic supply-chain infrastructure.

---

## Strategic direction

<!-- was docs/STRATEGIC_DIRECTION.md until the 2026-07-23 consolidation -->

### Position

Dezh should not be framed as a cleaner copy of existing operating-system ideas.
The long-term thesis is stronger:

**Dezh is an intent-native, effect-accountable operating-system prototype.**

The goal is not just to combine a microkernel, capability security, user-space
drivers, package rollback, and service supervision. Those are necessary
building blocks, but they are not the differentiator by themselves.

The differentiator should be that Dezh treats **intent** and **effect** as
first-class OS concepts.

### The Ground We Own (D021)

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

#### Value Is Only Visible Against An Adversary

A secure system that is never attacked is just an assertion. The proving demo
must carry a **villain**: an agent that actively *tries to escape* its intent —
read another namespace, write raw device MMIO, forge or amplify a capability,
act outside its declared intent, monopolize the CPU — and is stopped at a named
boundary each time, with `why-denied` explaining it. Happy-path demos (an app
acting inside its grant) do not make the value visible; the escape that fails
does.

#### The Mission Is The Reversible Unit

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
`docs/SECURITY_MODEL.md#threat-model` states what Dezh's trusted base is, what it defends, and
what it explicitly does not defend (side channels, a malicious kernel, hardware,
no-IOMMU DMA).

### Why This Matters

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

### Core Thesis

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

### Competitive Advantages To Build Toward

#### Intent-Scoped Authority

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

#### Effect Ledger

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

#### Reversible OS Boundary

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

#### No Ambient Continuity

State should not silently carry forward forever.

Dezh should make generations explicit:

- boot generation
- service graph generation
- package generation
- namespace generation
- intent/effect generation

Rollback and audit should be generation-aware.

#### Explainable Denial

"Permission denied" is not enough.

Dezh should explain:

- which intent was denied
- which capability was missing or too broad
- which component requested it
- which safer route is available
- whether review, migration, or explicit override is required

#### Agent-Ready Without Blind Trust

Future systems will run more agents and automation.

Dezh should be designed so agents can operate productively without receiving
ambient authority:

- intent-scoped capability grants
- bounded namespaces
- structured effect ledger
- review gates for sensitive changes
- rollback/compensation where possible
- denial explanations that can guide safer retries

### Honest Novelty Accounting (D015)

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

### Architectural Guardrails

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

### Relationship To The MVP (D019)

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

### Near-Term Milestones

These are now consolidated as roadmap **W8 (Intent + Effect Runtime)**, the one
workstream that turns D020/D021 from prose into a demonstrated differentiator.
W8 is not "add a feature"; it is the feature plus the three things that make its
value legible to a skeptical practitioner audience — an adversary, a
whole-mission rollback with an honest irreversible effect, and an owned cost.

#### 1. Intent as mechanism (Ahd)

- `intent-open <kind>` issues an **Ahd** (an intent token: a ceiling of
  capabilities for a target namespace), `intent-run <ahd> <app>` runs an app
  whose derived capability is proven ⊆ the Ahd, `intent-list` enumerates open
  Ahds.
- Manifest grants (W1) become Ahd-derived; a request for authority beyond the
  Ahd is denied. This rides the existing IPC attenuation and per-task
  capability bits.

#### 2. Effect ledger on Cairn (Sand) — built (W8 P2)

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

#### 3. Mission (Sfar) + whole-mission rollback + honest external effect

- A **Sfar** groups the effects under one Ahd; `effect-rollback <sfar>` undoes
  them atomically; `effect-rollback <id>` undoes one.
- At least one `irreversible` external effect (simulated network/print) that
  rollback **refuses with an explanation**, and one `compensatable` effect with
  a registered compensation action.

#### 4. The adversary

- A `redteam` scenario: a malicious agent that attempts cross-namespace reads,
  raw MMIO writes, capability forgery/amplification, out-of-intent actions, and
  CPU monopoly — each stopped at a named boundary (page fault / capability check
  / intent bound / preemption) with `why-denied`.

#### 5. Explainable denial + provenance

- `why-denied <last|id>`, `cap-tree` / `cap-audit` / `component-info`, and
  **Tbar**, a queryable `actor → intent → effect` provenance graph
  ("everything this agent touched and why").

#### 6. Credibility layer

- **Cost:** the per-effect ledger overhead measured and folded into
  `BENCH.md` (D015).
- **Head-to-head:** a documented scenario where gVisor / Firecracker /
  wasmtime cannot cleanly undo a whole mission but Dezh can (Dezh's side
  reproducible in CI even if the competitor is only described).
- **`docs/SECURITY_MODEL.md#threat-model`:** trusted base, what is defended, and what is
  explicitly not defended.

#### 7. One flagship narrative

All of the above collapse into a single story — "leave a coding agent loose on
your machine overnight" — with a transcript and a CI smoke leg. This is the
final form of the F1 (D020) agent-containment demo, not a separate demo.

The first implementation maps a small set of intents onto existing package,
storage, and service operations, with the ledger stored in Cairn.

#### 2. Capability Attestation v1 (rides along W3)

Make authority explainable at runtime.

Candidate commands:

- `cap-audit`
- `cap-tree`
- `why-denied`
- `component-info <id>`

#### 3. App Storage Namespace + Migration v0 (post-MVP)

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

#### 4. Dezh Tooling MCPs

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

### Review Outcome (2026-07-04)

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

# Status and honest limitations

One page, no spin. Dezh is a research prototype that demonstrates an
architecture; it is not a production OS. This is exactly what is and is not
true today, so a reviewer never has to guess.

## What genuinely works (in CI, reproducible)

| Area | State |
| --- | --- |
| No-ambient-authority thesis | Enforced at the syscall boundary **and** by hardware paging (U-mode faults on ungranted memory/MMIO). |
| F1 — agent containment | Agent app runs in-grant, is DENIED by the kernel beyond it, delegates an attenuated cap over IPC, and its writes are rolled back. |
| F2 — Cairn v1 storage | Commit-log store: commit, snapshot, roll back, verify; survives reboot; cross-namespace access denied by kernel-attested caps. |
| F3 — multi-ISA | The same Dezh-IR bytecode runs on the RISC-V and x86_64 kernels; the bytes are pinned byte-identical by a test; x86 runs it as a real `.dzp` package. |
| F4 — Pol (Linux personality) | A real, unmodified static Linux/RISC-V ELF runs under a capability-gated Linux syscall shim; the same bytes also run on real riscv64 Linux. |
| x86_64 boot | Boots via QEMU `-kernel` (PVH) and from a GRUB Multiboot2 ISO in QEMU **and VirtualBox**; a 32-vector exception IDT reports faults instead of triple-faulting. |
| x86_64 returnable interrupts | A 256-vector IDT: exceptions still end in a reported halt, while vectors 32..255 save every general-purpose register, dispatch, restore, and `iretq`. A Local APIC timer is armed at 100 Hz from a rate **measured** against PIT channel 2 (~999 MHz APIC bus under QEMU, printed as counted). Proof is a work loop that keeps summing 1..=1000 across the ticks with no round corrupted, and a tick count that freezes when the timer is masked while that loop runs on. No device IRQs on x86 yet. |
| x86_64 preemption | The same entry path can decline to resume what it interrupted: the dispatcher is handed the interrupted `rsp` and its return value is loaded back into `rsp`, so a saved 22-qword frame *is* a task. Three kernel tasks containing no yield of any kind are round-robined with the boot task, one tick per turn, each still checking its arithmetic; asserted in CI by **turns granted** (9/8/8 in both debug and release) rather than work completed, since only the interrupt handler can grant a turn. |
| x86_64 isolation | Per-task address spaces (own `cr3`, kernel entries shared and USER-free) and real ring 3: a GDT with user descriptors, a TSS whose `rsp0` follows the running task, and exactly one DPL3 IDT gate (`0x80`) as the way in. Two CPL3 tasks run programs copied into pages of their own — a kernel Rust function is unreachable from ring 3 by construction, since `.text` is mapped but never USER. One of them reads an address it was not given: it is **killed alone**, with `cr2`/`rip`/error reported, while its neighbour keeps making syscalls and exits normally. Asserted in CI, including the `cs` the CPU saved (`0x23`), which a task cannot forge. Still missing: nothing frees a dead task's pages, one CPU. |
| x86_64 derived authority | An x86 task's syscalls are capability-checked, and the capability is **derived from an intent ceiling**, not held by default: `granted = requested ∩ ceiling`, computed by `dezh_core::mcap` — the same function the RISC-V kernel calls and the one its exhaustive test pins, not a second implementation. Two CPL3 tasks run byte-identical code from a byte-identical manifest under different ceilings; the narrow one is refused `print` **that its own manifest requested**, by name (`DENIED: task 7 holds no PRINT capability`), and reports back which of the two calls it was allowed. A refusal is a return value, not a fault. Honest scope: there is no `Ahd` token, no `Sand` ledger and no mission on x86 — the ceiling is a number the boot code passes in, so this is the derivation rule and the denial, not the accounting built on them. |
| Drivers out of kernel | virtio-block is a U-mode daemon holding an explicit MMIO + DMA grant; clients reach it only over typed IPC. **Caveat (not buried):** without an IOMMU this gives fault isolation + least privilege of the driver *process*, not memory safety against a malicious driver that programs the device to DMA anywhere. The IOMMU is core to this story, not future polish. |
| W8 — intent → effect runtime | An agent runs under one **intent** (`Ahd`); its derived capability is provably ⊆ the intent. Every effect is a ledger record (`Sand`) carrying `actor → intent → derived cap → reversibility`. A whole **mission** (`Sfar`) is rolled back honestly: reversible effects retracted, compensatable effects undone by a **recorded** compensating action, irreversible effects **refused with a reason** — and rollback needs authority over every namespace the mission touched. A five-escape adversary (`redteam`) is stopped at five named boundaries; `why-denied` names the boundary of the last denial; `Tbar` renders the `actor → intent → effect` provenance graph. The `overnight` flagship runs the whole story. |
| W12 — an effect that really leaves | Until W12 every effect Dezh could attribute lived inside Dezh's own storage, so the ledger was checked against itself. `marz-effect` now drives a real external system: the request is authorized (NIC capability live, egress authority held for that named destination, DIFC export rule allows it), ARP-resolved, sent on the wire, and the **outcome comes back** and is ledgered — as `compensatable`, with the undo recorded *on* the effect, so `sfar-plan` can name the compensating action instead of promising one. The reply **lowers operator integrity**, because bytes off the wire are attacker-chosen. `tools/ci/effect_test.py` is the acceptance and it does **not** believe Dezh's transcript: all twelve checks read the external system's own state, including that a revoked NIC capability leaves it untouched. **Boundary:** the host gateway is *not* in Dezh's TCB — a compromised gateway can lie about what it did, and Dezh proves only the parts it owns. |
| Device interrupts | The kernel is interrupt-driven, not polled: a PLIC routes virtio IRQs to the boot hart's S-mode context, drivers **sleep** on `sys_irq_wait` and are woken by the device, and the scheduler idles (`wfi`) for a device when nothing else is runnable (`irq-stat`). |
| SMP bring-up | Secondary harts are started through the real **SBI HSM** protocol, each with its own stack and identity (`tp` = hart id); a parallel round proves >1 hart executes concurrently on coherent shared memory (`smp-demo`, and asserted at boot under `-smp 4`). The boot hart is chosen by firmware and is **not** assumed to be hart 0. |
| SMP mutual exclusion | The kernel has a fair **ticket spinlock**. All harts hammer a non-atomic counter under it and the total is exact (`MUTEX-OK`) — proof the lock works, which atomics cannot show. This is the primitive symmetric scheduling is built on. |
| SMP shared run queue | The core of a symmetric scheduler: 48 jobs on ONE queue, drained concurrently by every hart under the lock, each item running **exactly once** (`QUEUE-OK`) — none lost, none double-run. |
| U-mode task on a secondary hart | A real U-mode task is dispatched onto a secondary hart, drops to U-mode via a per-hart trap path, has its syscalls serviced **on that hart**, and runs to completion while the boot hart stays on the console (`smp-task`, `U-MODE-ON-AP`). |
| Symmetric scheduling | One task queue, every hart pulling from it: tasks land wherever a hart is free and several run in U-mode **at the same instant** (`smp-sched` — 4 tasks across 3 harts, each exactly once, peak 3 live → `SCHED-OK`). Per-hart trap state is reached via `sscratch`, so harts can trap simultaneously. |
| Isolation under parallelism | Each task gets its **own address space** (only its stack region is U-mapped), so concurrent tasks on different harts cannot reach each other's memory: an intruder page-faults and dies on its own hart while its neighbour runs on (`smp-isolate`, `ISOLATION-OK`). |
| Information flow, both directions | Secrecy **and** integrity are enforced on the live storage path. Reading a labelled namespace raises secrecy so a secret cannot be written down or exported (`taintflow-demo`); consuming **network input** lowers integrity so unvalidated bytes cannot become trusted state (`ingress-demo`, `INGRESS-OK`). The escapes are explicit, privileged and recorded: `declassify` for secrecy, `endorse` for integrity — and neither grants the other. |
| Bidirectional networking | The Marz daemon **receives**, not just transmits: it offers the NIC receive buffers, blocks on the device interrupt, resolves the destination by **ARP**, and completes a real **ICMP echo** exchange, matching the reply by id and sequence (`marz-ping`, `NET-RX-OK`). CI decodes the packet capture structurally and asserts the echo left and the reply came back. |
| Engineering baseline (W10) | Every tree lints with `-D warnings` on a **pinned** toolchain, so a regression cannot land quietly and "green locally" cannot disagree with CI. No reference to a `static mut` survives anywhere in the kernel — the four clusters that had them (device authority, namespace authority, information flow, the event ring) now go through a pointer, which matters because a `&mut` to a static two harts can reach is UB and secondary harts run real tasks. The superseded Step 1..9 prototypes moved to `spikes/`, off the shipping path. Manifest capability derivation — the narrowest security decision in the system — is unit-tested exhaustively in `dezh_core::mcap` rather than only observed in a transcript. |
| Reviewability (W11) | The kernel was one file of 8,776 lines. It is now 724 lines of boot sequence plus 26 modules, moved across 23 commits that each had to stay green. This is listed here because it is the difference between a reviewer being *able* to audit a subsystem and being told to trust a summary of it — the audience this repository asks for critique from reads code, not diagrams. Three gaps are stated rather than rounded away in [the roadmap's W11 acceptance table](ROADMAP.md): `pkg.rs` is over the size cap, `smp` still holds four `static mut`, and the command table is separate from its handlers. |

## What is measured, and how honestly

- All performance numbers live in [dezh-boot/BENCH.md](../dezh-boot/BENCH.md)
  and follow D015: a named architectural lever plus a measurement, never a bare
  "faster than X".
- The only **real-silicon, same-CPU** comparison is the capability-check cost
  (~1 ns) vs the Linux syscall floor (~49 ns). Everything else measured inside
  the kernel (ecall round trip, Pol translation overhead) is **QEMU-emulated**
  and labelled as such; those absolute numbers are not comparable to hardware.
  The Pol overhead is reported as a *delta* precisely because the emulated trap
  cost cancels in the subtraction.

## Known limitations (the parts reviewers should push on)

- **VM targets only.** No real-hardware port; no real device drivers beyond
  virtio under QEMU/VirtualBox.
- **x86 kernel is thin, but no longer trusting.** It has a returnable interrupt
  path, preemption, per-task address spaces, ring 3, a fault that kills only the
  task that caused it, and capability-checked syscalls whose grants are derived
  from an intent ceiling — all asserted in CI on both boot paths. What it does
  not have: device IRQs, storage, an SDK or install path, and — the honest gap —
  no `Ahd` token, no `Sand` effect ledger and no mission on x86, so authority is
  *derived* there but effects are not yet *accounted for*. Nothing frees a dead
  task's pages. The rich interactive surface (console, IPC, Cairn, Pol) and the
  whole intent-to-effect ledger remain RISC-V only.
- **Pol is a small syscall subset.** `write`, `exit`/`exit_group` are serviced;
  everything else returns a clean `-ENOSYS`. No threads, no dynamic linking, no
  file system. It proves the mechanism, not broad Linux compatibility.
- **Intent-level leases + revocation exist; in-flight capability clawback does
  not.** An intent (`Ahd`) can be opened with a **lease** (a bounded run count
  that auto-revokes on exhaustion) or revoked explicitly (`intent-revoke`); a
  revoked or exhausted intent authorizes nothing further, while the effects it
  already produced keep their provenance (`tbar`/`sfar` still resolve). This
  gives coarse, honest revocation for long-lived agents (`lease-demo`). What is
  still **not** done is clawing back a capability already handed to and running
  inside another task mid-execution; attenuation, task-death, and rollback cover
  the common cases. See [Enforcement model](SECURITY_MODEL.md#enforcement-model).
- **No IOMMU.** DMA isolation for the block daemon is a bounce-window
  convention, not hardware-enforced. Accelerator/DMA isolation (D017) is a
  hypothesis, not implemented.
- **Package signing — the mechanism is built; the distribution layer is not.**
  `.dzp` packages can be wrapped in a signed `DZSP` envelope whose Ed25519
  signature binds the *authority* the package requests, and the kernel verifies
  it against a root-anchored trust store, attenuating the grant to the
  publisher's ceiling (`granted = requested ∩ ceiling`) and refusing tampered or
  revoked-key packages — proven end to end by `sig-demo` (see
  [Package signing](SUBSYSTEMS.md#package-signing)). What is **not** done yet: a stand-
  alone developer signing CLI, a root-signed trust store loaded from disk with
  key rotation (today it is kernel-embedded), and verifying packages on the live
  `pkg-recv` upload path. No online PKI / certificate-transparency service.
- **SMP: symmetric scheduling works for queued tasks; the console's own scheduler
  is still single-hart.** Secondary harts come up via SBI HSM, the kernel has a fair
  spinlock, several harts drain one shared queue, and real U-mode tasks are
  scheduled symmetrically across harts with per-task address spaces keeping them
  isolated (`smp-sched`, `smp-isolate`). A secondary hart now also arms **its own
  timer** while a U-mode task runs there, so such a task is interrupted and
  resumed instead of owning the hart until it exits — `smp-preempt` reports the
  tick count for the specific hart that ran the task, and refuses to claim
  success if the task landed on the boot hart, which has preempted since W9.
  What is **not** done: that interrupt only resumes the task, it does not yet
  pick a *different* one, so there is still **no migration** and no scheduling
  decision on a secondary. The *console's* scheduler with its task table, IPC
  mailboxes and frame allocator is still single-threaded on the boot hart and not
  under the lock, so daemons and console tasks are not yet dispatchable on any hart.
  Merging the two into one lock-protected scheduler is the rest of **W13** (see
  [ROADMAP.md](ROADMAP.md)).
- No production installer, no side-channel hardening, no formal verification.
- **The live capabilities are a per-task bitmask; the object-capability
  primitive is built but not yet the substrate.** Today's task authority is a bit
  per class/namespace (kernel-attested on every IPC message and attenuable on
  delegation — so *not* Linux-style ambient caps), not an unforgeable per-object
  reference like seL4/CHERI. The first-class alternative now exists and is proven
  (`dezh_core::ocap` + the `cap-demo`: generation-stamped object handles with
  per-object revocation and an attenuated delegation graph), but the live IPC /
  Cairn plumbing has **not** been migrated onto it yet — that migration is the
  single largest planned change. See [Enforcement model](SECURITY_MODEL.md#enforcement-model).
- **Confidentiality: DIFC is enforced on the storage path; other channels are
  not yet.** The information-flow-control primitive is built (`dezh_core::difc`)
  and **enforced on the live Cairn path** (`taintflow-demo`): reading `ns=vault`
  (labelled secret) taints the operator, then a commit to a lower namespace is
  refused (no write-down) until a privileged `declassify`. The **integrity** axis
  is enforced too (`ingress-demo`): talking to the network lowers the operator's
  integrity, so unvalidated input cannot be written into a namespace that demands
  an endorsement until a privileged `endorse`. What is **not** done is enforcing
  either taint across the U-mode client→daemon hop and IPC, and the ingress taint
  is at **operator granularity** — consuming *any* network reply lowers integrity
  wholesale rather than tracking the individual bytes through the system. See
  [Threat model](SECURITY_MODEL.md#threat-model) §5.
- **Networking is a probe, not a stack.** Marz does Ethernet, ARP, IPv4, UDP
  egress and ICMP echo — enough to prove the edge is real and reachable in both
  directions. There is **no TCP, no DNS, no DHCP (the address is static), no
  inbound listening and no routing**. Only `marz-effect` ledgers what comes back
  (see the effect-gateway row above); `marz-ping`'s ICMP replies are not effect
  records. See [Marz](SUBSYSTEMS.md#marz-guarded-egress).
- **Effect-runtime honesty (W8 + W12).** Most modeled effects (`email.send`,
  `prod.deploy`, a compensatable `api-key`) are still **models** — they prove
  the mechanism, not an integration. One is not: `marz-effect` drives a real
  external system through the host gateway. The limit there is stated in the
  row above and is worth repeating, because it is the kind of thing a reader
  should not have to find twice — **the gateway is outside Dezh's TCB.** Dezh
  proves the effect was authorized, left the machine, was ledgered under an
  intent, and that its compensation ran. It cannot prove the gateway was honest
  about what it did on the other side.
  Ledger integrity trusts the storage daemon (records are parent-linked and
  hashed for corruption detection + rollback, not signed against a malicious
  writer). The commit log is a fixed 255 slots with no GC yet. Intents (`Ahd`)
  are runtime sessions and are not persisted across a reboot; for their lease
  and revocation status, which this bullet used to deny and the list above
  grants, see that entry — leases and `intent-revoke` are real, in-flight
  clawback is not. See [Threat model](SECURITY_MODEL.md#threat-model).
- **Console input is reliable but not perfect.** UART0 is routed through the
  PLIC and both the interrupt handler and `getc` drain the FIFO into a ring, so
  a pasted line no longer depends on the console happening to be inside `getc`.
  Pasting 64-character lines: 9/10 at `-smp 1`, 8/9 at `-smp 2`, 10/10 at
  `-smp 4`, 8/9 at `-smp 8`. What is **gone** is the collapse with hart count —
  the same test was 2/8 at `-smp 4` and 0/3 at `-smp 8`, where most lines never
  arrived at all. That was never a race: idle secondary harts spun, and on an
  emulated host every vCPU shares one budget, so they took it from the hart
  draining the UART. They now sleep. What **remains** is occasional loss at every
  hart count, cause not yet identified; `irq-stat` reports the ring's own
  full-count, which stays at zero, so the bytes go missing before any drain runs.
  Tracked as issue #19.
- **In-kernel U-mode task caveat (RISC-V).** Some baked demo tasks share the
  kernel binary and must avoid non-inlined calls; real apps use the separate-ELF
  and `.dzp` loader paths, which do not have this constraint.

## How to check these claims yourself

See [REVIEWER_GUIDE.md](REVIEWER_GUIDE.md) for the exact commands, or
[Running in a VM](GETTING_STARTED.md#running-in-a-vm) to boot a release in a VM. Everything in
the first two tables above is asserted by `tools/ci/qemu_smoke.py` and runs on
every push.

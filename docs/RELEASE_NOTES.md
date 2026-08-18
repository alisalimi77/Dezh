# Release Notes

## v0.5-review Candidate

The release where the second ISA stops being a demo target, and where a
secondary hart stops running a task uninterruptibly.

v0.4 shipped with `STATUS.md` saying the x86 kernel had "no returnable interrupt
path yet — no timer, no device IRQs, no scheduler on x86", and that tasks on
secondary RISC-V harts "run to completion — no preemption or migration there".
Both sentences are why this release exists.

### x86_64: from a boot smoke to a kernel that does not trust its tasks

Sixteen commits, each green on both boot paths (QEMU `-kernel` PVH and the GRUB
Multiboot2 ISO):

- **A returnable interrupt path.** The IDT grows from 32 exception vectors to
  256, and vectors 32..255 save every register, dispatch, restore and `iretq` —
  an interrupt now interrupts work and hands control back, rather than ending it.
- **Preemption.** Three tasks that never yield, and none of them keeps the CPU.
- **Per-task address spaces.** Every task gets its own `cr3` and reads a
  different page from the same address.
- **Ring 3.** A task runs at CPL3 with one door back into the kernel, and a
  faulting task dies without taking the machine with it.
- **Authority derived from intent, not asserted by the caller.** x86 had no
  capability check at all — its syscalls served anyone. A task now carries a
  capability word and the syscall path consults the task table, never a register
  the caller set. The derivation is `granted = requested ∩ ceiling`, computed by
  **the same `dezh_core::mcap` function the RISC-V kernel calls**, so a second
  implementation cannot drift from the exhaustive test that pins it. Two CPL3
  tasks run byte-identical code from a byte-identical manifest and end up with
  different authority.

### RISC-V: W13 steps 1 and 2

- **A secondary hart arms its own timer**, so a U-mode task there is interrupted
  and resumed instead of owning the hart until it exits (`smp-preempt`). The
  demo counts ticks per hart and refuses to claim success if the task landed on
  the boot hart, which has preempted since W9.
- **Idle secondary harts sleep.** They used to spin, and on an emulated host
  every vCPU shares one budget — so they were taking it from the hart draining
  the console.
- **One ticket lock for the whole kernel**, which masks the acquiring hart's
  interrupts. Not a performance choice: `plic_handle` writes scheduler state
  from interrupt context, so without masking a hart can take the lock, take an
  interrupt, and wait for itself.
- **The task table is private to the scheduler** and its reachable surface is
  under that lock. Six modules used to read and write it directly.

### The console stops losing pasted input

UART0 is IRQ 10 on the `virt` board and had never been enabled at the PLIC,
which routed only the virtio slots — so `getc` spun on the line-status register
and read the receive register directly. That keeps up with a person typing and
loses bytes to anything faster. It is routed now, with a receive ring both the
handler and `getc` drain into, and `irq-stat` reports bytes received plus the
two places a byte could be dropped.

### Honest scope

- **No migration.** The timer interrupt on a secondary resumes the task it
  interrupted; it does not pick a different one, because choosing means reading
  a task table that is still the boot hart's. That is the rest of W13.
- **x86 derives authority but does not account for effects.** No `Ahd` token, no
  `Sand` ledger, no mission there. It has no device IRQs, no storage, no install
  path, and nothing frees a dead task's pages.
- **Console input can still be lost, and it is not ours.** Six runs sending 204
  bytes measured 204/202/200/188 received with zero drops at every layer, the
  shortfall matching the echoed line exactly each time. The guest is never handed
  those bytes. Single-run paste measurements on a host pipe are noise — the same
  configuration gave 0/10 and 10/10 minutes apart.
- The release workflow no longer publishes a container image. It never
  succeeded in publishing one; see [RELEASING](RELEASING.md#the-review-environment).


## v0.4-review Candidate

The release where the effect ledger stops being checked against itself, and
where the kernel becomes something a stranger can actually read.

v0.3 could attribute and undo an agent's night — but every effect it attributed
lived inside Dezh's own storage. The ledger was the only witness to its own
claims. That is the gap this release closes.

### What a reviewer can now do

- Run `marz-effect <dest> <verb> <arg>` and watch an effect **leave the
  machine**: authorized against a live NIC capability, egress authority for that
  named destination and the DIFC export rule, ARP-resolved, sent on the wire —
  and then the **outcome comes back** and is ledgered. It records
  `compensatable` and carries **the undo itself**, not just the class, so
  `sfar-plan` names the compensating action rather than promising one exists.
  The reply lowers operator integrity, because bytes off the wire are
  attacker-chosen.
- Check that claim without trusting us. `tools/ci/effect_test.py` runs twelve
  checks and **none of them read Dezh's transcript** — every assertion about
  external state is made against the external system itself, including that a
  revoked NIC capability leaves it untouched.
- Read the kernel. `main.rs` went from 8,776 lines to 724 plus 26 modules,
  across 23 commits that each stayed green.
- Type `help` and see all 151 commands. In v0.3 it silently listed 111: the
  `Intent` and `Effects` groups were missing from a hand-written list, so
  `intent-open`, `sand-log`, `tbar`, `why-denied`, `overnight` and `redteam`
  were absent from the first screen a reviewer reads. The list is now checked
  against the command table when the kernel is built.

### The boundary, stated up front

The host gateway that performs the external effect is **not in Dezh's TCB**. A
compromised gateway can lie about what it did. Dezh proves the parts it owns —
authorized, left the machine, ledgered under an intent, compensation ran — and
not the gateway's honesty. That is a smaller claim than "the OS speaks git",
and it is the true one.

### Honest scope

Everything named in the v0.3 notes that is still open stays open: no IOMMU, the
x86 kernel has no scheduler or drivers, Pol is a small syscall subset, the
console's own scheduler is single-hart, and in-flight capability clawback does
not exist. See [STATUS.md](STATUS.md), which now also lists the three W11 gaps
rather than rounding them away.

One correction to the v0.3 notes: they described intents as having no lease or
revocation. That had already stopped being true when `lease-demo` shipped, and
STATUS.md said so in one place while denying it in another. The contradiction is
fixed in favour of the accurate half.

## v0.3-review Candidate

The milestone where the no-ambient-authority rule stops being a single-core,
single-threaded claim. Two bodies of work land here: an intent-to-effect runtime
that can undo an agent's night honestly, and the hardware work — real device
interrupts, symmetric multiprocessing, and a bidirectional network edge — that
tests whether the rule holds when the machine gets harder.

### What a reviewer can now do

- Run `overnight` — leave a coding agent loose under **one intent**, then in the
  morning forecast the rollback, retract what is reversible, run and record a
  compensating action for what is compensatable, and watch the system **refuse**
  the irreversible rather than pretend. The agent's attempt to act outside its
  intent is denied by the kernel, and `why-denied` names the boundary.
- Boot under `-smp 4` and watch U-mode tasks run on several harts **at the same
  instant**, each in its own address space — then watch an intruder page-fault
  and die on its own hart while its neighbour keeps running.
- Send a real ICMP echo to a destination the caller holds a capability for, and
  see the reply come back through ARP resolution — then watch consuming that
  reply **lower integrity**, so unvalidated network bytes cannot quietly become
  trusted state.
- Verify a signed `.dzp`: the Ed25519 envelope binds the authority the package
  asks for, and the kernel checks it.

### Flagship demos

Everything from v0.2-review (F1 containment, F2 Cairn, F3 multi-ISA, F4 Pol)
still runs, joined by `overnight`, `smp-sched`, `smp-isolate`, `marz-ping`,
`ingress-demo`, `taintflow-demo`, `redteam`, `sig-demo` and `lease-demo` — all
green in `tools/ci/qemu_smoke.py`.

### Honest scope

Three limitations named in the v0.2 notes are closed: runtime revocation,
package signing, and SMP. Still open, and stated plainly rather than buried:

- **No IOMMU.** User-space drivers buy fault isolation and least privilege of
  the driver *process*, not memory safety against a malicious driver that
  programs the device to DMA anywhere. This is core to the story, not polish.
- **No formal verification**, and no in-flight capability clawback — revocation
  is at the intent-lease and object-generation level, which is coarse but
  honest.
- **Package signing has no distribution layer** yet: no standalone signing CLI,
  no on-disk root-signed trust store.
- QEMU and VirtualBox targets only. Emulated benchmarks are labelled as such.

Full detail, including what reviewers should push on, in `docs/STATUS.md`.

### Artifacts

RISC-V and x86_64 kernels, the bootable `dezh-<tag>-x86_64.iso`, a `.dzp` sample
package, a `RUN.txt`, the docs bundle, a manifest, and `SHA256SUMS`.

## v0.2-review Candidate

The milestone where all four flagship demos are green in CI and a reviewer can
boot Dezh in a VM with no source tree.

### What a reviewer can now do

- Boot the x86_64 kernel from a real bootable ISO in **VirtualBox / VMware** (or
  QEMU `-cdrom`); it reaches 64-bit long mode, installs and runs a `.dzp` agent
  package, enforces the print capability, and catches a deliberately-raised CPU
  exception instead of triple-faulting. See `GETTING_STARTED.md#running-in-a-vm`.
- Run the RISC-V capability console — agent containment (F1), Cairn versioned
  storage with rollback across reboot (F2), the same byte-identical Dezh-IR app
  on both ISAs (F3), and a real unmodified Linux ELF under Pol (F4).

### Flagship demos

- **F1 agent containment** — narrow caps, kernel-DENIED beyond grant, attenuated
  IPC delegation, rollback (`tools/demo/run_agent_demo.py`).
- **F2 Cairn v1** — commit log, rollback, reboot-persistent, capability-gated
  namespaces (`cairn-demo`).
- **F3 multi-ISA** — byte-identical `.dzp` runs on RISC-V and x86_64; bytes
  pinned by a test.
- **F4 Pol** — a stock static Linux/RISC-V ELF runs capability-gated; the same
  bytes run on real Linux; translation overhead measured (`bench-pol`).

### Honest scope

QEMU/VirtualBox targets only; benchmarks that are emulated are labelled as such;
Pol is a small syscall subset; no runtime revocation, IOMMU, package signing, or
SMP yet. Full detail in `docs/STATUS.md`.

### Artifacts

RISC-V and x86_64 kernels, the bootable `dezh-<tag>-x86_64.iso`, a `.dzp` sample
package, a `RUN.txt`, the docs bundle, a manifest, and `SHA256SUMS`.

## v0.1-review Candidate

`v0.1-review` is the first public review candidate for Dezh OS.

It is intended for architecture, security-model, package-lifecycle, and
prototype-execution review. It is not a production release.

## Highlights

- Bare-metal RISC-V QEMU boot through OpenSBI.
- x86_64 smoke target for the shared runtime path.
- U-mode task isolation with contained page faults.
- Explicit capability gates for syscall effects.
- Long-lived user-space `virtio-block` daemon.
- Typed IPC with status-aware replies and timeout accounting.
- Service registry with stop, restart, and controlled fault demos.
- Reboot-safe package store for SDK-built `.dzp` packages.
- Transactional install/remove/update/rollback path.
- Journal recovery, quarantine, pin/unpin, explicit GC, and capability
  escalation review.
- Embedded apps for note, lab, calculator, and vault workflows.
- Public demo transcripts and review tooling.

## Validation

Recommended validation:

```sh
python tools/review/run_full_review.py --quick
```

Full validation:

```sh
python tools/review/run_full_review.py --full
```

Expected release artifacts are described in
[Release Process](RELEASING.md#release-process). GitHub Packages usage is described in
[Packages And Releases](RELEASING.md#packages-and-releases).

## Known Limitations

- QEMU is the primary validation environment.
- The installer initializes a prototype disk layout, not production boot media.
- Package checksums are deterministic v0 checks, not production cryptographic
  signatures.
- DMA isolation is modeled through page-table discipline and grants; real IOMMU
  work is future scope.
- Store sizes and package limits are intentionally small for reviewability.
- Networking, graphics, formal verification, and real hardware bring-up are
  future work.

## Review Questions

- Is the no-ambient-authority model visible in the code and tests?
- Is the user-space block driver boundary placed correctly?
- Are package lifecycle states and recovery rules sufficiently explicit?
- Are service failure modes clean enough for long-running operation?
- Which parts should be reduced, split, or formalized before the next review
  candidate?

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
| Drivers out of kernel | virtio-block is a U-mode daemon holding an explicit MMIO + DMA grant; clients reach it only over typed IPC. **Caveat (not buried):** without an IOMMU this gives fault isolation + least privilege of the driver *process*, not memory safety against a malicious driver that programs the device to DMA anywhere. The IOMMU is core to this story, not future polish. |
| W8 — intent → effect runtime | An agent runs under one **intent** (`Ahd`); its derived capability is provably ⊆ the intent. Every effect is a ledger record (`Sand`) carrying `actor → intent → derived cap → reversibility`. A whole **mission** (`Sfar`) is rolled back honestly: reversible effects retracted, compensatable effects undone by a **recorded** compensating action, irreversible effects **refused with a reason** — and rollback needs authority over every namespace the mission touched. A five-escape adversary (`redteam`) is stopped at five named boundaries; `why-denied` names the boundary of the last denial; `Tbar` renders the `actor → intent → effect` provenance graph. The `overnight` flagship runs the whole story. |

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
- **x86 kernel is thin.** Exception IDT exists, but there is no returnable
  interrupt path yet — no timer, no device IRQs, no scheduler on x86. The rich
  interactive surface (console, scheduler, IPC, Cairn, Pol) is RISC-V only.
- **Pol is a small syscall subset.** `write`, `exit`/`exit_group` are serviced;
  everything else returns a clean `-ENOSYS`. No threads, no dynamic linking, no
  file system. It proves the mechanism, not broad Linux compatibility.
- **No runtime revocation** of an already-delegated, still-in-use capability.
  Attenuation, task-death, and rollback cover the common cases; a lease/generation
  scheme is designed but not built. See [SECURITY_MODEL.md](SECURITY_MODEL.md).
- **No IOMMU.** DMA isolation for the block daemon is a bounce-window
  convention, not hardware-enforced. Accelerator/DMA isolation (D017) is a
  hypothesis, not implemented.
- **Unsigned packages — a known structural gap, not just missing polish.** `.dzp`
  packages are CRC-checked and manifest-verified, not cryptographically signed.
  For a system whose thesis is "no authority without explicit provenance," an
  unsigned package is a real tension: install-time provenance is currently
  operator trust + checksum, not a verified signature. Signed manifests
  (provenance of *who authored the authority a package requests*) are the first
  item on the hardening roadmap, ahead of broad features.
- No production installer, no SMP, no side-channel hardening, no formal
  verification.
- **W8 effect-runtime honesty.** External effects (`email.send`, `prod.deploy`,
  a compensatable `api-key`) are **modeled**, not wired to real connectors — the
  point proven is the *mechanism* (attribution, honest rollback, compensation),
  not a network/DB/secrets integration (that is the future "Gateways" line).
  Ledger integrity trusts the storage daemon (records are parent-linked and
  hashed for corruption detection + rollback, not signed against a malicious
  writer). The commit log is a fixed 255 slots with no GC yet. Intents (`Ahd`)
  are runtime sessions, not persisted, and there is no lease/revocation for
  long-lived agents. See [THREAT_MODEL.md](THREAT_MODEL.md).
- **In-kernel U-mode task caveat (RISC-V).** Some baked demo tasks share the
  kernel binary and must avoid non-inlined calls; real apps use the separate-ELF
  and `.dzp` loader paths, which do not have this constraint.

## How to check these claims yourself

See [REVIEWER_GUIDE.md](REVIEWER_GUIDE.md) for the exact commands, or
[QUICKSTART_VM.md](QUICKSTART_VM.md) to boot a release in a VM. Everything in
the first two tables above is asserted by `tools/ci/qemu_smoke.py` and runs on
every push.

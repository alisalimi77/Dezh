# Marz — the guarded egress boundary

**Marz** (*border*) is the boundary an effect crosses to leave the machine.
Crossing it is irreversible: once bytes are on the wire, no ledger can call them
back. So Marz is where Dezh's whole stack has to hold at once — capability,
intent, information flow, and effect accountability.

This document follows the project method: study what existing systems do and
where they fail, state the precise delta, then design. Status: **design +
phased implementation**; §6 marks what is built.

---

## 1. What the field already does

| System | Network access model | Source |
| --- | --- | --- |
| **seL4 / Genode / Fuchsia** | The protocol stack runs in **user space** (lwIP/PicoTCP). An application has **no direct channel to the NIC driver**; it reaches the network only by capability-protected IPC to the stack. | [seL4 whitepaper][sel4] |
| **HiStar / Flume (DIFC)** | Data carries **labels**; exporting data *out of the system* is a **declassification** that only a privileged principal may perform. Flume gives each tag two capabilities (`t+`/`t−`) for declassify/endorse. | [Flume, SOSP'07][flume] |
| **Linux / Windows** | **Ambient authority**: a process names a destination and connects. Authorization is a global property of the process, not of the *destination*. | [Ambient authority][ambient] |

Read together, the field has solved two different halves:

- capability systems confine **access to the device/stack**, and
- DIFC systems constrain **which data may flow out**,

while mainstream systems do neither per-destination — which is exactly the
exfiltration channel: a compromised process connects anywhere, and nothing
records *which* destination on whose authority.

## 2. The mistakes we design against

1. **Ambient egress.** "Any process may connect anywhere." → In Marz the
   **destination is part of the capability**, not a parameter the caller picks
   freely.
2. **Access control without flow control.** Confining *who may use the NIC* does
   not stop a permitted principal from shipping a secret. → Marz applies the
   DIFC rule on export: a tainted actor may not send to a lower-secrecy
   destination without an explicit **declassification** (the Flume lesson).
3. **Flow control without accountability.** A label check leaves no record of
   *what left, under whose intent*. → Every send is a **Sand effect**:
   `actor → intent → derived cap → destination → irreversible`.
4. **Pretending egress is reversible.** Rollback machinery that "undoes" a send
   is a lie. → Marz effects are classified **irreversible**; `sfar-rollback`
   refuses them with an explanation, exactly as it already does for the modeled
   external effects.
5. **A side-channel audit log.** A log beside the socket can be bypassed. → The
   Marz record is on the authorization path: no ambient route to the NIC exists,
   so an effect cannot reach the wire without going through the record.

## 3. Design

**Principals.** The NIC is owned by a user-space **Marz daemon** holding only an
explicit MMIO + DMA grant for the virtio-net device — the same shape as the
existing `virtio-block` daemon. No task, and no agent, ever touches the NIC.

**The egress capability names a destination.** Authority to send is not "network
access"; it is a capability for a specific **destination** (address + port
class). It is derived from an intent (`Ahd`) exactly like every other authority:

```text
derived_destinations = requested_destinations ∩ intent_ceiling
```

so an agent can only reach destinations its intent already allowed, and anything
beyond is dropped and reported.

**Export requires declassification (the DIFC gate).** Before a send, the actor's
secrecy taint must flow to the destination's label:

```text
send permitted  ⟺  taint(actor) ⊆ label(destination)
```

A secret-tainted actor sending to a public destination is **refused** — the
exfiltration case — unless a privileged principal explicitly declassifies. This
is Flume's rule applied at the wire.

**Every send is a ledgered, irreversible effect.** On success Marz appends a Sand
record: actor, intent, derived capability, destination, `reversibility =
irreversible`, so `tbar` attributes it and `sfar-plan` forecasts honestly that it
cannot be undone.

## 4. The precise delta (what is ours)

We claim no novelty on user-space network stacks (seL4/Genode/Fuchsia) or on
DIFC labels and declassification (HiStar/Flume). The recombination is:

> **egress as a per-destination, intent-derived capability whose every use is a
> declassification-checked, irreversibly-classified record on the same effect
> ledger** — on a substrate with no ambient authority to route around it.

| Property | Linux/Win | seL4/Genode/Fuchsia | HiStar/Flume | Marz |
| --- | --- | --- | --- | --- |
| No ambient path to the NIC | ✗ | ✓ | ~ | ✓ |
| Capability names the **destination** | ✗ | ~ | ✗ | ✓ |
| Authority derived from an **intent** | ✗ | ✗ | ✗ | ✓ |
| Flow control on export (declassify) | ✗ | ✗ | ✓ | ✓ |
| Send is a **ledgered effect** | ✗ | ✗ | ✗ | ✓ |
| Classified **irreversible**, rollback refuses | ✗ | ✗ | ✗ | ✓ |
| Attributed to a **mission** | ✗ | ✗ | ✗ | ✓ |

## 5. Why this matters beyond a feature

Until now every external effect in Dezh has been **modeled** (`email.send`,
`prod.deploy`) and the docs say so. Marz makes one real. It also makes the
confidentiality work load-bearing: today an agent is bound to a single Cairn
namespace, so there is no channel to exfiltrate *through*. A network gives it
one — and the DIFC gate is what stands in the way.

## 6. Phases (each CI-green, in the W8 style)

- **M1 — device. DONE.** The `marz` daemon is a separate U-mode ELF holding
  exactly two grants: the **single** virtio-net MMIO page the kernel discovered
  (capability `TASK_DEVICE_VIRTIO_NET` — not the whole window the block grant
  maps) and a DMA window. It never scans for hardware. It negotiates no features,
  arms the transmit queue, builds a real Ethernet + IPv4 + UDP frame and sends
  it. `marz-send` drives it; CI asserts the frame in QEMU's packet capture, so
  the claim is verified **on the wire**, not from a print.
- **M2 — the gate. DONE.** Egress authority names a **destination**, not "the
  network": each destination carries an address and a secrecy label, and the gate
  requires (a) the capability for *that* destination and (b) a flow the
  destination may legally receive (`taint(actor) subset of label(destination)`).
  Revoking one destination leaves the others intact. `marz-demo` proves both on
  the wire, and CI counts frames in the capture: exactly the authorized sends
  appear, and a refused send leaves **nothing** behind. (Deriving the destination
  set from an intent ceiling is the remaining slice.)
- **M3 — the effect. DONE.** Every authorized send is recorded as an
  **irreversible** Sand effect carrying its actor, intent and destination, so
  `tbar` attributes what left the machine and `sfar-plan` forecasts it honestly.
  `sfar-rollback` **refuses** it - the wire cannot be undone and Dezh does not
  pretend otherwise. `marz-effect-demo` shows the whole loop.
- **Verification.** QEMU's packet capture (`-object filter-dump`) lets CI assert
  the permitted frame actually left **and that the refused one did not** — a real
  test, not a printed claim.

### Honest non-goals (v0)

No TCP, no DNS, no inbound listening, no routing; a minimal frame/UDP-class
egress only. No cryptographic transport. This is the authority + accountability
mechanism at the network edge, not a network stack.

<!-- sources -->
[sel4]: https://sel4.systems/About/seL4-whitepaper.pdf
[flume]: https://pdos.csail.mit.edu/papers/flume-sosp07.pdf
[ambient]: https://en.wikipedia.org/wiki/Ambient_authority

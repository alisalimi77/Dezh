# Outreach

Targeted technical review requests. **Do not mass-send.** Verify the appropriate
public channel for each community or organization at send time, and post as a
person asking for critique — not as an announcement.

Every claim below must stay true to [STATUS.md](STATUS.md) and
[Threat model](SECURITY_MODEL.md#threat-model). If a reviewer finds a gap we did not name
ourselves, the post was wrong.

---

## 1. Technical post — OS and systems people

*(r/osdev, and the seL4 / Genode / CHERI / capability-systems crowd. Goal:
serious critique, not applause.)*

**Title: Dezh — making an agent's *effects* accountable, on a kernel with no
ambient authority**

I've been building a from-scratch capability OS substrate and I'd like it torn
apart by people who know this space.

**What I am not claiming.** Capability security (Dennis & Van Horn; KeyKOS/EROS;
Miller's object-capability model), a verified microkernel (seL4), user-space
drivers and capability components (Genode), hardware capabilities (CHERI),
decentralized information-flow control (HiStar/Flume), and compensation-based
recovery (sagas) are all prior art. None of them is my contribution, and the
from-scratch kernel is **not** the point — for a product, building this on seL4
or Genode would be the right call, and the FAQ says so.

**What I think is new** is a recombination that needs a substrate with no ambient
authority underneath it:

- **Intent is the only path to authority.** A capability is derived as
  `requested ∩ intent_ceiling` — a structural subset, not a purpose string.
  Anything beyond the intent is dropped and reported.
- **The effect ledger is *on* the authorization path**, not beside it. Every
  effect *is* the record that authorized and persisted it, carrying
  `actor → intent → derived capability → reversibility class`. On an
  ambient-authority host you can usually reach the resource around the logger;
  here there is no such path.
- **Rollback that refuses to lie.** Effects are classified reversible /
  compensatable / irreversible / unknown. A whole mission is forecast *before*
  anything is touched; then reversible effects are retracted, compensatable ones
  are undone by running and **recording** a compensating action, and irreversible
  ones are **refused with a reason**. A connector that declares nothing is
  `unknown` and is never optimistically "undone".
- **Egress is a first-class effect.** Network authority names a *destination*,
  not "the network"; export is checked against an information-flow taint (the
  Flume rule that leaving the system is a declassification); and a raw send is
  recorded as an **irreversible** effect that rollback refuses.
- **The effect leaves the machine and the outcome comes back.** This is the part
  I most expect to be attacked, so it is stated narrowly. `marz-effect` drives a
  real external system through a host gateway: authorized against a live device
  capability, egress authority for that named destination and the export rule,
  ARP-resolved, sent on the wire — and the reply is ledgered, as `compensatable`,
  carrying **the undo itself** rather than only its class, so the rollback
  forecast names the compensating action instead of promising one exists. The
  reply also *lowers integrity*, because bytes off the wire are attacker-chosen.

**Evidence, not slides.** Everything is exercised in CI on a RISC-V kernel under
QEMU. The one I'd point a skeptic at first: the external-effect test **never
reads Dezh's own transcript**. Every assertion about what happened out there is
made against the external system's own state, including the negative case — a
revoked device capability must leave it byte-for-byte untouched. The egress test
is the same idea one layer down: QEMU captures the packets and it **fails unless
exactly the authorized frames are on the wire**, so a refused send that leaked or
an authorized one that never left both turn CI red. The capability algebra
(`derived ⊆ intent`; delegation only attenuates) is proved by exhaustive
enumeration in a host test rather than asserted in prose.

**Where it is weak, in my own words.** Not formally verified — seL4 is the bar and
I am not near it. QEMU only. **No IOMMU**, so a user-space driver gives fault
isolation and least privilege of the driver *process*, not memory safety against a
malicious driver; that is core to the story, not future polish. **The gateway
that performs the external effect is outside the TCB** — a compromised one can
lie about what it did, so what is proved is authorization, egress, ledgering and
that the compensation ran, not the gateway's honesty; it is one connector, and
the rest of the modeled effects are still models. Information flow
is enforced on the storage path and at egress, not across every channel. Packages
are signed, but there is no key distribution or transparency service.
`print`/`time`/`ipc` are still plain permission bits on purpose (they name no
object); namespaces, devices and destinations are generation-stamped handles with
per-object revocation.

**What I would most value critique on:** whether the ledger is genuinely
unbypassable given the trusted base I describe; whether the reversibility
classification is honest; and whether the novelty claim survives contact with
prior art you know better than I do.

Repo: <https://github.com/alisalimi77/Dezh> · Design + prior-art comparison: `docs/RELATED_WORK.md`,
`docs/SUBSYSTEMS.md#marz-guarded-egress` · Honest limits: `docs/STATUS.md`, `docs/SECURITY_MODEL.md#threat-model`

---

## 2. Short intro — agent-runtime and coding-agent teams

*(Teams shipping agents that touch repos, CI, deploys, or secrets. Goal: a design
partner and a real use case.)*

**Subject: containing coding agents at the OS level — worth a look?**

You already sandbox the agents you run. Sandboxes are good at *confinement* and
bad at the question that actually comes up after an incident: **what exactly did
it do, on whose authority, and how much of it can I undo?**

Dezh is a research OS substrate built around that question. An agent runs under a
single declared **intent**; every effect it produces is recorded on the path that
authorized it, carrying who did it, under which intent, and whether it can be
undone. In the morning you get a **forecast** of what a rollback can and cannot
reverse — then reversible work is retracted, compensatable work is undone by a
recorded compensating action, and anything genuinely irreversible is **refused
with an explanation** instead of being silently "rolled back".

Three things that usually surprise people:

- **Exfiltration is refused at the wire, not audited afterwards.** If the agent
  reads something secret it becomes tainted, and a send to a destination not
  cleared for that secret is blocked before a packet exists. Network authority
  names a *destination*, so "it had network access" is not a thing here.
- **The undo is recorded with the effect, not guessed later.** One connector is
  real rather than modeled: an effect that leaves the machine, changes an
  external system, and comes back to be ledgered with the specific action that
  reverses it. So the morning forecast names the compensating command instead of
  claiming a class and hoping. Honest limit: that gateway is outside the trusted
  base, so it can lie about what it did on the far side.
- **It is one command.** `overnight` runs the whole story: an agent loose under
  one intent, a morning of forecast and provenance, an honest rollback, and a
  contained escape attempt.

Honest framing: this is a **research prototype on QEMU**, not a product. No formal
verification, no IOMMU yet, a small syscall surface. I am looking for one or two
teams whose agents change real repos/CI/deploys to tell me where the effect model
breaks against their workflow — especially which effects would need typed
connectors, and what "undo" has to mean for them.

Repo: <https://github.com/alisalimi77/Dezh> · Start here: `docs/transcripts/overnight.md`

---

## Sending checklist

- Verify every link resolves before sending.
- Re-read [STATUS.md](STATUS.md): if a limitation changed, fix the post first.
- Ask for critique on something specific; a post with no question gets no review.
- One channel at a time. Answer the hard replies before posting anywhere else.

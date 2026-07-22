# Flagship demo: leave a coding agent loose overnight

This is the one story the whole W8 intent/effect runtime exists to tell. You give
an AI agent a single **intent** and turn it loose overnight. It does real work,
touches the outside world, and even tries to escape its intent. In the morning
you **account for and undo its night** — honestly, with no over-promising.

One console command, `overnight`, runs the whole thing, and it is asserted end to
end in CI (`tools/ci/qemu_smoke.py`). It collapses every W8 part into a single
narrative:

- **Intent (`Ahd`, P1)** — the agent runs under one declared authority ceiling;
  its derived capability is provably a subset of that intent.
- **Effect ledger (`Sand`, P2)** — every effect *is* a Cairn commit enriched with
  `actor → intent → derived cap → reversibility class`. No second write, no
  bypass.
- **Mission + honest rollback (`Sfar`, P3)** — the effects under one intent are a
  mission. Reversible effects are retracted by a ref move; a **compensatable**
  effect is undone by *running and recording* its registered compensating action;
  an **irreversible** effect is **refused with a reason**, never silently undone.
  Mission authority spans every namespace the mission touched.
- **Adversary (`redteam`, P4)** — the agent also tries to act beyond its intent;
  the kernel denies it at a named boundary.
- **Explainable denial + provenance (`why-denied` / `Tbar`, P5)** — the refusal is
  attributable to a specific mechanism, and the provenance graph attributes every
  effect to its actor and intent, unforgeably.

## What the agent's night looks like

The agent, under intent `Ahd#8`, produces a mission of mixed effects:

| Effect | Namespace | Reversibility | How a rollback treats it |
| --- | --- | --- | --- |
| `prod.deploy:web@v9` (modeled external send) | lab | irreversible | **refused with a reason** — it already happened |
| `wrote build cache` | lab | reversible | retracted by moving the ref |
| `updated changelog` | lab | reversible | retracted by moving the ref |
| `created api-key:tmp/42` (modeled) | calc | compensatable | **compensated** — `revoke api-key:tmp/42` is run and recorded |

It also tries to write to Cairn beyond the `compute` ceiling it derived — an
out-of-intent escape — and is denied by the kernel.

## Transcript (captured from the RISC-V kernel under QEMU)

```
dezh> overnight
[overnight] you leave a coding agent loose overnight under ONE intent; in the morning you account for and undo its night
[overnight] 1/6 opened the agent's intent Ahd#8 (a writer ceiling) and turned it loose
[overnight] 2/6 the agent's night: an irreversible deploy + two reversible writes (ns=lab), one compensatable external action (ns=calc)
  [cairn] commit ns=lab slot=19 parent=none len=55 hash=0x7378c6ecf8dd4b1f actor=task1 intent=Ahd#8
  [cairn] commit ns=lab slot=20 parent=19 len=17 hash=0x13a7541a8ace112c actor=task1 intent=Ahd#8
  [cairn] commit ns=lab slot=21 parent=20 len=17 hash=0x910d3425d3954e9a actor=task1 intent=Ahd#8
  [cairn] commit ns=calc slot=22 parent=18 len=68 hash=0xb86876c08745737e actor=task1 intent=Ahd#8
[overnight] 3/6 morning: FORECAST the rollback before touching anything, and read the provenance
  [sfar] rollback forecast for mission Ahd#8 (live effects, newest first):
    slot=21 gen=3 actor=task1 intent=Ahd#8 derived=print,cairn-read,cairn-write reversibility=reversible status=committed hash=0x910d3425d3954e9a ns=lab
    slot=20 gen=2 actor=task1 intent=Ahd#8 derived=print,cairn-read,cairn-write reversibility=reversible status=committed hash=0x13a7541a8ace112c ns=lab
    slot=19 gen=1 actor=task1 intent=Ahd#8 derived=print,cairn-read,cairn-write reversibility=irreversible status=committed hash=0x7378c6ecf8dd4b1f ns=lab
    slot=22 gen=3 actor=task1 intent=Ahd#8 derived=print,cairn-read,cairn-write reversibility=compensatable status=committed hash=0xb86876c08745737e ns=calc
  [sfar] plan: reversible=2 compensatable=1 irreversible=1 unknown=0 confidence=partial (some effects cannot be undone)
  [tbar] provenance graph for intent Ahd#8 (actor -> intent -> effect, unforgeable):
    actor task1 -> intent Ahd#8 (derived print,cairn-read,cairn-write) -> effect ns=lab slot=21 class=reversible status=committed hash=0x910d3425d3954e9a
    actor task1 -> intent Ahd#8 (derived print,cairn-read,cairn-write) -> effect ns=lab slot=20 class=reversible status=committed hash=0x13a7541a8ace112c
    actor task1 -> intent Ahd#8 (derived print,cairn-read,cairn-write) -> effect ns=lab slot=19 class=irreversible status=committed hash=0x7378c6ecf8dd4b1f
    actor task1 -> intent Ahd#8 (derived print,cairn-read,cairn-write) -> effect ns=calc slot=22 class=compensatable status=committed hash=0xb86876c08745737e
  [tbar] 4 effect(s) attributed to intent Ahd#8
[overnight] 4/6 undo the night honestly: retract the reversible writes, run the compensation, REFUSE the irreversible deploy with a reason
    [sfar] REFUSED at ns=lab slot=19: irreversible effect already happened in the outside world; cannot be undone
    [sfar] COMPENSATED at ns=calc slot=22: ran compensating action "revoke api-key:tmp/42" recorded as effect slot=23
  [sfar] mission Ahd#8 rolled back: reversible effects retracted=2 compensations performed=1 refused_irreversible=1 refused_compensatable=0
  [sfar] history preserved: reversible effects retracted by ref, compensatable effects undone by a recorded compensating action, irreversible effects explained not erased
[overnight] 5/6 the agent also TRIED to escape its intent (a write beyond the ceiling); the kernel denied it
[redteam] agent under Ahd#9 kind=compute requests=print cairn-read cairn-write
[redteam] beyond-intent dropped by the derivation ceiling: cairn-read cairn-write (derived cap proven <= Ahd)
[redteam] kernel DENIED the out-of-intent Cairn write: missing required capability for this host call
[overnight] 6/6 why was the escape denied? name the boundary:
[why-denied] last denial: actor=overnight action=intent.derive target=cairn-write result=DENIED (tick 168)
[why-denied] boundary: intent-derivation ceiling (derived cap <= Ahd), enforced in the kernel
[why-denied] policy: authority is explicit and unforgeable; nothing runs on ambient permission
[overnight] PASS: the whole night is accounted for - reversibles undone, the compensatable action compensated, the irreversible deploy refused with a reason, and the escape contained
```

## Why a user-space sandbox cannot tell this story

A sandbox (gVisor, Firecracker, `wasmtime`/WASI, `seccomp`+`landlock`) can confine
the agent, and it can kill the process. What it cannot cleanly do is **attribute
and reverse the whole set of effects the agent produced under one intent** — the
effect log sits beside the resource on an ambient-authority host, and there is
generally a path to the resource that skips it. On Dezh there is no ambient
authority under the ledger: the effect path goes *through* the record that
authorizes it, which is why the from-scratch kernel exists. See
[`THREAT_MODEL.md`](THREAT_MODEL.md#6-why-not-just-a-user-space-sandbox-head-to-head).

## Reproduce

Boot the RISC-V kernel (see [`BUILD_AND_RUN.md`](BUILD_AND_RUN.md)) and type
`overnight`. The individual acts are also available on their own: `intent-open` /
`intent-run`, `sand-log` / `sand-info`, `sfar-plan` / `sfar-rollback`,
`comp-demo`, `sfar-cross-demo`, `tbar`, `redteam`, `why-denied`. All are asserted
in `tools/ci/qemu_smoke.py`.

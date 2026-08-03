#!/usr/bin/env python3
"""Generate every diagram in docs/assets from data + shared tokens.

Run:   python tools/docs/build_diagrams.py
Check: python tools/docs/build_diagrams.py --check   (CI: fails on drift)

Why this exists: the diagrams used to be hand-placed SVG. Twelve rows times
five columns of hand-typed coordinates is a standing invitation to drift, and
three files authored separately had drifted into three different palettes, three
type stacks and three corner radii. Here the coordinates are computed and the
palette comes from one module, so consistency is a build property rather than a
review chore.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from diagram_tokens import (  # noqa: E402
    DARK,
    FS_BODY,
    FS_DISPLAY,
    FS_HERO,
    FS_LEAD,
    FS_MICRO,
    FS_SMALL,
    FS_TITLE,
    LIGHT,
    MONO,
    PAD,
    RADIUS_CARD,
    RADIUS_CELL,
    SANS,
    STROKE_EDGE,
    STROKE_HAIR,
    STROKE_MARK,
    esc,
    svg_close,
    svg_open,
    write,
)

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "docs" / "assets"


# Average advance width as a fraction of font-size, measured against the system
# sans stack. Deliberately pessimistic: it is used to *reject* layouts that
# would collide, so over-estimating is the safe direction.
ADV_REGULAR = 0.55
ADV_BOLD = 0.60
ADV_TRACKED = 0.68      # bold + letter-spacing:.09em


def text_w(s: str, fs: float, adv: float = ADV_REGULAR) -> float:
    return len(s) * fs * adv


def fits(s: str, fs: float, limit: float, where: str, adv: float = ADV_REGULAR) -> None:
    """Fail the build when a string would overrun the space reserved for it.

    Text collisions are the one class of diagram bug a colour validator cannot
    catch, and the one most likely to creep back in when copy is edited.
    """
    w = text_w(s, fs, adv)
    if w > limit:
        raise SystemExit(
            f"diagram layout: {where} needs {w:.0f}px but only {limit:.0f}px "
            f"is reserved\n  text: {s!r}\n  shorten the copy or widen the column"
        )


# ==========================================================================
# Verdict marks -- silhouette first, colour second
# ==========================================================================

YES, PARTIAL, NO = "yes", "partial", "no"

VERDICT_LABEL = {
    YES: "yes",
    PARTIAL: "partial",
    NO: "no",
}


def verdict_mark(cx: float, cy: float, kind: str, r: float = 9) -> str:
    """A verdict glyph that still reads with the colour removed.

    filled disc = yes, half-filled disc = partial, open ring with a bar = no.
    A reader with deuteranopia, a greyscale printer and forced-colours mode all
    get the same three-way distinction a full-colour reader gets.
    """
    title = f"<title>{VERDICT_LABEL[kind]}</title>"
    g = f'<g transform="translate({cx} {cy})">{title}'

    if kind == YES:
        g += f'<circle r="{r}" fill="var(--success)"/>'
        g += (
            f'<path d="M {-r * 0.40} {r * 0.03} L {-r * 0.13} {r * 0.32} '
            f'L {r * 0.42} {-r * 0.30}" fill="none" stroke="var(--canvas)" '
            f'stroke-width="{STROKE_MARK}" stroke-linecap="round" '
            f'stroke-linejoin="round"/>'
        )
    elif kind == PARTIAL:
        # Left half solid: the disc is literally half full.
        g += f'<path d="M 0 {-r} A {r} {r} 0 0 0 0 {r} Z" fill="var(--attention)"/>'
        g += (
            f'<circle r="{r}" fill="none" stroke="var(--attention)" '
            f'stroke-width="{STROKE_MARK}"/>'
        )
    elif kind == NO:
        g += (
            f'<circle r="{r}" fill="none" stroke="var(--danger)" '
            f'stroke-width="{STROKE_MARK}"/>'
        )
        g += (
            f'<path d="M {-r * 0.45} 0 L {r * 0.45} 0" stroke="var(--danger)" '
            f'stroke-width="{STROKE_MARK}" stroke-linecap="round"/>'
        )
    else:  # pragma: no cover - guarded by the data assertion below
        raise ValueError(f"unknown verdict {kind!r}")

    return g + "</g>"


# ==========================================================================
# 1. Comparison matrix
# ==========================================================================

SYSTEMS = ["Dezh", "seL4", "Genode", "Fuchsia", "gVisor"]

# (group heading, group note, [(row label, [verdict per system])])
MATRIX = [
    (
        "Foundations",
        "mature prior art already does these",
        [
            ("No ambient authority (from scratch)", [YES, YES, YES, YES, NO]),
            ("Capability-based access control", [YES, YES, YES, YES, NO]),
            ("Drivers in user space", [YES, YES, YES, YES, NO]),
            ("Formally verified kernel", [NO, YES, NO, NO, NO]),
        ],
    ),
    (
        "Effect accountability",
        "the axis this project exists for",
        [
            ("Effect ledger ON the authorization path", [YES, NO, NO, NO, NO]),
            ("Per-effect reversibility class", [YES, NO, NO, NO, NO]),
            ("Whole-mission rollback + compensation", [YES, NO, NO, NO, NO]),
            ("Explainable denial + provenance graph", [YES, NO, NO, NO, PARTIAL]),
            ("Intent-scoped authority + leases", [YES, PARTIAL, PARTIAL, PARTIAL, NO]),
            ("Agent as a first-class principal", [YES, NO, NO, NO, NO]),
        ],
    ),
    (
        "Hardening",
        "where Dezh concedes today",
        [
            ("IOMMU-enforced DMA", [NO, PARTIAL, YES, YES, NO]),
            ("Signed packages (+ publisher attenuation)", [YES, NO, PARTIAL, YES, NO]),
        ],
    ),
]

for _g, _n, _rows in MATRIX:
    for _label, _verdicts in _rows:
        assert len(_verdicts) == len(SYSTEMS), f"row {_label!r} has wrong arity"
        assert all(v in VERDICT_LABEL for v in _verdicts), f"row {_label!r} bad verdict"


def build_comparison() -> tuple[str, list[str]]:
    W = 960
    LABEL_W = 390
    COL_W = 106
    COL_X0 = PAD + LABEL_W          # 410
    ROW_H = 34
    GROUP_H = 32
    HEAD_H = 44
    assert COL_X0 + COL_W * len(SYSTEMS) == W - PAD, "column band must be flush"

    css = (
        f".ttl{{fill:var(--fg);font:700 {FS_TITLE}px {SANS}}}"
        f".sub{{fill:var(--fg-muted);font:400 {FS_SMALL}px {SANS}}}"
        f".sys{{fill:var(--fg);font:600 {FS_BODY}px {SANS}}}"
        f".sysx{{fill:var(--fg);font:700 {FS_BODY}px {SANS}}}"
        f".grp{{fill:var(--fg);font:700 {FS_MICRO}px {SANS};letter-spacing:.09em}}"
        f".grpn{{fill:var(--fg-subtle);font:400 {FS_MICRO}px {SANS}}}"
        f".row{{fill:var(--fg);font:400 {FS_BODY}px {SANS}}}"
        f".leg{{fill:var(--fg-muted);font:400 {FS_SMALL}px {SANS}}}"
        f".foot{{fill:var(--fg-subtle);font:400 {FS_MICRO}px {SANS}}}"
        f".hair{{stroke:var(--border);stroke-width:{STROKE_HAIR};fill:none}}"
    )

    # ---- vertical layout, computed ------------------------------------
    y_title = PAD + 16
    y_sub = y_title + 20
    table_top = y_sub + 18
    head_bot = table_top + HEAD_H

    y = head_bot
    plan: list[tuple] = []
    for heading, note, rows in MATRIX:
        plan.append(("group", y, heading, note))
        y += GROUP_H
        for label, verdicts in rows:
            plan.append(("row", y, label, verdicts))
            y += ROW_H
    table_bot = y

    y_legend = table_bot + 30
    y_foot = y_legend + 26
    H = y_foot + PAD - 6

    p = svg_open(
        W,
        H,
        "Dezh compared with capability and isolation systems",
        "A twelve-row capability matrix comparing Dezh with seL4, Genode, Fuchsia and "
        "gVisor across three groups. Foundations: Dezh matches prior art on ambient "
        "authority, capabilities and user-space drivers, and concedes formal "
        "verification to seL4. Effect accountability: Dezh is the only system marked "
        "yes on effect ledger, per-effect reversibility, whole-mission rollback, "
        "provenance and agent-as-principal. Hardening: Dezh concedes IOMMU-enforced "
        "DMA to Genode and Fuchsia.",
        css,
    )

    p.append(f'<rect width="{W}" height="{H}" rx="{RADIUS_CARD}" fill="var(--canvas)"/>')

    title = "Dezh vs capability & isolation systems"
    subtitle = (
        "An honest matrix. Dezh concedes formal verification and IOMMU-enforced "
        "DMA; what it holds alone is the middle group."
    )
    fits(title, FS_TITLE, W - 2 * PAD, "matrix title", ADV_BOLD)
    fits(subtitle, FS_SMALL, W - 2 * PAD, "matrix subtitle")
    p.append(f'<text x="{PAD}" y="{y_title}" class="ttl">{esc(title)}</text>')
    p.append(f'<text x="{PAD}" y="{y_sub}" class="sub">{esc(subtitle)}</text>')

    # Painted in explicit layers, back to front. The group strips have to land
    # on top of the grid: a column rule crossing a full-width group heading
    # chops it into cells that mean nothing.
    grid: list[str] = []
    strips: list[str] = []
    ink: list[str] = []

    # ---- Dezh column emphasis, one continuous band ---------------------
    p.append(
        f'<rect x="{COL_X0}" y="{table_top}" width="{COL_W}" '
        f'height="{table_bot - table_top}" rx="{RADIUS_CELL}" '
        f'fill="var(--accent-wash)"/>'
    )

    # ---- column headers ------------------------------------------------
    for i, name in enumerate(SYSTEMS):
        cx = COL_X0 + i * COL_W + COL_W / 2
        cls = "sysx" if i == 0 else "sys"
        ink.append(
            f'<text x="{cx}" y="{table_top + 27}" class="{cls}" '
            f'text-anchor="middle">{esc(name)}</text>'
        )
    grid.append(
        f'<path d="M {PAD} {head_bot} H {W - PAD}" class="hair" '
        f'stroke="var(--border-strong)"/>'
    )
    for i in range(len(SYSTEMS) + 1):
        x = COL_X0 + i * COL_W
        grid.append(f'<path d="M {x} {table_top} V {table_bot}" class="hair"/>')

    # ---- groups and rows ----------------------------------------------
    for item in plan:
        if item[0] == "group":
            _, gy, heading, note = item
            strips.append(
                f'<rect x="{PAD}" y="{gy}" width="{W - 2 * PAD}" height="{GROUP_H}" '
                f'fill="var(--surface)"/>'
            )
            hx = PAD + 10
            ink.append(
                f'<text x="{hx}" y="{gy + 21}" class="grp">'
                f"{esc(heading.upper())}</text>"
            )
            nx = hx + text_w(heading.upper(), FS_MICRO, ADV_TRACKED) + 14
            # The group strip must stay inside the label column; a note that
            # spilled into the Dezh band would read as a collision.
            fits(note, FS_MICRO, COL_X0 - 10 - nx, f"group note {heading!r}")
            ink.append(f'<text x="{nx}" y="{gy + 21}" class="grpn">{esc(note)}</text>')
        else:
            _, ry, label, verdicts = item
            fits(label, FS_BODY, LABEL_W - 20, f"row label {label!r}")
            ink.append(
                f'<text x="{PAD + 10}" y="{ry + 22}" class="row">{esc(label)}</text>'
            )
            for i, v in enumerate(verdicts):
                cx = COL_X0 + i * COL_W + COL_W / 2
                ink.append(verdict_mark(cx, ry + ROW_H / 2, v))
            grid.append(f'<path d="M {PAD} {ry + ROW_H} H {W - PAD}" class="hair"/>')

    p.extend(grid)
    p.extend(strips)
    p.extend(ink)

    # ---- legend ---------------------------------------------------------
    lx = PAD + 9
    for kind, word in (
        (YES, "yes"),
        (PARTIAL, "partial"),
        (NO, "no"),
    ):
        p.append(verdict_mark(lx, y_legend, kind, r=7))
        p.append(f'<text x="{lx + 14}" y="{y_legend + 4}" class="leg">{word}</text>')
        lx += 14 + 8.5 * len(word) + 26
    p.append(
        f'<text x="{W - PAD}" y="{y_legend + 4}" class="leg" text-anchor="end">'
        f"Marks are readable without colour: filled / half / open.</text>"
    )

    p.append(
        f'<text x="{PAD}" y="{y_foot}" class="foot">Dezh is a research prototype '
        f"(QEMU + bootable x86_64 ISO). Sources: docs/RELATED_WORK.md and "
        f"docs/SECURITY_MODEL.md#threat-model</text>"
    )

    p.append(svg_close())
    return "comparison.svg", p


# ==========================================================================
# 2. README banner
# ==========================================================================

def build_banner() -> tuple[str, list[str]]:
    W, H = 1280, 400

    css = (
        f".hero{{fill:var(--fg);font:700 {FS_HERO}px {SANS}}}"
        f".eyebrow{{fill:var(--accent);font:700 {FS_MICRO}px {SANS};letter-spacing:.16em}}"
        f".lead{{fill:var(--fg);font:600 {FS_DISPLAY}px {SANS}}}"
        f".body{{fill:var(--fg-muted);font:400 {FS_LEAD}px {SANS}}}"
        f".boxt{{fill:var(--fg);font:700 {FS_BODY}px {SANS}}}"
        f".boxn{{fill:var(--fg-muted);font:400 {FS_MICRO}px {SANS}}}"
        f".card{{fill:var(--surface);stroke:var(--border);stroke-width:{STROKE_EDGE}}}"
        f".wire{{stroke:var(--border-strong);stroke-width:{STROKE_EDGE};fill:none}}"
    )

    p = svg_open(
        W,
        H,
        "Dezh OS -- intent-native, capability-secure operating-system prototype",
        "Banner. Left: the Dezh OS wordmark with the project's rule -- no program "
        "starts with ambient authority. Right: a layer diagram. Apps, the "
        "virtio-block driver and agents run in user mode; every call crosses a "
        "kernel that grants no ambient authority; effects land in the Cairn commit "
        "log, which is rollbackable.",
        css,
    )

    p.append(f'<rect width="{W}" height="{H}" fill="var(--canvas)"/>')

    # ---- left: the claim ------------------------------------------------
    x = 80
    p.append(
        f'<path d="M {x} 64 H {W - 80}" stroke="var(--border)" '
        f'stroke-width="{STROKE_HAIR}"/>'
    )
    p.append(
        f'<text x="{x}" y="112" class="eyebrow">CAPABILITY-SECURE OS PROTOTYPE '
        f"· RISC-V · x86_64</text>"
    )
    p.append(f'<text x="{x}" y="192" class="hero">Dezh OS</text>')
    p.append(
        f'<rect x="{x}" y="214" width="72" height="4" rx="2" fill="var(--accent)"/>'
    )
    p.append(
        f'<text x="{x}" y="264" class="lead">No program starts with ambient '
        f"authority.</text>"
    )
    p.append(
        f'<text x="{x}" y="300" class="body">Every effect is backed by an explicit '
        f"capability, grant or lease —</text>"
    )
    p.append(
        f'<text x="{x}" y="324" class="body">and every effect stays attributable, '
        f"and reversible where it can be.</text>"
    )

    # ---- right: the layer diagram ---------------------------------------
    RX, RW = 700, 500
    bw = (RW - 2 * 16) / 3
    top_y, mid_y, bot_y = 92, 196, 284
    bh = 60

    for i, (label, note) in enumerate(
        [("Apps", "manifest caps"), ("Driver", "virtio-block"), ("Agents", "one intent")]
    ):
        bx = RX + i * (bw + 16)
        p.append(
            f'<rect x="{bx}" y="{top_y}" width="{bw}" height="{bh}" '
            f'rx="{RADIUS_CELL}" class="card"/>'
        )
        p.append(f'<text x="{bx + 14}" y="{top_y + 26}" class="boxt">{label}</text>')
        p.append(f'<text x="{bx + 14}" y="{top_y + 45}" class="boxn">{note}</text>')
        # wire down into the kernel
        cx = bx + bw / 2
        p.append(f'<path d="M {cx} {top_y + bh} V {mid_y}" class="wire"/>')
        p.append(f'<circle cx="{cx}" cy="{mid_y}" r="3" fill="var(--accent)"/>')

    p.append(
        f'<rect x="{RX}" y="{mid_y}" width="{RW}" height="{bh}" rx="{RADIUS_CELL}" '
        f'fill="var(--accent-wash)" stroke="var(--accent)" '
        f'stroke-width="{STROKE_EDGE}"/>'
    )
    p.append(f'<text x="{RX + 16}" y="{mid_y + 26}" class="boxt">Kernel</text>')
    p.append(
        f'<text x="{RX + 16}" y="{mid_y + 45}" class="boxn">typed IPC · '
        f"capability check on every effect · no ambient authority</text>"
    )

    p.append(f'<path d="M {RX + RW / 2} {mid_y + bh} V {bot_y}" class="wire"/>')
    p.append(f'<circle cx="{RX + RW / 2}" cy="{bot_y}" r="3" fill="var(--accent)"/>')

    p.append(
        f'<rect x="{RX}" y="{bot_y}" width="{RW}" height="{bh}" rx="{RADIUS_CELL}" '
        f'class="card"/>'
    )
    p.append(f'<text x="{RX + 16}" y="{bot_y + 26}" class="boxt">Cairn</text>')
    p.append(
        f'<text x="{RX + 16}" y="{bot_y + 45}" class="boxn">commit log · '
        f"per-app namespaces · rollback and compensation</text>"
    )

    p.append(svg_close())
    return "dezh-readme-banner.svg", p


# ==========================================================================
# 3. Overnight transcript
# ==========================================================================

# (role, text) -- role maps onto a terminal colour token
OVERNIGHT = [
    ("prompt", "overnight"),
    ("info", "[overnight] leave a coding agent loose overnight under ONE intent"),
    ("info", "[overnight] 1/6 opened the agent's intent Ahd#8 (a writer ceiling)"),
    ("info", "[overnight] 2/6 the night: 1 irreversible deploy + 2 reversible writes"),
    ("info", "            (ns=lab), 1 compensatable external action (ns=calc)"),
    ("dim", "  [cairn] commit ns=lab  intent=Ahd#8  x3"),
    ("dim", "  [cairn] commit ns=calc intent=Ahd#8"),
    ("info", "[overnight] 3/6 morning: forecast the rollback, read the provenance"),
    ("note", "  [sfar] plan: reversible=2 compensatable=1 irreversible=1  -> partial"),
    ("note", "  [tbar] 4 effect(s) attributed to intent Ahd#8 (actor -> intent -> effect)"),
    ("info", "[overnight] 4/6 undo the night honestly:"),
    ("bad", "    [sfar] REFUSED ns=lab: irreversible - already happened outside"),
    ("warn", '    [sfar] COMPENSATED ns=calc: ran "revoke api-key:tmp/42", recorded'),
    ("note", "  [sfar] rolled back: retracted=2 compensations=1 refused_irreversible=1"),
    ("info", "[overnight] 5/6 the agent also TRIED to escape its intent:"),
    ("bad", "  [redteam] kernel DENIED the out-of-intent write (derived cap <= Ahd)"),
    ("info", "[overnight] 6/6 why-denied -> boundary: intent-derivation ceiling"),
    ("pass", "[overnight] PASS: undone, compensated, the irreversible refused, escape contained"),
]

ROLE_FILL = {
    "info": "var(--term-info)",
    "note": "var(--term-note)",
    "dim": "var(--term-dim)",
    "warn": "var(--term-warn)",
    "bad": "var(--term-bad)",
    "pass": "var(--term-ok)",
}


def build_overnight() -> tuple[str, list[str]]:
    FS = 13
    LH = 20
    CHROME = 40
    PADX = 18
    # Monospace advance is a stable ~0.60em across the stack; size the frame to
    # the longest line so nothing ever clips.
    longest = max(len(t) for _r, t in OVERNIGHT)
    W = int(PADX * 2 + longest * FS * 0.60) + 10
    H = CHROME + 22 + len(OVERNIGHT) * LH + 14

    css = (
        f".m{{font:400 {FS}px {MONO}}}"
        f".b{{font:700 {FS}px {MONO}}}"
        f".cap{{fill:var(--fg-muted);font:400 {FS_MICRO}px {SANS}}}"
    )

    p = svg_open(
        W,
        H,
        "The Dezh overnight flagship run",
        "Terminal transcript. A coding agent is left running overnight under one "
        "intent. In the morning Dezh forecasts the rollback, retracts the two "
        "reversible writes, compensates the external action, refuses the "
        "irreversible deploy rather than pretending to undo it, and reports that "
        "the agent's attempt to act outside its intent was denied by the kernel.",
        css,
    )

    p.append(
        f'<rect width="{W}" height="{H}" rx="{RADIUS_CARD}" fill="var(--term-canvas)" '
        f'stroke="var(--border)" stroke-width="{STROKE_HAIR}"/>'
    )
    # window chrome
    p.append(
        f'<path d="M 0 {CHROME} V {RADIUS_CARD} A {RADIUS_CARD} {RADIUS_CARD} 0 0 1 '
        f'{RADIUS_CARD} 0 H {W - RADIUS_CARD} A {RADIUS_CARD} {RADIUS_CARD} 0 0 1 '
        f'{W} {RADIUS_CARD} V {CHROME} Z" fill="var(--term-chrome)"/>'
    )
    p.append(
        f'<path d="M 0 {CHROME} H {W}" stroke="var(--border)" '
        f'stroke-width="{STROKE_HAIR}"/>'
    )
    for i, tok in enumerate(("--danger", "--attention", "--success")):
        p.append(
            f'<circle cx="{18 + i * 18}" cy="{CHROME / 2}" r="5" '
            f'fill="var({tok})" opacity="0.85"/>'
        )
    p.append(
        f'<text x="{W / 2}" y="{CHROME / 2 + 4}" class="cap" text-anchor="middle">'
        f"Dezh — RISC-V kernel (QEMU) · the overnight flagship</text>"
    )

    y = CHROME + 26
    for role, text in OVERNIGHT:
        if role == "prompt":
            p.append(
                f'<text x="{PADX}" y="{y}" class="b">'
                f'<tspan fill="var(--term-ok)">dezh&gt; </tspan>'
                f'<tspan fill="var(--term-fg)">{esc(text)}</tspan></text>'
            )
        else:
            weight = "b" if role == "pass" else "m"
            p.append(
                f'<text x="{PADX}" y="{y}" class="{weight}" '
                f'fill="{ROLE_FILL[role]}" xml:space="preserve">{esc(text)}</text>'
            )
        y += LH

    p.append(svg_close())
    return "overnight.svg", p


# ==========================================================================

BUILDERS = (build_banner, build_comparison, build_overnight)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--check",
        action="store_true",
        help="verify committed assets match this generator; do not write",
    )
    args = ap.parse_args()

    stale: list[str] = []
    for builder in BUILDERS:
        name, parts = builder()
        body = "\n".join(parts) + "\n"
        path = ASSETS / name
        if args.check:
            current = path.read_text(encoding="utf-8") if path.exists() else ""
            if current != body:
                stale.append(name)
        else:
            write(path, parts)
            print(f"wrote {path.relative_to(ROOT).as_posix()} ({len(body):,} bytes)")

    if args.check:
        if stale:
            print("diagram drift: " + ", ".join(stale), file=sys.stderr)
            print("run: python tools/docs/build_diagrams.py", file=sys.stderr)
            return 1
        print(f"diagrams up to date ({len(BUILDERS)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

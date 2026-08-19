#!/usr/bin/env python3
"""Render a captured console session as an animated SVG.

Why this exists, and why it is a generator rather than a drawing: the evidence
this project offers *is* console output. A hand-drawn animation of an agent
moving through boxes would be a picture of a claim; replaying the bytes the
kernel actually printed is the claim itself. So every line below comes out of a
real QEMU smoke transcript, and regenerating after a behaviour change is one
command rather than an illustration job.

Animated SVG rather than GIF: it stays sharp at any width, the file is a few
kilobytes instead of a few megabytes, the text is real text for screen readers
and search, and one asset can follow the reader's light/dark theme -- which a
GIF cannot do without shipping two of them.

Usage:
    python tools/docs/make_terminal_svg.py --scene redteam --out docs/assets/demo-redteam.svg
"""
from __future__ import annotations

import argparse
import html
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# --- The scenes -------------------------------------------------------------
#
# Each scene is a title, a caption and the console lines. Lines are stored here
# verbatim from a smoke run rather than read from a transcript at build time, so
# the asset is reproducible without a QEMU run -- and `--verify` checks them
# back against a live transcript when there is one.

SCENES = {
    "redteam": {
        "title": "Dezh: five escapes, five named boundaries",
        "caption": "console session, verbatim from tools/ci/qemu_smoke.py",
        "lines": [
            ("prompt", "redteam"),
            ("dim", "[redteam] adversary loose: a malicious agent attempts five escapes;"),
            ("dim", "          each must hit a NAMED boundary and the system must survive"),
            ("", ""),
            ("dim", "[redteam] escape 1/5: read another app's private Cairn namespace"),
            ("ok", "[redteam] escape 1 STOPPED at boundary: storage-service capability check"),
            ("dim", "          (kernel-attested caps) -- console survived"),
            ("dim", "[redteam] escape 2/5: write a device MMIO register directly"),
            ("ok", "[redteam] escape 2 STOPPED at boundary: hardware memory boundary"),
            ("dim", "          (Sv39 paging, MMIO mapped U=0) -- console survived"),
            ("dim", "[redteam] escape 3/5: forge a capability"),
            ("bad", "  [kernel] DENIED print: task 0 holds no PRINT capability"),
            ("ok", "[redteam] escape 3 STOPPED at boundary: kernel syscall capability check"),
            ("dim", "          (no ambient authority to forge/amplify) -- console survived"),
            ("dim", "[redteam] escape 4/5: act beyond the granted intent"),
            ("warn", "[redteam] beyond-intent dropped by the derivation ceiling: cairn-read cairn-write"),
            ("ok", "[redteam] escape 4 STOPPED at boundary: intent-derivation ceiling (derived cap <= Ahd)"),
            ("dim", "[redteam] escape 5/5: monopolize the CPU (two busy tasks that never yield)"),
            ("ok", "[redteam] escape 5 STOPPED at boundary: preemptive scheduler"),
            ("dim", "          (timer interrupt forces a context switch) -- console survived"),
            ("", ""),
            ("pass", "[redteam] PASS: all five escapes were stopped at named boundaries;"),
            ("pass", "          the adversary was contained and the console is still alive"),
        ],
    },
    "sfar": {
        "title": "Dezh: rolling back a mission, refusing what cannot be undone",
        "caption": "console session, verbatim from tools/ci/qemu_smoke.py",
        "lines": [
            ("prompt", "sfar-demo"),
            ("dim", "[sfar-demo] 1/4 mission Ahd#4: one irreversible external send"),
            ("dim", "            + two reversible writes"),
            ("", ""),
            ("dim", "[sfar-demo] 2/4 rollback FORECAST before touching anything"),
            ("warn", "  [sfar] plan: reversible=2 compensatable=0 irreversible=1 unknown=0"),
            ("warn", "         confidence=partial (some effects cannot be undone)"),
            ("", ""),
            ("dim", "[sfar-demo] 3/4 roll the mission back"),
            ("bad", "    [sfar] REFUSED at ns=agent slot=10: irreversible effect already"),
            ("bad", "           happened in the outside world; cannot be undone"),
            ("ok", "  [sfar] mission Ahd#4 rolled back: reversible effects retracted=2"),
            ("ok", "         compensations performed=0 refused_irreversible=1 refused_compensatable=0"),
            ("", ""),
            ("pass", "[sfar-demo] PASS: whole-mission rollback undid the reversible writes"),
            ("pass", "            and refused the irreversible send with an explanation"),
            ("dim", "[sfar-demo] Dezh does not over-promise rollback: unknown/irreversible"),
            ("dim", "            effects are never silently 'undone'"),
        ],
    },
}

# --- Geometry ---------------------------------------------------------------

CHAR_W = 7.55  # width of one monospace glyph at 13px, measured for the stack below
LINE_H = 21
PAD_X = 26
PAD_Y = 20
CHROME_H = 38
FONT_PX = 13

CLASS_FOR = {
    "": "t-dim",
    "dim": "t-dim",
    "ok": "t-ok",
    "bad": "t-bad",
    "warn": "t-warn",
    "pass": "t-pass",
    "prompt": "t-fg",
}


def build(scene_key: str, hold: float = 3.2, step: float = 0.62) -> str:
    scene = SCENES[scene_key]
    lines = scene["lines"]

    widest = max((len(text) for _, text in lines), default = 0)
    widest = max(widest, len(scene["caption"]) + 4)
    width = int(PAD_X * 2 + widest * CHAR_W) + 8
    height = int(CHROME_H + PAD_Y * 2 + (len(lines) + 1) * LINE_H)

    # One loop: every line appears in turn, the whole frame holds, then it
    # restarts. `step` is how long a line waits for the one above it.
    total = round(len(lines) * step + hold, 2)

    rows = []
    for i, (kind, text) in enumerate(lines):
        if not text:
            continue
        y = CHROME_H + PAD_Y + (i + 1) * LINE_H
        delay = round(i * step, 2)
        cls = CLASS_FOR.get(kind, "t-dim")
        if kind == "prompt":
            rows.append(
                f'<text x="{PAD_X}" y="{y}" class="mono t-prompt line" style="animation-delay:{delay}s">'
                f'dezh&gt; <tspan class="t-fg">{html.escape(text)}</tspan></text>'
            )
        else:
            rows.append(
                f'<text x="{PAD_X}" y="{y}" class="mono {cls} line" style="animation-delay:{delay}s">'
                f'{html.escape(text)}</text>'
            )

    # The cursor rides one line below the last, and only shows once the session
    # has finished printing.
    cur_y = CHROME_H + PAD_Y + (len(lines) + 1) * LINE_H - FONT_PX + 3
    cur_delay = round(len(lines) * step, 2)

    dots = "".join(
        f'<circle cx="{20 + n * 18}" cy="19" r="5.5" class="dot d{n}"/>' for n in range(3)
    )

    described = " ".join(t for _, t in lines if t)

    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-labelledby="ti de">
<title id="ti">{html.escape(scene["title"])}</title>
<desc id="de">An animated replay of a real Dezh console session. {html.escape(described)}</desc>
<style>
:root{{--term:#f6f8fa;--chrome:#eaeef2;--edge:#d1d9e0;--fg:#1f2328;--dim:#59636e;--prompt:#0969da;--ok:#1a7f37;--warn:#9a6700;--bad:#cf222e;--pass:#1a7f37;--c1:#ff5f57;--c2:#febc2e;--c3:#28c840}}
@media(prefers-color-scheme:dark){{:root{{--term:#0d1117;--chrome:#161b22;--edge:#30363d;--fg:#e6edf3;--dim:#8b949e;--prompt:#79c0ff;--ok:#3fb950;--warn:#e3b341;--bad:#ff7b72;--pass:#3fb950}}}}
.mono{{font:400 {FONT_PX}px ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,'Liberation Mono',monospace}}
.t-fg{{fill:var(--fg)}}.t-dim{{fill:var(--dim)}}.t-ok{{fill:var(--ok)}}.t-warn{{fill:var(--warn)}}.t-bad{{fill:var(--bad)}}.t-prompt{{fill:var(--prompt)}}
.t-pass{{fill:var(--pass);font-weight:700}}
.cap{{font:400 11px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;fill:var(--dim)}}
.d0{{fill:var(--c1)}}.d1{{fill:var(--c2)}}.d2{{fill:var(--c3)}}
.line{{opacity:0;animation:appear {total}s linear infinite}}
@keyframes appear{{0%{{opacity:0}}1%{{opacity:1}}96%{{opacity:1}}100%{{opacity:0}}}}
.cursor{{fill:var(--prompt);opacity:0;animation:cur {total}s linear infinite}}
@keyframes cur{{0%{{opacity:0}}1%{{opacity:1}}45%{{opacity:1}}50%{{opacity:0}}55%{{opacity:1}}96%{{opacity:1}}100%{{opacity:0}}}}
@media(prefers-reduced-motion:reduce){{.line,.cursor{{animation:none;opacity:1}}}}
</style>
<rect x="0.5" y="0.5" width="{width - 1}" height="{height - 1}" rx="10" fill="var(--term)" stroke="var(--edge)"/>
<path d="M0.5 {CHROME_H}.5 H{width - 1}" stroke="var(--edge)"/>
<rect x="0.5" y="0.5" width="{width - 1}" height="{CHROME_H}" rx="10" fill="var(--chrome)"/>
<rect x="0.5" y="{CHROME_H - 10}" width="{width - 1}" height="10" fill="var(--chrome)"/>
<path d="M0.5 {CHROME_H}.5 H{width - 1}" stroke="var(--edge)"/>
{dots}
<text x="{width - PAD_X}" y="23" class="cap" text-anchor="end">{html.escape(scene["caption"])}</text>
{chr(10).join(rows)}
<rect x="{PAD_X}" y="{cur_y}" width="8" height="15" class="cursor" style="animation-delay:{cur_delay}s"/>
</svg>
'''


def verify(scene_key: str, transcript: Path) -> list[str]:
    """Check the scene's lines still appear in a real transcript.

    A demo that drifts from what the kernel prints is worse than no demo, so the
    check is by substring on the distinctive part of each line - leading
    indentation and wrapping are this renderer's business, not the kernel's.
    """
    text = transcript.read_text(encoding="utf-8", errors="replace")
    flat = re.sub(r"\s+", " ", text)
    missing = []
    for kind, line in SCENES[scene_key]["lines"]:
        if not line or kind == "prompt":
            continue
        needle = re.sub(r"\s+", " ", line.strip())
        if len(needle) < 12:
            continue
        if needle not in flat:
            missing.append(needle)
    return missing


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scene", choices=sorted(SCENES), required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--verify", type=Path, help="a QEMU transcript to check the lines against")
    args = ap.parse_args()

    if args.verify:
        missing = verify(args.scene, args.verify)
        if missing:
            print(f"{args.scene}: {len(missing)} line(s) not found in the transcript:")
            for m in missing:
                print("  " + m)
            return 1
        print(f"{args.scene}: every line is present in {args.verify.name}")

    out = ROOT / args.out if not args.out.is_absolute() else args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(build(args.scene), encoding="utf-8", newline="")
    print(f"wrote {out.relative_to(ROOT)} ({out.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

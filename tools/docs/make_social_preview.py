#!/usr/bin/env python3
"""Render the repository's social preview card.

GitHub generates a default card - repo name, description, a language bar - for
every repository that does not supply one, and that is what every link to Dezh
has rendered as so far on Hacker News, Reddit, Lobsters and X. This replaces it.

1280x640 is GitHub's documented size for the field, and it is also the 2:1 that
the major link unfurlers crop to, so nothing important goes near the edges.

The card is deliberately the same argument the README leads with, and no more:
the rule, the four stages an effect passes through, and three numbers that came
out of a real run. No screenshot, because a screenshot at thumbnail size is
noise; no tagline that STATUS.md would not sign.

Usage:
    python tools/docs/make_social_preview.py --out docs/assets/social-preview.png
"""
from __future__ import annotations

import argparse
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]

W, H = 1280, 640

# GitHub's dark palette, because the card is viewed on its own and a dark card
# reads as a terminal rather than as a document.
BG = (13, 17, 23)
SURFACE = (22, 27, 34)
EDGE = (48, 54, 61)
FG = (230, 237, 243)
MUTED = (139, 148, 158)
ACCENT = (88, 166, 255)
OK = (63, 185, 80)


def load_font(names: list[str], size: int) -> ImageFont.FreeTypeFont:
    """First font that exists, else Pillow's default at the requested size."""
    for name in names:
        for base in (
            Path("C:/Windows/Fonts"),
            Path("/usr/share/fonts/truetype/dejavu"),
            Path("/Library/Fonts"),
        ):
            candidate = base / name
            if candidate.exists():
                return ImageFont.truetype(str(candidate), size)
    return ImageFont.load_default(size)


SANS_B = ["segoeuib.ttf", "arialbd.ttf", "DejaVuSans-Bold.ttf"]
SANS = ["segoeui.ttf", "arial.ttf", "DejaVuSans.ttf"]
MONO = ["consola.ttf", "cour.ttf", "DejaVuSansMono.ttf"]


def build() -> Image.Image:
    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)

    hero = load_font(SANS_B, 78)
    lead = load_font(SANS_B, 30)
    body = load_font(SANS, 22)
    tiny = load_font(SANS_B, 15)
    mono = load_font(MONO, 19)
    mono_b = load_font(MONO, 19)
    step_t = load_font(SANS_B, 19)
    step_n = load_font(SANS, 15)

    # A hairline at the top, the same device the banner uses.
    d.line([(72, 74), (W - 72, 74)], fill=EDGE, width=1)

    d.text((72, 104), "INTENT-NATIVE  ·  EFFECT-ACCOUNTABLE  ·  RISC-V + x86_64",
           font=tiny, fill=ACCENT)

    d.text((72, 140), "Dezh OS", font=hero, fill=FG)
    d.rectangle([72, 244, 148, 249], fill=ACCENT)

    d.text((72, 286), "No program starts with", font=lead, fill=FG)
    d.text((72, 326), "ambient authority.", font=lead, fill=FG)

    d.text((72, 386), "Every effect is backed by an explicit", font=body, fill=MUTED)
    d.text((72, 416), "capability — attributable, and reversible", font=body, fill=MUTED)
    d.text((72, 446), "where it honestly can be.", font=body, fill=MUTED)

    # Three results, from a real smoke run. Numbers, not adjectives. Drawn as
    # one flowing line each rather than two aligned columns: the value strings
    # are different lengths, and a fixed column wide enough for the longest one
    # pushed the labels into the diagram.
    d.line([(72, 486), (600, 486)], fill=EDGE, width=1)
    stat_v = load_font(MONO, 17)
    stat_l = load_font(MONO, 15)
    stats = [
        ("5/5", "  escapes stopped at named boundaries"),
        ("4x3", "  U-mode tasks across harts, 0 faults"),
        ("2 undone, 1 refused", "  mission rollback"),
    ]
    y = 506
    for value, label in stats:
        d.text((72, y), value, font=stat_v, fill=FG)
        d.text((72 + d.textlength(value, font=stat_v), y + 1), label, font=stat_l, fill=MUTED)
        y += 30

    # The effect path, the same four stages as the README banner.
    steps = [
        ("Agent", "runs under one intent — Ahd#4", False),
        ("Kernel", "granted = requested ∩ ceiling", True),
        ("Sand ledger", "actor → intent → cap → reversibility", False),
        ("Sfar rollback", "retracts reversible, refuses the rest", False),
    ]
    x0, x1 = 660, W - 72
    card_h, gap = 104, 30
    top = 104
    for i, (title, note, accented) in enumerate(steps):
        y0 = top + i * (card_h + gap)
        fill = (12, 45, 107) if accented else SURFACE
        edge = ACCENT if accented else EDGE
        d.rounded_rectangle([x0, y0, x1, y0 + card_h], radius=10, fill=fill, outline=edge, width=2)
        d.text((x0 + 24, y0 + 26), title, font=step_t, fill=FG)
        d.text((x0 + 24, y0 + 58), note, font=step_n, fill=MUTED)
        d.text((x1 - 46, y0 + 18), f"0{i + 1}", font=load_font(SANS_B, 13), fill=(110, 118, 129))
        if i < len(steps) - 1:
            cx = (x0 + x1) // 2
            d.line([(cx, y0 + card_h), (cx, y0 + card_h + gap)], fill=EDGE, width=2)
            d.ellipse([cx - 4, y0 + card_h + gap - 4, cx + 4, y0 + card_h + gap + 4], fill=ACCENT)

    d.text((72, H - 38), "github.com/alisalimi77/Dezh", font=mono, fill=MUTED)
    d.text((72 + d.textlength("github.com/alisalimi77/Dezh   ", font=mono), H - 38), "Apache-2.0", font=mono, fill=OK)
    return img


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", type=Path, default=Path("docs/assets/social-preview.png"))
    args = ap.parse_args()
    out = args.out if args.out.is_absolute() else ROOT / args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    build().save(out, "PNG", optimize=True)
    print(f"wrote {out.relative_to(ROOT)} ({out.stat().st_size} bytes, {W}x{H})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

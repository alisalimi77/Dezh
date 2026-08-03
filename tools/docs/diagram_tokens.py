#!/usr/bin/env python3
"""Shared design tokens for every generated diagram in docs/assets.

One palette, one type scale, one geometry vocabulary. Every diagram imports
from here, so "harmonised" is a property of the build rather than something a
reviewer has to police by eye.

Colour choices are Primer (GitHub's own design system) so the diagrams sit
inside a README as native page furniture instead of as foreign objects, in both
themes.

Status encoding note (important, and measured rather than assumed):
green/amber/red is *not* separable under red-green colour vision deficiency. Run

    node validate_palette.js "#3fb950,#d29922,#f85149" --mode dark --pairs all

and the worst pair is green/red at deltaE 2.2 (deuteranopia); the light-mode set
is worse still at 1.5. Roughly 8% of men cannot read this scale by hue. Every
verdict mark therefore carries its meaning in its *silhouette* -- filled disc,
half-filled disc, open ring -- which survives CVD, greyscale printing and
forced-colours mode. Colour is reinforcement, never the signal.
"""

from __future__ import annotations


# --------------------------------------------------------------------------
# Type
# --------------------------------------------------------------------------
# System stacks only. A webfont cannot be fetched from inside an <img>-embedded
# SVG on GitHub, so naming one just yields silent, uncontrolled fallback.

SANS = (
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,"
    "'Noto Sans',sans-serif"
)
MONO = (
    "ui-monospace,SFMono-Regular,'SF Mono',Menlo,Consolas,"
    "'Liberation Mono',monospace"
)

# Type scale. Steps are deliberately few; a diagram that needs a seventh size is
# a diagram that is trying to say too much.
FS_MICRO = 11
FS_SMALL = 12
FS_BODY = 13
FS_LEAD = 15
FS_TITLE = 18
FS_DISPLAY = 26
FS_HERO = 64


# --------------------------------------------------------------------------
# Geometry
# --------------------------------------------------------------------------

RADIUS_CARD = 10       # outer frames
RADIUS_CELL = 6        # inner cells, chips, boxes
STROKE_HAIR = 1        # grid, dividers
STROKE_EDGE = 1.5      # card borders, emphasis
STROKE_MARK = 2        # data marks, verdict glyphs

PAD = 20               # standard outer padding
GAP = 8                # standard inner gap


# --------------------------------------------------------------------------
# Colour — Primer light / dark
# --------------------------------------------------------------------------

LIGHT = {
    "canvas": "#ffffff",
    "surface": "#f6f8fa",
    "surface-2": "#eaeef2",
    "border": "#d1d9e0",
    "border-strong": "#b7bfc7",
    "fg": "#1f2328",
    "fg-muted": "#59636e",
    "fg-subtle": "#818b98",
    "accent": "#0969da",
    "accent-wash": "#ddf4ff",
    "success": "#1a7f37",
    "attention": "#9a6700",
    "danger": "#cf222e",
    # terminal-flavoured roles, light
    "term-canvas": "#f6f8fa",
    "term-chrome": "#eaeef2",
    "term-fg": "#1f2328",
    "term-dim": "#59636e",
    "term-info": "#0550ae",
    "term-note": "#0969da",
    "term-ok": "#1a7f37",
    "term-warn": "#9a6700",
    "term-bad": "#cf222e",
}

DARK = {
    "canvas": "#0d1117",
    "surface": "#161b22",
    "surface-2": "#21262d",
    "border": "#30363d",
    "border-strong": "#3d444d",
    "fg": "#e6edf3",
    "fg-muted": "#8b949e",
    "fg-subtle": "#6e7681",
    "accent": "#58a6ff",
    "accent-wash": "#0c2d6b",
    "success": "#3fb950",
    "attention": "#d29922",
    "danger": "#f85149",
    # terminal-flavoured roles, dark
    "term-canvas": "#0d1117",
    "term-chrome": "#161b22",
    "term-fg": "#e6edf3",
    "term-dim": "#6e7681",
    "term-info": "#58a6ff",
    "term-note": "#79c0ff",
    "term-ok": "#3fb950",
    "term-warn": "#e3b341",
    "term-bad": "#ff7b72",
}

assert LIGHT.keys() == DARK.keys(), "both themes must define the same roles"


def theme_css() -> str:
    """Emit the custom-property block that makes a diagram theme-aware.

    Light is the default declaration and dark arrives via prefers-color-scheme,
    so a viewer with no preference signal still gets a readable diagram rather
    than a dark slab dropped into a light page.
    """
    light = "".join(f"--{k}:{v};" for k, v in LIGHT.items())
    dark = "".join(f"--{k}:{v};" for k, v in DARK.items())
    return (
        f":root{{{light}}}"
        f"@media(prefers-color-scheme:dark){{:root{{{dark}}}}}"
    )


# --------------------------------------------------------------------------
# XML helpers
# --------------------------------------------------------------------------

def esc(text: str) -> str:
    """Escape text for an XML text node or attribute value."""
    return (
        str(text)
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
    )


def svg_open(width: int, height: int, title: str, desc: str, extra_css: str = "") -> list[str]:
    """Open a themed, accessible SVG root element.

    role/aria-labelledby plus a real title and desc mean assistive technology
    gets the diagram's argument, not just "image".
    """
    return [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="t d">',
        f"<title id=\"t\">{esc(title)}</title>",
        f"<desc id=\"d\">{esc(desc)}</desc>",
        f"<style>{theme_css()}{extra_css}</style>",
    ]


def svg_close() -> str:
    return "</svg>"


def write(path, parts: list[str]) -> None:
    """Write a diagram, newline-terminated, with LF endings on every platform."""
    body = "\n".join(parts) + "\n"
    path.write_text(body, encoding="utf-8", newline="\n")

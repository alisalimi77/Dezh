#!/usr/bin/env python3
"""Keep the documentation navigable, and keep it from sprawling again.

Two invariants, both learned the hard way:

1. Every relative link and every in-page anchor resolves. Consolidating docs
   silently breaks links, and a broken link in a review-facing repo costs more
   credibility than the missing page would have.
2. `docs/` stays small. A reviewer who sees thirty near-duplicate documents reads
   it as generated bulk rather than as work, and they are not wrong to: the
   duplicates we merged in July 2026 included a stale threat model that
   contradicted the real one. Generated evidence lives in `docs/transcripts/`
   and is deliberately exempt - it is produced by the kernel, not written.

Exits non-zero with the offending paths on failure.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Raising this cap is a decision, not a formality: a new top-level document must
# be worth more than a section inside an existing one.
MAX_TOP_LEVEL_DOCS = 16

SKIP_DIRS = {".git", "target", "dist", "node_modules", "__pycache__"}

LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
HEADING = re.compile(r"^(#{1,6}) (.*)$")


def markdown_files() -> list[Path]:
    return [
        p
        for p in ROOT.rglob("*.md")
        if not SKIP_DIRS & set(p.relative_to(ROOT).parts)
    ]


def anchors(path: Path) -> set[str]:
    """GitHub's heading slugs for one file."""
    found: set[str] = set()
    fenced = False
    for line in path.read_text(encoding="utf-8").split("\n"):
        if line.lstrip().startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        m = HEADING.match(line)
        if not m:
            continue
        text = re.sub(r"[`*\[\]()]", "", m.group(2)).strip().lower()
        found.add(re.sub(r"[^a-z0-9 \-]", "", text).replace(" ", "-"))
    return found


def check_links() -> list[str]:
    problems: list[str] = []
    slug_cache: dict[Path, set[str]] = {}
    for src in markdown_files():
        for target in LINK.findall(src.read_text(encoding="utf-8")):
            if target.startswith(("http://", "https://", "mailto:")):
                continue
            rel, _, fragment = target.partition("#")
            dest = (src.parent / rel) if rel else src
            here = src.relative_to(ROOT).as_posix()
            if rel and not dest.exists():
                problems.append(f"{here} -> {target} (no such file)")
                continue
            if fragment and dest.suffix == ".md":
                if dest not in slug_cache:
                    slug_cache[dest] = anchors(dest)
                if fragment not in slug_cache[dest]:
                    problems.append(f"{here} -> {target} (no such heading)")
    return problems


def check_sprawl() -> list[str]:
    top = sorted(p.name for p in (ROOT / "docs").glob("*.md"))
    if len(top) <= MAX_TOP_LEVEL_DOCS:
        return []
    return [
        f"docs/ has {len(top)} top-level documents, cap is {MAX_TOP_LEVEL_DOCS}.",
        "Fold the new material into an existing document, or raise the cap "
        "deliberately in this file and say why in the commit message.",
        "Present: " + ", ".join(top),
    ]


def main() -> int:
    problems = check_links() + check_sprawl()
    if problems:
        print("documentation check failed:")
        for p in problems:
            print(f"  {p}")
        return 1
    top = len(list((ROOT / "docs").glob("*.md")))
    generated = len(list((ROOT / "docs" / "transcripts").glob("*.md")))
    print(
        f"documentation check passed: {len(markdown_files())} files, "
        f"all links and anchors resolve; docs/ holds {top} top-level documents "
        f"(cap {MAX_TOP_LEVEL_DOCS}) plus {generated} generated transcripts"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Keep the documentation navigable, and keep it from sprawling again.

Three invariants, all learned the hard way:

1. Every relative link and every in-page anchor resolves. Consolidating docs
   silently breaks links, and a broken link in a review-facing repo costs more
   credibility than the missing page would have.
2. `docs/` stays small. A reviewer who sees thirty near-duplicate documents reads
   it as generated bulk rather than as work, and they are not wrong to: the
   duplicates we merged in July 2026 included a stale threat model that
   contradicted the real one. Generated evidence lives in `docs/transcripts/`
   and is deliberately exempt - it is produced by the kernel, not written.
3. No public document denies a capability CI now proves. The honesty rule (D015)
   is usually read as "do not overclaim", and the linting effort went there. It
   cuts both ways: README carried "package checksums are deterministic v0
   checks, not production signatures" for weeks after `sig-demo` had been
   proving an Ed25519 envelope that binds requested authority. A reviewer who
   reads only README - which is most of them - came away with a worse picture of
   the system than the truth, and never asked a second question.

   This is a lint against *known-stale phrasings*, not a claim prover: it cannot
   discover that a document understates something, only that it still contains a
   sentence we have retired. When a capability lands, retire its old denial here
   in the same commit.

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

SMOKE = ROOT / "tools" / "ci" / "qemu_smoke.py"

# Sentences a shipped capability has since made false, each tied to the console
# demo that disproves it. Patterns are deliberately tight: docs legitimately
# describe what a capability replaced ("all device I/O *was* polled"), and a
# loose pattern that flags its own changelog is a check nobody keeps.
#
# (pattern, what it is about, the demo that disproves it)
RETIRED_CLAIMS: list[tuple[str, str, str]] = [
    (
        # `no package signing` is the same escape as the lease entry below: the
        # third alternative wants the word "implemented", and the whitepaper's
        # limitations list said it in two words instead. Bare noun-phrase
        # denials are how these sentences are actually written in a list.
        r"(checksums?[^.]{0,80}not production signatures"
        r"|not production signatures[^.]{0,80}checksums?"
        r"|package signing is (?:not|un)implemented"
        r"|no package signing)",
        "package signing",
        "sig-demo",
    ),
    (
        # `there is no lease/revocation` is here because it got through. STATUS
        # granted leases and `intent-revoke` in one bullet and denied them in
        # another sixty lines later, and shipped that way for weeks: the first
        # alternative below wants "there is no revocation" and the sentence read
        # "there is no lease/revocation", so one slash bought it a pass. The
        # docstring's warning that this lints phrasings rather than claims is
        # not a hedge - this is what it looks like when it bites.
        r"(there is no revocation"
        r"|there is no lease"
        r"|no lease\s*/\s*revocation"
        r"|revocation is (?:not implemented|absent|unimplemented)"
        r"|no (?:lease|revocation) (?:mechanism|semantics) exists?)",
        "intent lease and revocation",
        "lease-demo",
    ),
    (
        r"(device I/O is polled"
        r"|drivers (?:poll|spin) (?:for|on) (?:the )?(?:device|completion)"
        r"|the kernel is (?:not interrupt-driven|polled))",
        "interrupt-driven I/O",
        "irq-stat",
    ),
]


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


def check_retired_claims() -> list[str]:
    """Flag public docs that still deny a capability CI proves."""
    problems: list[str] = []
    smoke = SMOKE.read_text(encoding="utf-8")
    # Transcripts are kernel output, not prose: they record what a past run
    # printed and must never be edited to satisfy a lint.
    pages = [
        p
        for p in markdown_files()
        if "transcripts" not in p.relative_to(ROOT).parts
    ]
    for pattern, subject, demo in RETIRED_CLAIMS:
        # A claim is only retired for as long as CI still disproves it. If the
        # demo is gone, this entry is what is wrong, not the document - fail
        # loudly rather than keep enforcing a rule whose evidence vanished.
        if f'"{demo}"' not in smoke:
            problems.append(
                f"retired claim '{subject}' cites {demo}, which "
                f"tools/ci/qemu_smoke.py no longer runs; re-verify the "
                f"capability and update RETIRED_CLAIMS in this file"
            )
            continue
        rx = re.compile(pattern, re.IGNORECASE)
        for page in pages:
            for n, line in enumerate(page.read_text(encoding="utf-8").split("\n"), 1):
                if rx.search(line):
                    here = page.relative_to(ROOT).as_posix()
                    problems.append(
                        f"{here}:{n} denies {subject}, which `{demo}` proves "
                        f"in CI: {line.strip()[:80]}"
                    )
    return problems


def main() -> int:
    problems = check_links() + check_sprawl() + check_retired_claims()
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
        f"(cap {MAX_TOP_LEVEL_DOCS}) plus {generated} generated transcripts; "
        f"no document denies any of {len(RETIRED_CLAIMS)} CI-proven capabilities"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

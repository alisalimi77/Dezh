#!/usr/bin/env python3
"""Scan public-facing review files for non-neutral or private markers."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]

PUBLIC_PATHS = [
    ROOT / "README.md",
    ROOT / "LICENSE",
    ROOT / "NOTICE",
    ROOT / "SECURITY.md",
    ROOT / "CONTRIBUTING.md",
    ROOT / "CODE_OF_CONDUCT.md",
    ROOT / "CHANGELOG.md",
    ROOT / ".github",
    ROOT / "docs",
    ROOT / "tools" / "demo",
    ROOT / "tools" / "review",
]

FORBIDDEN_TERMS = [
    "".join(("Ir", "an")),
    "".join(("Ir", "an", "ian")),
    "".join(("Teh", "ran")),
    "".join(("Per", "sian")),
    "".join(("Far", "si")),
]

LOCAL_PATH_PATTERNS = [
    re.compile("[A-Za-z]" + chr(58) + re.escape(chr(92))),
    re.compile("/" + "home" + r"/[^/\s]+"),
    re.compile("/" + "Users" + r"/[^/\s]+"),
]

SECRET_PATTERNS = [
    re.compile(r"github_pat_[A-Za-z0-9_]+"),
    re.compile(r"ghp_[A-Za-z0-9_]+"),
    re.compile(r"AKIA[0-9A-Z]{16}"),
]


def iter_files() -> list[Path]:
    files: list[Path] = []
    for path in PUBLIC_PATHS:
        if path.is_file():
            files.append(path)
        elif path.is_dir():
            for child in path.rglob("*"):
                if "__pycache__" in child.parts:
                    continue
                if child.suffix in {".pyc", ".pyo"}:
                    continue
                if child.is_file():
                    files.append(child)
    return sorted(files)


def has_rtl_unicode(text: str) -> bool:
    for ch in text:
        code = ord(ch)
        if 0x0600 <= code <= 0x06FF:
            return True
        if 0x0750 <= code <= 0x077F:
            return True
        if 0x08A0 <= code <= 0x08FF:
            return True
    return False


def main() -> int:
    failures: list[str] = []
    for path in iter_files():
        rel = path.relative_to(ROOT)
        text = path.read_text(encoding="utf-8", errors="replace")
        if has_rtl_unicode(text):
            failures.append(f"{rel}: contains RTL Unicode code points")
        lower = text.lower()
        for term in FORBIDDEN_TERMS:
            if term.lower() in lower:
                failures.append(f"{rel}: contains restricted identity/geography term")
        for pattern in LOCAL_PATH_PATTERNS:
            if pattern.search(text):
                failures.append(f"{rel}: contains local filesystem path")
        for pattern in SECRET_PATTERNS:
            if pattern.search(text):
                failures.append(f"{rel}: contains secret-like token")

    if failures:
        print("public hygiene scan failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(f"public hygiene scan passed ({len(iter_files())} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

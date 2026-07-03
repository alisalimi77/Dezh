#!/usr/bin/env python3
"""Create a clean external-review snapshot under dist/."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "dist" / "dezh-review-v0"

EXCLUDE_DIRS = {
    ".git",
    "target",
    "dist",
    "graphify-out",
}

EXCLUDE_SUFFIXES = {
    ".img",
}


def should_skip(path: Path) -> bool:
    parts = set(path.relative_to(ROOT).parts)
    if parts & EXCLUDE_DIRS:
        return True
    if path.suffix in EXCLUDE_SUFFIXES:
        return True
    return False


def copy_tree() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    for path in ROOT.rglob("*"):
        if should_skip(path):
            continue
        rel = path.relative_to(ROOT)
        dest = OUT / rel
        if path.is_dir():
            dest.mkdir(parents=True, exist_ok=True)
        elif path.is_file():
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, dest)


def main() -> int:
    copy_tree()
    subprocess.run(["python", str(ROOT / "tools" / "review" / "scan_public.py")], check=True)
    print(f"review package written to {OUT}")
    print("Create an orphan public review branch or a separate repository from this snapshot.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Build Dezh review release artifacts under dist/release."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "dist" / "release"
RISC_V_KERNEL = ROOT / "dezh-boot" / "target" / "riscv64gc-unknown-none-elf" / "debug" / "dezh-boot"
X86_KERNEL = ROOT / "dezh-boot-x86" / "target" / "x86_64-unknown-none" / "debug" / "dezh-boot-x86"


def run(cmd: list[str]) -> None:
    print("+ " + " ".join(cmd), flush=True)
    subprocess.run(cmd, cwd=ROOT, check=True)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def copy_required(src: Path, dest_name: str) -> Path:
    if not src.exists():
        raise SystemExit(f"required artifact missing: {src}")
    dest = OUT / dest_name
    shutil.copy2(src, dest)
    return dest


def zip_docs(tag: str) -> Path:
    archive = OUT / f"dezh-{tag}-review-docs.zip"
    include = [
        ROOT / "README.md",
        ROOT / "LICENSE",
        ROOT / "NOTICE",
        ROOT / "SECURITY.md",
        ROOT / "CONTRIBUTING.md",
        ROOT / "CODE_OF_CONDUCT.md",
        ROOT / "CHANGELOG.md",
    ]
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        for path in include:
            if path.exists():
                zf.write(path, path.relative_to(ROOT))
        for path in sorted((ROOT / "docs").rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(ROOT))
    return archive


def build_sdk_packages(tag: str) -> list[Path]:
    packages: list[Path] = []
    templates = [ROOT / "tools" / "sdk" / "templates" / "hello"]
    for template in templates:
        name = template.name
        out = OUT / f"dezh-{tag}-{name}.dzp"
        run([sys.executable, "tools/sdk/build_pkg.py", str(template), "-o", str(out)])
        packages.append(out)
    return packages


def write_checksums(paths: list[Path]) -> Path:
    checksums = OUT / "SHA256SUMS"
    checksums.write_text(
        "".join(f"{sha256(path)}  {path.name}\n" for path in sorted(paths)),
        encoding="utf-8",
    )
    return checksums


def write_manifest(tag: str, paths: list[Path]) -> Path:
    owner = os.environ.get("GITHUB_REPOSITORY_OWNER", "alisalimi77")
    manifest = OUT / "release-manifest.json"
    payload = {
        "name": "Dezh OS",
        "tag": tag,
        "kind": "public-review-release",
        "generated_utc": datetime.now(timezone.utc).isoformat(),
        "artifacts": [
            {
                "name": path.name,
                "bytes": path.stat().st_size,
                "sha256": sha256(path),
            }
            for path in sorted(paths)
        ],
        "review_commands": [
            "python tools/review/run_full_review.py --quick",
            "python tools/review/run_full_review.py --full",
        ],
        "container_image": f"ghcr.io/{owner}/dezh-review-env:{tag}",
    }
    manifest.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag", default="v0.1-review")
    args = parser.parse_args()

    preserved_transcript: bytes | None = None
    transcript = OUT / "demo-transcript-riscv64.md"
    if transcript.exists():
        preserved_transcript = transcript.read_bytes()
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    if preserved_transcript is not None:
        transcript.write_bytes(preserved_transcript)

    artifacts = [
        copy_required(RISC_V_KERNEL, f"dezh-{args.tag}-riscv64-qemu-kernel.elf"),
        copy_required(X86_KERNEL, f"dezh-{args.tag}-x86_64-qemu-kernel.elf"),
    ]

    if transcript.exists():
        artifacts.append(transcript)

    artifacts.extend(build_sdk_packages(args.tag))
    artifacts.append(zip_docs(args.tag))
    manifest = write_manifest(args.tag, artifacts)
    artifacts.append(manifest)
    artifacts.append(write_checksums(artifacts))

    print(f"release artifacts written to {OUT}")
    for path in sorted(artifacts):
        print(f"  {path.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

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


def build_x86_iso(tag: str) -> Path | None:
    """Build the bootable x86_64 GRUB ISO (the VirtualBox/VMware artifact).

    Non-fatal if the GRUB tooling is absent (e.g. a Windows host): the release
    still ships the kernels, and the miss is logged loudly rather than hidden.
    On the Linux CI runner the tools are installed, so the ISO is produced.
    """
    iso = OUT / f"dezh-{tag}-x86_64.iso"
    script = ROOT / "tools" / "x86" / "build-iso.sh"
    if shutil.which("grub-mkrescue") is None or shutil.which("bash") is None:
        print(
            "WARNING: grub-mkrescue/bash not found; skipping bootable x86 ISO "
            "(install grub-pc-bin, grub2-common, xorriso, mtools to include it)",
            file=sys.stderr,
        )
        return None
    try:
        run(["bash", str(script), str(X86_KERNEL), str(iso)])
    except subprocess.CalledProcessError as exc:
        print(f"WARNING: x86 ISO build failed ({exc}); skipping", file=sys.stderr)
        return None
    return iso


def write_run_instructions(tag: str, have_iso: bool) -> Path:
    """A copy-paste RUN.txt so a stranger can boot the release with no repo."""
    riscv = f"dezh-{tag}-riscv64-qemu-kernel.elf"
    iso = f"dezh-{tag}-x86_64.iso"
    lines = [
        "Dezh OS — how to run this release in a VM",
        "=========================================",
        "",
        "RISC-V (QEMU one-liner):",
        "",
        f"  qemu-system-riscv64 -machine virt -nographic -bios default -kernel {riscv} \\",
        "    -drive file=dezh-disk.img,format=raw,if=none,id=hd0 \\",
        "    -device virtio-blk-device,drive=hd0",
        "",
        "  (create the disk once: `qemu-img create -f raw dezh-disk.img 4M`,",
        "   or run without the two -drive/-device lines to skip persistence.)",
        "  At the `dezh>` prompt try: caps, linux-elf, cairn-demo, agent, bench-pol, help.",
        "",
        "x86_64 in VirtualBox / VMware (bootable ISO):",
        "",
        f"  1. New VM, type 'Other/Unknown 64-bit', 128 MB RAM, no disk.",
        f"  2. Attach {iso} as the optical/CD drive.",
        "  3. Start the VM. The kernel boots to long mode and runs the .dzp agent",
        "     on screen (capability-gated: prints 15, then DENIED without the cap).",
        "",
        "x86_64 in QEMU (same ISO):",
        "",
        f"  qemu-system-x86_64 -cdrom {iso} -serial stdio",
        "",
    ]
    if not have_iso:
        lines.insert(
            0,
            "NOTE: this build did not include the x86 ISO (GRUB tooling absent at "
            "build time); build it with tools/x86/build-iso.sh.\n",
        )
    dest = OUT / "RUN.txt"
    dest.write_text("\n".join(lines), encoding="utf-8")
    return dest


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

    iso = build_x86_iso(args.tag)
    if iso is not None:
        artifacts.append(iso)
    artifacts.append(write_run_instructions(args.tag, iso is not None))

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

#!/usr/bin/env python3
"""Run the public Dezh OS review validation suite.

The quick mode is intended for a reviewer who wants a high-confidence pass in
one command. The full mode adds the longer SDK package lifecycle acceptance
test and review package generation.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
RISC_V_KERNEL = ROOT / "dezh-boot" / "target" / "riscv64gc-unknown-none-elf" / "debug" / "dezh-boot"
X86_KERNEL = ROOT / "dezh-boot-x86" / "target" / "x86_64-unknown-none" / "debug" / "dezh-boot-x86"
REVIEW_TRANSCRIPT = ROOT / "dist" / "review" / "demo-transcript-riscv64.md"


def default_qemu(name: str, explicit: str | None) -> str:
    if explicit:
        return explicit
    found = shutil.which(name)
    if found:
        return found
    if os.name == "nt":
        candidate = Path("C:/Program Files/qemu") / f"{name}.exe"
        if candidate.exists():
            return str(candidate)
    return name


def run(label: str, cmd: list[str], cwd: Path = ROOT) -> None:
    print(f"\n==> {label}", flush=True)
    print("+ " + " ".join(cmd), flush=True)
    subprocess.run(cmd, cwd=cwd, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description="Run Dezh public review validation")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--quick", action="store_true", help="Run the quick review suite")
    mode.add_argument("--full", action="store_true", help="Run the full review suite")
    parser.add_argument("--qemu-riscv", default=None, help="Path to qemu-system-riscv64")
    parser.add_argument("--qemu-x86", default=None, help="Path to qemu-system-x86_64")
    args = parser.parse_args()

    full = args.full
    qemu_riscv = default_qemu("qemu-system-riscv64", args.qemu_riscv)
    qemu_x86 = default_qemu("qemu-system-x86_64", args.qemu_x86)

    run("public hygiene scan", [sys.executable, "tools/review/scan_public.py"])
    run("host workspace tests", ["cargo", "test", "--locked", "--workspace"])
    run("build RISC-V kernel", ["cargo", "build", "--locked"], cwd=ROOT / "dezh-boot")
    run("build x86_64 kernel", ["cargo", "build", "--locked"], cwd=ROOT / "dezh-boot-x86")
    run(
        "RISC-V QEMU smoke",
        [
            sys.executable,
            "tools/ci/qemu_smoke.py",
            "riscv64",
            "--kernel",
            str(RISC_V_KERNEL),
            "--qemu",
            qemu_riscv,
        ],
    )
    run(
        "x86_64 QEMU smoke",
        [
            sys.executable,
            "tools/ci/qemu_smoke.py",
            "x86_64",
            "--kernel",
            str(X86_KERNEL),
            "--qemu",
            qemu_x86,
        ],
    )
    run(
        "review demo transcript",
        [
            sys.executable,
            "tools/demo/run_review_demo.py",
            "--kernel",
            str(RISC_V_KERNEL),
            "--qemu-riscv",
            qemu_riscv,
            "--mode",
            "short",
            "--no-build",
            "--transcript",
            str(REVIEW_TRANSCRIPT),
        ],
    )

    if full:
        run(
            "SDK package lifecycle acceptance",
            [
                sys.executable,
                "tools/ci/sdk_test.py",
                "--kernel",
                str(RISC_V_KERNEL),
                "--qemu",
                qemu_riscv,
            ],
        )
        run("review package snapshot", [sys.executable, "tools/review/make_review_package.py"])

    print("\nDezh review validation complete.")
    print(f"Transcript: {REVIEW_TRANSCRIPT}")
    if not full:
        print("Run with --full to add SDK lifecycle acceptance and review package generation.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

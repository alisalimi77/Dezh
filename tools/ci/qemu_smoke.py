#!/usr/bin/env python3
"""QEMU smoke tests for Dezh bare-metal kernels.

This script is intentionally stricter than "QEMU exited": it waits for real
kernel output and fails if expected capability, isolation, or IR signals are
missing from the transcript.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import threading
import time
from pathlib import Path


class QemuSession:
    def __init__(self, cmd: list[str], timeout: float) -> None:
        self.timeout = timeout
        self.output = bytearray()
        self.proc = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            bufsize=0,
        )
        self.reader = threading.Thread(target=self._read_output, daemon=True)
        self.reader.start()

    def _read_output(self) -> None:
        assert self.proc.stdout is not None
        while True:
            chunk = self.proc.stdout.read(1)
            if not chunk:
                return
            self.output.extend(chunk)

    def text(self) -> str:
        return self.output.decode("utf-8", errors="replace")

    def wait_for(self, needle: str, timeout: float | None = None) -> None:
        deadline = time.monotonic() + (timeout or self.timeout)
        while time.monotonic() < deadline:
            if needle in self.text():
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.05)
        raise AssertionError(f"timed out waiting for {needle!r}")

    def send_line(self, line: str) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((line + "\n").encode("ascii"))
        self.proc.stdin.flush()

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=2)


def run_riscv64(qemu: str, kernel: Path) -> None:
    session = QemuSession(
        [
            qemu,
            "-machine",
            "virt",
            "-nographic",
            "-bios",
            "default",
            "-kernel",
            str(kernel),
        ],
        timeout=30,
    )
    try:
        session.wait_for("boot contract VALIDATED")
        session.wait_for("Dezh console. Every command requires an explicit capability.")

        commands = [
            ("caps", "console capabilities: INSPECT TIME ECHO HALT SPAWN"),
            ("secret", "denied: 'secret' requires capability SECRET"),
            ("run", "sys_uptime was DENIED (task holds no TIME capability)"),
            ("rogue", "rogue task handled; console survived"),
            ("ipc", "[service] <payload delivered with a delegated PRINT cap>"),
            ("linux", "unsupported syscall, denied cleanly"),
            ("halt", "halting."),
        ]
        for command, expected in commands:
            session.wait_for("dezh> ")
            session.send_line(command)
            session.wait_for(expected)

        exit_code = session.proc.wait(timeout=10)
        if exit_code != 0:
            raise AssertionError(f"QEMU exited with {exit_code}, expected 0")
    finally:
        transcript = session.text()
        print(transcript)
        session.stop()


def run_x86_64(qemu: str, kernel: Path) -> None:
    session = QemuSession(
        [
            qemu,
            "-display",
            "none",
            "-serial",
            "stdio",
            "-no-reboot",
            "-kernel",
            str(kernel),
        ],
        timeout=20,
    )
    try:
        session.wait_for("Dezh x86_64")
        session.wait_for("long mode reached. 64-bit kernel running.")
        session.wait_for("Dezh-IR agent (sum 1..=5 with a loop) on x86_64:")
        session.wait_for("[ir] => 15")
        session.wait_for("[ir] DENIED: agent holds no PRINT capability")
    finally:
        transcript = session.text()
        print(transcript)
        session.stop()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=["riscv64", "x86_64"])
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--qemu", required=True)
    args = parser.parse_args()

    if not args.kernel.exists():
        print(f"kernel not found: {args.kernel}", file=sys.stderr)
        return 2

    try:
        if args.target == "riscv64":
            run_riscv64(args.qemu, args.kernel)
        else:
            run_x86_64(args.qemu, args.kernel)
    except Exception as exc:
        print(f"qemu smoke failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

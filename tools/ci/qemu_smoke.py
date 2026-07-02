#!/usr/bin/env python3
"""QEMU smoke tests for Dezh bare-metal kernels.

This script is intentionally stricter than "QEMU exited": it waits for real
kernel output and fails if expected capability, isolation, or IR signals are
missing from the transcript.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
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
        tail = self.text()[-3000:]
        raise AssertionError(f"timed out waiting for {needle!r}\n--- transcript tail ---\n{tail}")

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
    disk = tempfile.NamedTemporaryFile(prefix="dezh-disk-", suffix=".img", delete=False)
    disk_path = Path(disk.name)
    try:
        disk.truncate(2 * 1024 * 1024)
    finally:
        disk.close()
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
            "-drive",
            f"file={disk_path},format=raw,if=none,id=dezhdisk",
            "-device",
            "virtio-blk-device,drive=dezhdisk",
        ],
        timeout=60,
    )
    try:
        session.wait_for("boot contract VALIDATED")
        session.wait_for("service registry built from boot plan")
        session.wait_for("Dezh console. Every command requires an explicit capability.")

        commands = [
            ("caps", "console capabilities: INSPECT TIME ECHO HALT SPAWN"),
            ("status", "status:"),
            ("secret", "denied: 'secret' requires capability SECRET"),
            ("run", "sys_uptime was DENIED (task holds no TIME capability)"),
            ("rogue", "rogue task handled; console survived"),
            ("ipc", "[service] <payload delivered with a delegated PRINT cap>"),
            ("ipcq", "FIFO mailbox preserved both client messages"),
            ("queues", "queue demo done; back in the console"),
            ("linux", "unsupported syscall, denied cleanly"),
            ("services", "VirtioBlock state=Running"),
            ("tasks", "service=virtio-block"),
            ("install-check", "install-check: no Dezh root marker yet"),
            ("install-init", "install-init status=0"),
            ("root-status", "root metadata = \"DEZHROOT v0"),
            ("root", "installed root marker found"),
            ("apps available", "[available] note"),
            ("apps installed", "[installed] none"),
            ("app-run note", "note not installed"),
            ("app-install note", "installed note version=0.1.0 state=Active"),
            ("apps installed", "[installed] note"),
            ("app-run note", "[note] running with caps=PRINT,IPC only"),
            ("note-set hello-note", "note-set status=0"),
            ("note-get", "note value = \"hello-note"),
            ("app-deny note", "note device/block direct access denied; console survived"),
            ("app-remove note", "removed note state=Removed status=0"),
            ("app-run note", "note not installed or not active; launch denied"),
            ("app-install lab", "installed lab version=0.1.0 state=Active"),
            (
                "app-run lab",
                [
                    "Dezh Lab :: installable app system probe",
                    "[lab-ui] worker signals received=2",
                    "[lab-ui] PASS: scheduler, IPC, installer launch, and UI path cooperated",
                    "lab value = \"lab-run-complete",
                ],
            ),
            ("lab-set manual-lab-value", "lab-set status=0"),
            ("lab-get", "lab value = \"manual-lab-value"),
            ("app-deny lab", "lab device/block direct access denied; console survived"),
            ("disk", "disk probe via registered daemon status=0"),
            ("disk", "no-grant probe returned; console survived"),
            ("bwrite", "bwrite via registered daemon status=0"),
            ("bread", "test sector = \"DEZH-DAEMON-BLOCK-OK"),
            ("write hello-interactive", "cairn set via registered daemon status=0"),
            ("read", "cairn current = \"hello-interactive"),
            ("history", "previous value is used by rollback"),
            ("pset ci-value", "cairn set via registered daemon status=0"),
            ("pget", "cairn current = \"ci-value"),
            ("pset bad-edit", "cairn set via registered daemon status=0"),
            ("prollback", "rollback restored current = \"ci-value"),
            ("deny", "Pol denial demo skipped here to keep running services alive"),
            (
                "bench-all",
                [
                    "[bench-os] syscall boundary complete",
                    "[bench-ipc-service] received messages=32",
                    "[bench-storage] complete via user-space virtio-block daemon",
                    "[bench-caps] TIME denied as expected",
                    "[bench-all] PASS: syscall, IPC, storage, caps, and service liveness checked",
                ],
            ),
            (
                "vblkd",
                [
                    "vblkd uses registered daemon task=",
                    "vblk-client] test sector via daemon = \"DEZH-DAEMON-BLOCK-OK",
                    "vblk-client] rollback via daemon restored = \"daemon-ci-value",
                    "virtio-blk daemon demo done; back in the console",
                ],
            ),
            ("halt", "halting."),
        ]
        for command, expected in commands:
            session.wait_for("dezh> ")
            session.send_line(command)
            if isinstance(expected, list):
                for needle in expected:
                    session.wait_for(needle)
            else:
                session.wait_for(expected)

        exit_code = session.proc.wait(timeout=10)
        if exit_code != 0:
            raise AssertionError(f"QEMU exited with {exit_code}, expected 0")
    finally:
        transcript = session.text()
        print(transcript)
        session.stop()
        try:
            os.unlink(disk_path)
        except OSError:
            pass


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
        msg = str(exc).replace("%", "%25").replace("\n", "%0A").replace("\r", "%0D")
        print(f"::error title=QEMU smoke failed::{msg}", file=sys.stderr)
        print(f"qemu smoke failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""install-pkg — install a `.dzp` package into a live Dezh system over the UART.

Boots the Dezh RISC-V kernel under QEMU, streams the package as base64 lines
through the capability-gated console (`pkg-recv`), and optionally runs it.
The kernel checks package integrity (CRC-32), statically verifies Dezh-IR
payloads, and records the manifest capability grants at install time.

Usage:
    install_pkg.py hello-0.1.0.dzp --run hello
    install_pkg.py a.dzp b.dzp --run a --run b --interactive-halt
"""

from __future__ import annotations

import argparse
import base64
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_KERNEL = (
    REPO / "dezh-boot" / "target" / "riscv64gc-unknown-none-elf" / "debug" / "dezh-boot"
)

CHUNK_RAW_BYTES = 30  # 30 raw -> 40 base64 chars; conservative for QEMU UART pipes


class QemuConsole:
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
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self) -> None:
        assert self.proc.stdout is not None
        while True:
            chunk = self.proc.stdout.read(1)
            if not chunk:
                return
            self.output.extend(chunk)

    def text(self) -> str:
        return self.output.decode("utf-8", errors="replace")

    def wait_for(self, needle: str, since: int = 0, timeout: float | None = None) -> int:
        deadline = time.monotonic() + (timeout or self.timeout)
        while time.monotonic() < deadline:
            idx = self.text().find(needle, since)
            if idx >= 0:
                return idx + len(needle)
            if self.proc.poll() is not None:
                break
            time.sleep(0.02)
        tail = self.text()[-2000:]
        raise RuntimeError(f"timed out waiting for {needle!r}\n--- tail ---\n{tail}")

    def send_line(self, line: str) -> int:
        assert self.proc.stdin is not None
        mark = len(self.output)
        self.proc.stdin.write((line + "\n").encode("ascii"))
        self.proc.stdin.flush()
        return mark

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()


def upload_package(
    console: QemuConsole,
    package: Path,
    command: str = "pkg-recv",
    success: str = "[pkg] installed",
) -> None:
    data = package.read_bytes()
    mark = console.send_line(command)
    console.wait_for(f"[{command.split()[0]}] ready", since=mark)
    sent = 0
    for i in range(0, len(data), CHUNK_RAW_BYTES):
        chunk = base64.b64encode(data[i : i + CHUNK_RAW_BYTES]).decode("ascii")
        mark = console.send_line(chunk)
        sent += min(CHUNK_RAW_BYTES, len(data) - i)
        console.wait_for(f"+ok {sent}", since=mark)
    mark = console.send_line(".")
    end = console.wait_for("dezh> ", since=mark)
    window = console.text()[mark:end]
    if success not in window:
        raise RuntimeError(f"package command rejected:\n{window}")
    print(f"install-pkg: {command} accepted {package.name} ({len(data)} bytes)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("packages", nargs="+", type=Path)
    parser.add_argument("--kernel", type=Path, default=DEFAULT_KERNEL)
    parser.add_argument("--qemu", default="qemu-system-riscv64")
    parser.add_argument(
        "--run", action="append", default=[], metavar="NAME",
        help="after installing, pkg-run this package name (repeatable)",
    )
    parser.add_argument(
        "--command", action="append", default=[], metavar="CMD",
        help="extra console command to run after installs (repeatable)",
    )
    parser.add_argument("--transcript", action="store_true", help="print the full transcript")
    parser.add_argument("--timeout", type=float, default=60.0)
    parser.add_argument(
        "--persistent-disk",
        type=Path,
        help="use this raw disk image instead of a temporary one; create it if missing",
    )
    args = parser.parse_args()

    if not args.kernel.exists():
        print(
            f"install-pkg: kernel not found at {args.kernel}\n"
            "  build it first:  cd dezh-boot && cargo build",
            file=sys.stderr,
        )
        return 2
    for package in args.packages:
        if not package.exists():
            print(f"install-pkg: no such package {package}", file=sys.stderr)
            return 2

    delete_disk = args.persistent_disk is None
    if args.persistent_disk is not None:
        args.persistent_disk.parent.mkdir(parents=True, exist_ok=True)
        if not args.persistent_disk.exists():
            with args.persistent_disk.open("wb") as fh:
                fh.truncate(2 * 1024 * 1024)
        disk_name = str(args.persistent_disk)
    else:
        disk = tempfile.NamedTemporaryFile(prefix="dezh-disk-", suffix=".img", delete=False)
        disk.truncate(2 * 1024 * 1024)
        disk.close()
        disk_name = disk.name

    console = QemuConsole(
        [
            args.qemu,
            "-machine", "virt",
            "-nographic",
            "-bios", "default",
            "-kernel", str(args.kernel),
            "-drive", f"file={disk_name},format=raw,if=none,id=dezhdisk",
            "-device", "virtio-blk-device,drive=dezhdisk",
        ],
        timeout=args.timeout,
    )
    try:
        console.wait_for("Dezh console. Every command requires an explicit capability.")
        console.wait_for("dezh> ")
        for package in args.packages:
            upload_package(console, package)
        for name in args.run:
            mark = console.send_line(f"pkg-run {name}")
            end = console.wait_for("dezh> ", since=mark)
            print(f"--- pkg-run {name} ---")
            print(console.text()[mark:end].rsplit("dezh>", 1)[0].strip())
        for command in args.command:
            mark = console.send_line(command)
            end = console.wait_for("dezh> ", since=mark)
            print(f"--- {command} ---")
            print(console.text()[mark:end].rsplit("dezh>", 1)[0].strip())
        console.send_line("halt")
        code = console.proc.wait(timeout=10)
        if code != 0:
            raise RuntimeError(f"QEMU exited with {code}")
    except Exception as exc:
        print(f"install-pkg: FAILED: {exc}", file=sys.stderr)
        if args.transcript:
            print(console.text())
        console.stop()
        return 1
    finally:
        console.stop()
        if delete_disk:
            Path(disk_name).unlink(missing_ok=True)

    if args.transcript:
        print(console.text())
    print("install-pkg: done (clean halt, exit 0)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

#!/usr/bin/env python3
"""Run the Dezh OS external-review demo and write a clean transcript."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_KERNEL = ROOT / "dezh-boot" / "target" / "riscv64gc-unknown-none-elf" / "debug" / "dezh-boot"


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

    def wait_for(self, needle: str, timeout: float | None = None, since: int = 0) -> int:
        deadline = time.monotonic() + (timeout or self.timeout)
        while time.monotonic() < deadline:
            idx = self.text().find(needle, since)
            if idx >= 0:
                return idx + len(needle)
            if self.proc.poll() is not None:
                break
            time.sleep(0.05)
        tail = self.text()[-3000:]
        raise AssertionError(f"timed out waiting for {needle!r}\n--- transcript tail ---\n{tail}")

    def send_line(self, line: str) -> int:
        assert self.proc.stdin is not None
        start = len(self.output)
        self.proc.stdin.write((line + "\n").encode("ascii"))
        self.proc.stdin.flush()
        return start

    def stop(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=2)


def run(cmd: list[str], cwd: Path) -> None:
    print("+ " + " ".join(cmd), flush=True)
    subprocess.run(cmd, cwd=cwd, check=True)


def find_qemu(explicit: str | None) -> str:
    if explicit:
        return explicit
    candidates = ["qemu-system-riscv64"]
    if os.name == "nt":
        candidates.append(str(Path("C:" + "\\") / "Program Files" / "qemu" / "qemu-system-riscv64.exe"))
    for candidate in candidates:
        if Path(candidate).exists():
            return candidate
        found = shutil_which(candidate)
        if found:
            return found
    raise SystemExit("qemu-system-riscv64 not found; pass --qemu-riscv")


def shutil_which(name: str) -> str | None:
    paths = os.environ.get("PATH", "").split(os.pathsep)
    exts = [""]
    if os.name == "nt":
        exts.extend(os.environ.get("PATHEXT", ".EXE;.BAT;.CMD").split(os.pathsep))
    for directory in paths:
        for ext in exts:
            p = Path(directory) / (name + ext)
            if p.exists() and p.is_file():
                return str(p)
    return None


def command_plan(mode: str) -> list[tuple[str, str | list[str]]]:
    base: list[tuple[str, str | list[str]]] = [
        (
            "ipc-typed-demo",
            [
                "[typed-ipc] PING -> 0",
                "[typed-ipc] BADREQ -> 4",
                "[typed-ipc] RECV_TIMEOUT -> 3",
                "[typed-ipc] PASS",
            ],
        ),
        ("ipcstat", "timeouts="),
        ("services", "VirtioBlock state=Running"),
        ("install --dry-run", "dry-run complete; disk not modified"),
        (
            "install run",
            [
                "Install Plan: Dezh Root v1",
                "Install Report: Dezh Root v1",
                "install.run",
            ],
        ),
        ("app-permissions lab", "DENIED     DEVICE_VIRTIO_BLK"),
        (
            "app-run lab",
            [
                "Dezh Lab :: installable app system probe",
                "PASS: scheduler, IPC, installer launch, and UI path cooperated",
                "lab value = \"lab-run-complete",
            ],
        ),
        ("calc 7 + 5", "[calc] 7 + 5 = 12"),
        ("calc-history", "calc last = \"7 + 5 = 12"),
        ("vault-put demo-secret", "vault-put status=0"),
        ("vault-get", "vault value = \"demo-secret"),
        ("app-deny vault", "vault device/block direct access denied; console survived"),
        ("svc-stop virtio-block", "svc-stop virtio-block status=0 state=Stopped"),
        ("read", "virtio-block unavailable; command failed cleanly"),
        ("svc-restart virtio-block", "svc-restart virtio-block state=Running"),
        ("write recovered", "cairn set via registered daemon status=0"),
        ("read", "cairn current = \"recovered"),
        ("svc-fault-demo virtio-block", "svc-fault-demo virtio-block request_status=0 state=Faulted"),
        ("read", "virtio-block unavailable; command failed cleanly"),
        ("svc-restart virtio-block", "svc-restart virtio-block state=Running"),
        # Flagship F2: Cairn v1 commit log, rollback, and namespace denial.
        (
            "cairn-demo",
            [
                "[cairn-demo] 4/6 rollback one step restores the previous commit",
                "[cairn] DENIED: ns=note requires capability CAIRN_NS_0",
                "[cairn-demo] PASS",
            ],
        ),
        ("cairn-log note", "reversible=yes"),
        ("cairn-status", "ns=note cap=CAIRN_NS_0"),
    ]
    if mode == "full":
        base.extend(
            [
                ("disk", "no-grant probe returned; console survived"),
                (
                    "bench-all",
                    [
                        "[bench-storage] complete via user-space virtio-block daemon",
                        "[bench-all] PASS",
                    ],
                ),
            ]
        )
    base.append(("halt", "halting."))
    return base


ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def clean_transcript(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = ANSI_RE.sub("", text)
    return text.strip() + "\n"


def write_transcript(path: Path, mode: str, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = clean_transcript(text)
    path.write_text(
        "# Dezh OS RISC-V Review Demo Transcript\n\n"
        f"Mode: `{mode}`\n\n"
        "This transcript is generated by `tools/demo/run_review_demo.py`.\n\n"
        "```text\n"
        + body
        + "```\n",
        encoding="utf-8",
    )


def run_demo(qemu: str, kernel: Path, mode: str, transcript: Path) -> None:
    disk = tempfile.NamedTemporaryFile(prefix="dezh-review-disk-", suffix=".img", delete=False)
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
        cursor = session.wait_for("dezh> ")
        for command, expected in command_plan(mode):
            start = session.send_line(command)
            if isinstance(expected, list):
                for needle in expected:
                    session.wait_for(needle, since=start)
            else:
                session.wait_for(expected, since=start)
            if command != "halt":
                cursor = session.wait_for("dezh> ", since=start)
        exit_code = session.proc.wait(timeout=10)
        if exit_code != 0:
            raise AssertionError(f"QEMU exited with {exit_code}, expected 0")
    finally:
        output = session.text()
        print(output)
        write_transcript(transcript, mode, output)
        session.stop()
        try:
            os.unlink(disk_path)
        except OSError:
            pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=["short", "full"], default="full")
    parser.add_argument("--qemu-riscv")
    parser.add_argument("--kernel", type=Path, default=DEFAULT_KERNEL)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument(
        "--transcript",
        type=Path,
        default=ROOT / "docs" / "demo-transcript-riscv64.md",
    )
    args = parser.parse_args()

    qemu = find_qemu(args.qemu_riscv)
    if not args.no_build:
        run(["cargo", "build", "--locked"], ROOT / "dezh-boot")
    if not args.kernel.exists():
        raise SystemExit(f"kernel not found: {args.kernel}")
    run_demo(qemu, args.kernel, args.mode, args.transcript)
    print(f"wrote transcript: {args.transcript}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

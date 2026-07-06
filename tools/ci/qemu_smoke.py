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
            ("version", "v0.2-control-surface"),
            ("about", "capability-secure research prototype"),
            ("status", "status:"),
            ("memstat", "owned: process="),
            ("help install", "usage: install"),
            ("explain install run", "path: boot manifest"),
            ("install --dry-run", "dry-run complete; disk not modified"),
            (
                "ipc-typed-demo",
                [
                    "[typed-ipc] PING -> 0",
                    "[typed-ipc] BADREQ -> 4",
                    "[typed-ipc] RECV_TIMEOUT -> 3",
                    "[typed-ipc] no-IPC SEND -> 1",
                    "[typed-ipc] PASS: OK=OK, BAD_REQUEST=BAD_REQUEST, TIMEOUT=TIMEOUT, DENIED=DENIED",
                ],
            ),
            ("ipcstat", "timeouts="),
            ("secret", "denied: 'secret' requires capability SECRET"),
            ("run", "sys_uptime was DENIED (task holds no TIME capability)"),
            ("rogue", "rogue task handled; console survived"),
            ("ipc", "[service] <payload delivered with a delegated PRINT cap>"),
            ("ipcq", "FIFO mailbox preserved both client messages"),
            ("queues", "queue demo done; back in the console"),
            ("linux", "unsupported syscall, denied cleanly"),
            (
                "linux-elf",
                [
                    "loading a REAL unmodified static Linux/RISC-V ELF",
                    "[linux] hello from an unmodified static riscv64 Linux ELF",
                    "getpid() -> -ENOSYS: unsupported syscall, denied cleanly",
                    "write(fd=1) DENIED: task lacks PRINT capability",
                    "also runs on real riscv64 Linux",
                ],
            ),
            ("services", "VirtioBlock state=Running"),
            ("tasks", "service=virtio-block"),
            ("install-check", "install-check: no Dezh root marker yet"),
            (
                "install run",
                [
                    "Install Plan: Dezh Root v1",
                    "[install-v1] verifying root marker, metadata, and base app registry",
                    "Install Report: Dezh Root v1",
                    "install.run",
                ],
            ),
            ("events", "install.run"),
            ("audit", "audit summary:"),
            ("install-init", "install-init status=0"),
            ("root-status", "root metadata = \"DEZHROOT v0"),
            ("root", "installed root marker found"),
            ("apps available", "[available] note"),
            ("apps available", "[available] calc"),
            ("apps available", "[available] vault"),
            ("apps installed", "[installed] note"),
            ("apps installed", "[installed] calc"),
            ("apps installed", "[installed] vault"),
            ("app-install note", "already installed note version=0.1.0 state=Active"),
            ("apps installed", "[installed] note"),
            ("app-permissions note", "DENIED     DEVICE_VIRTIO_BLK"),
            ("app-run note", "[note] running with caps=PRINT,IPC only"),
            ("note-set hello-note", "note-set status=0"),
            ("note-get", "note value = \"hello-note"),
            ("app-deny note", "note device/block direct access denied; console survived"),
            ("app-remove note", "removed note state=Removed status=0"),
            ("app-run note", "note not installed or not active; launch denied"),
            ("app-install lab", "already installed lab version=0.1.0 state=Active"),
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
            ("app-install calc", "already installed calc version=0.1.0 state=Active"),
            ("app-run calc", "Dezh Calc :: installed U-mode app"),
            ("calc 7 + 5", "[calc] 7 + 5 = 12"),
            ("calc-history", "calc last = \"7 + 5 = 12"),
            ("app-permissions calc", "DENIED     DEVICE_VIRTIO_BLK"),
            ("app-install vault", "already installed vault version=0.1.0 state=Active"),
            ("app-run vault", "Dezh Vault :: private app storage"),
            ("vault-put alpha-secret", "vault-put status=0"),
            ("vault-get", "vault value = \"alpha-secret"),
            ("app-permissions vault", "DENIED     DEVICE_VIRTIO_BLK"),
            ("app-deny vault", "vault device/block direct access denied; console survived"),
            ("stress-lab", "PASS: free frames stable"),
            ("services", "VirtioBlock state=Running"),
            ("svc-stop virtio-block", "svc-stop virtio-block status=0 state=Stopped"),
            ("read", "virtio-block unavailable; command failed cleanly"),
            ("svc-restart virtio-block", "svc-restart virtio-block state=Running restart_count=1"),
            ("write after-restart", "cairn set via registered daemon status=0"),
            ("read", "cairn current = \"after-restart"),
            ("svc-fault-demo virtio-block", "svc-fault-demo virtio-block request_status=0 state=Faulted"),
            ("read", "virtio-block unavailable; command failed cleanly"),
            ("svc-restart virtio-block", "svc-restart virtio-block state=Running restart_count=2"),
            ("disk", "disk probe via registered daemon status=0"),
            ("disk", "no-grant probe returned; console survived"),
            ("bwrite", "bwrite via registered daemon status=0"),
            ("bread", "test sector = \"DEZH-DAEMON-BLOCK-OK"),
            ("write hello-interactive", "cairn set via registered daemon status=0"),
            ("read", "cairn current = \"hello-interactive"),
            ("history", "for the full commit history use `cairn-log <ns>` (Cairn v1)"),
            ("pset ci-value", "cairn set via registered daemon status=0"),
            ("pget", "cairn current = \"ci-value"),
            ("pset bad-edit", "cairn set via registered daemon status=0"),
            ("prollback", "rollback restored current = \"ci-value"),
            # --- Cairn v1 (W2 / flagship F2): commit log + namespace caps ---
            ("cairn-status", "ns=note cap=CAIRN_NS_0"),
            ("cairn-commit note ci-note-v1", "cairn-commit status=0"),
            ("cairn-commit note ci-note-v2", "commit ns=note slot="),
            ("cairn-get note", "cairn value = \"ci-note-v2"),
            ("cairn-log note", "reversible=yes"),
            ("cairn-commit note ci-bad-write", "cairn-commit status=0"),
            ("cairn-get note", "cairn value = \"ci-bad-write"),
            ("cairn-rollback note 1", "history preserved: rollback moves the ref"),
            ("cairn-get note", "cairn value = \"ci-note-v2"),
            ("cairn-verify note", "hash MATCH"),
            ("cairn-commit vault ci-vault-secret", "commit ns=vault"),
            (
                "agent",
                [
                    "[ir] print -> 15",
                    "missing required capability for this host call",
                    "[cairn] commit ns=agent",
                    "[ir] ir-wrote-this-durably",
                ],
            ),
            (
                "cairn-demo",
                [
                    "[cairn-demo] 5/6 cross-namespace access must be DENIED",
                    "[cairn] DENIED: ns=note requires capability CAIRN_NS_0",
                    "DENIED by storage service (kernel-attested caps)",
                    "[cairn-demo] PASS",
                ],
            ),
            ("events", "cairn.demo"),
            ("deny", "Pol denial demo skipped here to keep running services alive"),
            (
                "bench-pol",
                [
                    "native SYS_PRINT round-trip:",
                    "Pol Linux write(2) round-trip:",
                    "Pol translation overhead:",
                ],
            ),
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
        cursor = session.wait_for("dezh> ")
        for command, expected in commands:
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
        transcript = session.text()
        print(transcript)
        session.stop()

    # Second boot on the SAME disk: Cairn v1 state must survive a reboot
    # (F2 acceptance: rollback-restored value + hash verify after power cycle).
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
        session.wait_for("Dezh console. Every command requires an explicit capability.")
        session.wait_for("dezh> ")
        reboot_commands = [
            ("cairn-get note", "cairn value = \"note-v2"),
            ("cairn-get vault", "cairn value = \"ci-vault-secret"),
            ("cairn-verify note", "hash MATCH"),
            ("cairn-log note", "reversible=yes"),
            ("halt", "halting."),
        ]
        for command, expected in reboot_commands:
            start = session.send_line(command)
            session.wait_for(expected, since=start)
            if command != "halt":
                session.wait_for("dezh> ", since=start)
        exit_code = session.proc.wait(timeout=10)
        if exit_code != 0:
            raise AssertionError(f"QEMU (reboot) exited with {exit_code}, expected 0")
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
        session.wait_for("Dezh .dzp agent package (sum 1..=5 with a loop) on x86_64:")
        session.wait_for(".dzp verified: kind=dezh-ir, name=agent-sum")
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

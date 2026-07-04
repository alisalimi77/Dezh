#!/usr/bin/env python3
"""Run the F1 agent-containment demo end to end and write a transcript.

The flow a reviewer sees:

  1. attenuated delegation over IPC (an agent grants a no-authority service
     exactly one capability; the kernel enforces granted = requested & held);
  2. an out-of-tree `agent` app is built with the SDK, uploaded over the
     UART, and installed with manifest-scoped grants (its own Cairn
     namespace only);
  3. the agent works inside its grant: durable commits to ns=agent, then a
     bad write (an agent gone wrong);
  4. the operator undoes the damage with a one-step rollback (history kept),
     verified by re-hashing the head object;
  5. a second app (`spy`) that declares no capabilities is DENIED by the
     kernel the moment it tries to print;
  6. after a reboot, the rolled-back state is still what the disk answers.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "sdk"))

import build_pkg  # noqa: E402
import install_pkg  # noqa: E402

DEFAULT_KERNEL = REPO / "dezh-boot" / "target" / "riscv64gc-unknown-none-elf" / "debug" / "dezh-boot"

SPY_TOML = """\
name = "spy"
version = "0.1.0"
kind = "dezh-ir"
entry = "spy.dzs"
caps = []
"""

SPY_DZS = """\
; spy declares NO capabilities, then tries to print -> kernel must DENY.
    push 31337
    hostcall print_num
    halt
"""

ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def clean_transcript(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = ANSI_RE.sub("", text)
    return text.strip() + "\n"


def send_expect(console, line: str, needles) -> None:
    mark = console.send_line(line)
    for needle in needles if isinstance(needles, list) else [needles]:
        console.wait_for(needle, since=mark)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kernel", type=Path, default=DEFAULT_KERNEL)
    parser.add_argument("--qemu", default="qemu-system-riscv64")
    parser.add_argument(
        "--transcript",
        type=Path,
        default=REPO / "docs" / "demo-transcript-agent-f1.md",
    )
    args = parser.parse_args()

    work = Path(tempfile.mkdtemp(prefix="dezh-agent-demo-"))
    transcript_parts: list[str] = []
    try:
        agent_dir = work / "agent"
        shutil.copytree(REPO / "tools" / "sdk" / "templates" / "agent", agent_dir)
        agent_pkg = build_pkg.build(agent_dir, None)

        spy_dir = work / "spy"
        spy_dir.mkdir()
        (spy_dir / "app.toml").write_text(SPY_TOML, encoding="utf-8")
        (spy_dir / "spy.dzs").write_text(SPY_DZS, encoding="utf-8")
        spy_pkg = build_pkg.build(spy_dir, None)

        disk = work / "disk.img"
        with disk.open("wb") as fh:
            fh.truncate(2 * 1024 * 1024)

        def boot() -> install_pkg.QemuConsole:
            return install_pkg.QemuConsole(
                [
                    args.qemu,
                    "-machine", "virt",
                    "-nographic",
                    "-bios", "default",
                    "-kernel", str(args.kernel),
                    "-drive", f"file={disk},format=raw,if=none,id=dezhdisk",
                    "-device", "virtio-blk-device,drive=dezhdisk",
                ],
                timeout=60,
            )

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            # 1. Attenuated delegation (before services, so task slots are free).
            send_expect(console, "ipc", [
                "[service] <payload delivered with a delegated PRINT cap>",
                "IPC demo done",
            ])

            # 2. SDK app upload + manifest-scoped install.
            install_pkg.upload_package(console, agent_pkg)
            install_pkg.upload_package(console, spy_pkg)
            send_expect(console, "pkg-info agent", [
                "GRANTED  print cairn-read cairn-write",
                "DENIED   ipc uptime",
            ])

            # 3. In-grant agent work: durable commits, then a bad write.
            send_expect(console, "pkg-run agent", [
                "agent online, caps checked by kernel",
                "[cairn] commit ns=agent",
                "agent-note-BAD",
                "[pkg-run] 'agent' finished",
            ])
            send_expect(console, "cairn-log agent", "reversible=yes")

            # 4. The operator undoes the damage; verify re-hashes the object.
            send_expect(console, "cairn-rollback agent 1",
                        "history preserved: rollback moves the ref")
            send_expect(console, "cairn-get agent", 'cairn value = "agent-note-good')
            send_expect(console, "cairn-verify agent", "hash MATCH")

            # 5. No-capability app is DENIED by the kernel.
            send_expect(console, "pkg-run spy",
                        "DENIED by kernel: missing required capability")

            send_expect(console, "events", "cairn.rollback")

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU exited with {code}, expected 0")
        finally:
            transcript_parts.append(console.text())
            print(console.text())
            console.stop()

        # 6. Reboot: the rolled-back state is what the disk answers.
        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")
            send_expect(console, "cairn-get agent", 'cairn value = "agent-note-good')
            send_expect(console, "cairn-verify agent", "hash MATCH")
            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU (reboot) exited with {code}, expected 0")
        finally:
            transcript_parts.append(console.text())
            print(console.text())
            console.stop()

        body = "\n\n--- reboot ---\n\n".join(
            clean_transcript(part) for part in transcript_parts
        )
        args.transcript.parent.mkdir(parents=True, exist_ok=True)
        args.transcript.write_text(
            "# Dezh OS F1 Agent-Containment Demo Transcript\n\n"
            "Generated by `tools/demo/run_agent_demo.py`.\n\n"
            "Flow: attenuated delegation over IPC; SDK-built `agent` app installed\n"
            "with manifest-scoped grants (own Cairn namespace only); in-grant durable\n"
            "commits; a bad write undone by a one-step rollback (history kept,\n"
            "hash-verified); a no-capability `spy` app DENIED by the kernel; state\n"
            "checked again after a reboot.\n\n"
            "```text\n" + body + "```\n",
            encoding="utf-8",
        )
        print(f"wrote transcript: {args.transcript}")
    except Exception as exc:
        print(f"agent demo FAILED: {exc}", file=sys.stderr)
        return 1
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print("agent demo PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

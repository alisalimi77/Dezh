#!/usr/bin/env python3
"""End-to-end SDK acceptance test (W1).

Builds the out-of-tree `hello` template into a `.dzp`, plus a "greedy" variant
that uses print WITHOUT declaring the capability, then boots the RISC-V kernel
in QEMU and checks:

  1. upload over the UART installs the package (CRC + IR verify pass);
  2. `pkg-run hello` prints the app's output;
  3. `pkg-run greedy` is DENIED by the kernel (undeclared capability);
  4. a corrupted upload is rejected by the CRC check;
  5. clean halt, exit 0.
"""

from __future__ import annotations

import argparse
import base64
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools" / "sdk"))

import build_pkg  # noqa: E402
import install_pkg  # noqa: E402

GREEDY_TOML = """\
name = "greedy"
version = "0.1.0"
kind = "dezh-ir"
entry = "greedy.dzs"
caps = []
"""

GREEDY_DZS = """\
; Uses print WITHOUT declaring the capability -> kernel must DENY it.
    push 1234
    hostcall print_num
    halt
"""


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--kernel", type=Path, default=install_pkg.DEFAULT_KERNEL)
    parser.add_argument("--qemu", default="qemu-system-riscv64")
    args = parser.parse_args()

    work = Path(tempfile.mkdtemp(prefix="dezh-sdk-test-"))
    try:
        # Build hello from the template exactly like a stranger would.
        hello_dir = work / "hello"
        shutil.copytree(REPO / "tools" / "sdk" / "templates" / "hello", hello_dir)
        hello_pkg = build_pkg.build(hello_dir, None)

        greedy_dir = work / "greedy"
        greedy_dir.mkdir()
        (greedy_dir / "app.toml").write_text(GREEDY_TOML, encoding="utf-8")
        (greedy_dir / "greedy.dzs").write_text(GREEDY_DZS, encoding="utf-8")
        greedy_pkg = build_pkg.build(greedy_dir, None)

        disk = work / "disk.img"
        with disk.open("wb") as fh:
            fh.truncate(2 * 1024 * 1024)

        console = install_pkg.QemuConsole(
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
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            install_pkg.upload_package(console, hello_pkg)
            install_pkg.upload_package(console, greedy_pkg)

            mark = console.send_line("pkg-list")
            console.wait_for("hello 0.1.0 kind=dezh-ir", since=mark)
            console.wait_for("greedy 0.1.0 kind=dezh-ir", since=mark)

            mark = console.send_line("pkg-info hello")
            console.wait_for("GRANTED  print", since=mark)
            console.wait_for("DENIED   ipc uptime cairn-read cairn-write", since=mark)

            mark = console.send_line("pkg-run hello")
            console.wait_for("hello from a .dzp package!", since=mark)
            console.wait_for("42", since=mark)
            console.wait_for("[pkg-run] 'hello' finished", since=mark)

            mark = console.send_line("pkg-run greedy")
            console.wait_for("DENIED by kernel: missing required capability", since=mark)

            # Corrupted upload must be rejected by the CRC check.
            data = bytearray(hello_pkg.read_bytes())
            data[len(data) // 2] ^= 0xFF
            mark = console.send_line("pkg-recv")
            console.wait_for("[pkg-recv] ready", since=mark)
            sent = 0
            for i in range(0, len(data), install_pkg.CHUNK_RAW_BYTES):
                chunk = base64.b64encode(
                    bytes(data[i : i + install_pkg.CHUNK_RAW_BYTES])
                ).decode("ascii")
                mark = console.send_line(chunk)
                sent += min(install_pkg.CHUNK_RAW_BYTES, len(data) - i)
                console.wait_for(f"+ok {sent}", since=mark)
            mark = console.send_line(".")
            console.wait_for("rejected", since=mark)

            mark = console.send_line("pkg-remove greedy")
            console.wait_for("removed 'greedy'", since=mark)
            mark = console.send_line("pkg-run greedy")
            console.wait_for("no installed package 'greedy'", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()
    except Exception as exc:
        print(f"sdk test FAILED: {exc}", file=sys.stderr)
        return 1
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print("sdk test PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

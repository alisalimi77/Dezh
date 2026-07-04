#!/usr/bin/env python3
"""End-to-end SDK package-store acceptance test.

Builds the out-of-tree `hello` template into a `.dzp`, plus a "greedy" variant
that uses print WITHOUT declaring the capability, then boots the RISC-V kernel
in QEMU and checks:

  1. upload over the UART installs packages transactionally;
  2. packages persist across reboot and run with manifest-scoped grants;
  3. removed packages stay non-runnable across reboot;
  4. interrupted journal states recover cleanly;
  5. corrupt journals degrade the store until explicit recovery.
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

HELLO_V2_TOML = """\
name = "hello"
version = "0.2.0"
kind = "dezh-ir"
entry = "hello.dzs"
caps = ["print"]
"""

HELLO_V3_TOML = """\
name = "hello"
version = "0.3.0"
kind = "dezh-ir"
entry = "hello.dzs"
caps = ["print", "uptime"]
"""

HELLO_V2_DZS = """\
    string 0 "hello from a .dzp package!"
    prints 0 26
    push 12
    push 7
    mul
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

        hello_v2_dir = work / "hello-v2"
        hello_v2_dir.mkdir()
        (hello_v2_dir / "app.toml").write_text(HELLO_V2_TOML, encoding="utf-8")
        (hello_v2_dir / "hello.dzs").write_text(HELLO_V2_DZS, encoding="utf-8")
        hello_v2_pkg = build_pkg.build(hello_v2_dir, None)

        hello_v3_dir = work / "hello-v3"
        hello_v3_dir.mkdir()
        (hello_v3_dir / "app.toml").write_text(HELLO_V3_TOML, encoding="utf-8")
        (hello_v3_dir / "hello.dzs").write_text(HELLO_V2_DZS, encoding="utf-8")
        hello_v3_pkg = build_pkg.build(hello_v3_dir, None)

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

            install_pkg.upload_package(console, hello_pkg)
            install_pkg.upload_package(console, greedy_pkg)

            mark = console.send_line("pkg-store")
            console.wait_for("pkg-store:", since=mark)
            console.wait_for("active=2", since=mark)

            mark = console.send_line("pkg-list")
            console.wait_for("[Active] hello 0.1.0", since=mark)
            console.wait_for("[Active] greedy 0.1.0", since=mark)

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
            console.wait_for("package 'greedy' is Removed and not runnable", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            mark = console.send_line("pkg-list")
            console.wait_for("[Active] hello 0.1.0", since=mark)
            mark = console.send_line("pkg-run hello")
            console.wait_for("hello from a .dzp package!", since=mark)
            console.wait_for("[pkg-run] 'hello' finished", since=mark)
            install_pkg.upload_package(
                console,
                hello_v2_pkg,
                command="pkg-update hello",
                success="[pkg-update] committed",
            )
            mark = console.send_line("pkg-versions hello")
            console.wait_for("active=0.2.0", since=mark)
            console.wait_for("previous=0.1.0", since=mark)
            mark = console.send_line("pkg-run hello")
            console.wait_for("84", since=mark)
            mark = console.send_line("pkg-rollback hello")
            console.wait_for("restored 'hello' to 0.1.0", since=mark)
            mark = console.send_line("pkg-run hello")
            console.wait_for("42", since=mark)
            mark = console.send_line("pkg-pin hello")
            console.wait_for("pinned=yes", since=mark)
            mark = console.send_line("pkg-update hello")
            console.wait_for("is pinned", since=mark)
            mark = console.send_line("pkg-unpin hello")
            console.wait_for("pinned=no", since=mark)
            install_pkg.upload_package(
                console,
                hello_v3_pkg,
                command="pkg-update hello",
                success="review required",
            )
            mark = console.send_line("pkg-versions hello")
            console.wait_for("active=0.1.0", since=mark)
            install_pkg.upload_package(
                console,
                hello_v3_pkg,
                command="pkg-update hello --allow-new-caps",
                success="[pkg-update] committed",
            )
            mark = console.send_line("pkg-review hello")
            console.wait_for("active_caps=print uptime", since=mark)
            mark = console.send_line("pkg-lifecycle")
            console.wait_for("active=1", since=mark)
            console.wait_for("previous=1", since=mark)
            mark = console.send_line("pkg-store")
            console.wait_for("active=1", since=mark)
            console.wait_for("removed=1", since=mark)
            mark = console.send_line("pkg-fault remove-pending")
            console.wait_for("injected remove-pending", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU reboot #2 exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            mark = console.send_line("pkg-journal")
            console.wait_for("op=Remove", since=mark)
            mark = console.send_line("pkg-recover")
            console.wait_for("completed interrupted remove", since=mark)
            mark = console.send_line("pkg-run hello")
            console.wait_for("package 'hello' is Removed and not runnable", since=mark)
            mark = console.send_line("pkg-store")
            console.wait_for("active=0", since=mark)
            console.wait_for("removed=2", since=mark)

            mark = console.send_line("pkg-fault install-after-blob")
            console.wait_for("injected install-after-blob", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU reboot #3 exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            mark = console.send_line("pkg-journal")
            console.wait_for("phase=BlobWritten", since=mark)
            mark = console.send_line("pkg-recover")
            console.wait_for("rolled back incomplete install", since=mark)
            console.wait_for("complete", since=mark)
            mark = console.send_line("pkg-run faultpkg")
            console.wait_for("no installed package 'faultpkg'", since=mark)
            mark = console.send_line("pkg-fault install-pending-registry")
            console.wait_for("injected install-pending-registry", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU reboot #4 exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            mark = console.send_line("pkg-journal")
            console.wait_for("phase=RegistryPending", since=mark)
            mark = console.send_line("pkg-recover")
            console.wait_for("quarantined suspicious pending install", since=mark)
            mark = console.send_line("pkg-run faultpkg")
            console.wait_for("package 'faultpkg' is Quarantined and not runnable", since=mark)
            mark = console.send_line("pkg-fault corrupt-journal")
            console.wait_for("injected corrupt-journal", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU reboot #5 exited with {code}, expected 0")
        finally:
            print(console.text())
            console.stop()

        console = boot()
        try:
            console.wait_for("Dezh console. Every command requires an explicit capability.")
            console.wait_for("dezh> ")

            mark = console.send_line("pkg-run hello")
            console.wait_for("package store unavailable or degraded", since=mark)
            mark = console.send_line("pkg-recover")
            console.wait_for("journal corrupt", since=mark)
            console.wait_for("complete", since=mark)
            mark = console.send_line("pkg-store")
            console.wait_for("degraded=no", since=mark)
            mark = console.send_line("pkg-gc")
            console.wait_for("removed_slots=1", since=mark)
            console.wait_for("dry_run=yes", since=mark)
            mark = console.send_line("pkg-gc run")
            console.wait_for("wiped_slots=1", since=mark)
            mark = console.send_line("pkg-store")
            console.wait_for("removed=0", since=mark)
            console.wait_for("quarantined=1", since=mark)
            mark = console.send_line("pkg-run faultpkg")
            console.wait_for("package 'faultpkg' is Quarantined and not runnable", since=mark)

            console.send_line("halt")
            code = console.proc.wait(timeout=10)
            if code != 0:
                raise AssertionError(f"QEMU reboot #6 exited with {code}, expected 0")
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

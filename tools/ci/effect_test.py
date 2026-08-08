#!/usr/bin/env python3
"""W12 acceptance: a real external effect, and a compensation that really runs.

`gateway_test.py` proves the external system half without Dezh. This proves the
whole path: a capability-gated request leaves the machine on the wire, a real
git commit happens outside Dezh, the effect lands on the Sand ledger as
COMPENSATABLE, and the registered compensating action really reverts it.

The claim being tested is deliberately bounded. Dezh proves the request was
authorized for a *named destination*, left on the wire, was answered, and was
recorded; and that the compensation ran. It does not prove the gateway was
honest — the gateway is outside the TCB. What makes the test meaningful anyway
is that it checks the git repository directly rather than believing Dezh's
transcript: the assertions about external state come from `git`, not from what
the console printed.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "gateway"))
import dezh_gateway  # noqa: E402
from qemu_smoke import QemuSession  # noqa: E402

GATEWAY_PORT = 8888  # the `ops` destination in MARZ_DESTS
SLUG = "nightly-report"
FAILURES: list[str] = []


def check(cond: bool, what: str) -> None:
    print(f"  {'ok  ' if cond else 'FAIL'} {what}", flush=True)
    if not cond:
        FAILURES.append(what)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()


def boot(qemu: str, kernel: Path, disk: Path) -> QemuSession:
    return QemuSession(
        [
            qemu, "-machine", "virt", "-smp", "4", "-nographic",
            "-bios", "default", "-kernel", str(kernel),
            "-drive", f"file={disk},format=raw,if=none,id=dezhdisk",
            "-device", "virtio-blk-device,drive=dezhdisk",
            # User networking: the guest reaches the host at 10.0.2.2, which is
            # where the gateway listens. No host privileges, no port forward.
            "-netdev", "user,id=n0",
            "-device", "virtio-net-device,netdev=n0",
        ],
        timeout=90,
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--kernel", required=True, type=Path)
    ap.add_argument("--qemu", default="qemu-system-riscv64")
    args = ap.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / "scratch"
        dezh_gateway.init_repo(repo)
        threading.Thread(
            target=lambda: dezh_gateway.serve(repo, "0.0.0.0", GATEWAY_PORT),
            daemon=True,
        ).start()
        time.sleep(0.3)

        disk = Path(tmp) / "disk.img"
        disk.write_bytes(b"\0" * (2 * 1024 * 1024))
        base = int(git(repo, "rev-list", "--count", "HEAD"))

        session = boot(args.qemu, args.kernel, disk)
        try:
            session.wait_for("boot contract VALIDATED")
            session.wait_for("dezh>")

            print("effect:", flush=True)
            at = session.send_line(f"marz-effect ops git.commit {SLUG}")
            session.wait_for("[marz-effect] gateway says:", since=at)
            session.wait_for("DEZHFX1 OK", since=at)
            end = session.wait_for("recorded COMPENSATABLE", since=at)
            check(True, "Dezh observed the outcome and recorded it")

            # The transcript is Dezh's account. These are the external system's.
            check(
                (repo / f"{SLUG}.txt").is_file(),
                "the file exists in the git repo -- the effect really happened",
            )
            check(
                int(git(repo, "rev-list", "--count", "HEAD")) == base + 1,
                "the repo gained exactly one commit",
            )
            subject = git(repo, "log", "-1", "--format=%s")
            check("dezh:" in subject, f"the external system records it as ours: {subject!r}")

            window = session.text()[:end]
            check(
                "COMPENSATABLE" in window and "IRREVERSIBLE" not in window.split("marz-effect")[-1],
                "recorded compensatable, NOT irreversible -- a different class from marz-send",
            )
            check(
                "integrity LOWERED" in session.text()[at:],
                "the reply lowered operator integrity: bytes off the wire are unvalidated",
            )

            token = git(repo, "rev-parse", "--short=10", "HEAD")
            print("compensation:", flush=True)
            at = session.send_line(f"marz-effect ops git.revert {token}")
            session.wait_for("DEZHFX1 OK", since=at)
            session.wait_for("recorded COMPENSATABLE", since=at)

            check(
                not (repo / f"{SLUG}.txt").exists(),
                "the file is gone -- the compensating action really ran out there",
            )
            check(
                int(git(repo, "rev-list", "--count", "HEAD")) == base + 2,
                "history was KEPT: the compensation is a new commit",
            )

            print("refusal:", flush=True)
            at = session.send_line("dev-revoke net")
            session.wait_for("REVOKED", since=at)
            at = session.send_line(f"marz-effect ops git.commit second-{SLUG}")
            session.wait_for("DENIED", since=at)
            check(
                not (repo / f"second-{SLUG}.txt").exists(),
                "with the NIC capability revoked, NOTHING happened externally",
            )
            check(
                int(git(repo, "rev-list", "--count", "HEAD")) == base + 2,
                "the repo is untouched by the refused effect",
            )

            session.send_line("halt")
        finally:
            session.stop()

    print()
    if FAILURES:
        print(f"effect test FAILED ({len(FAILURES)} checks)", file=sys.stderr)
        for f in FAILURES:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print("effect test passed: a real external effect, authorized, recorded, and compensated")
    return 0


if __name__ == "__main__":
    sys.exit(main())

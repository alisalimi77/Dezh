#!/usr/bin/env python3
"""Prove the effect gateway performs real external effects, and undoes them.

This test does not involve Dezh. It exercises the gateway over a real UDP
socket and then inspects the git repository directly, so that when the
end-to-end demo later claims "the effect really happened", the claim about the
external system is already established independently of the OS path.

What it checks:

  1. A `git.commit` request creates a real commit containing a real file.
  2. The commit message carries the intent id, so the external system holds the
     attribution too, not just Dezh's ledger.
  3. A `git.revert` request creates a real revert commit and the file is gone.
  4. History is kept — compensation is a new commit, not a rewrite. That is the
     difference between compensating an effect and pretending it never happened.
  5. Requests the gateway should refuse are refused: bad slugs, unknown verbs,
     reverting a commit the gateway did not create, and double-commits.
"""

from __future__ import annotations

import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "gateway"))
import dezh_gateway  # noqa: E402

HOST = "127.0.0.1"
FAILURES: list[str] = []


def check(cond: bool, what: str) -> None:
    print(f"  {'ok  ' if cond else 'FAIL'} {what}", flush=True)
    if not cond:
        FAILURES.append(what)


def git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, capture_output=True, text=True, check=True
    ).stdout.strip()


def request(port: int, text: str, timeout: float = 5.0) -> str:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.settimeout(timeout)
    try:
        sock.sendto(text.encode("ascii"), (HOST, port))
        data, _ = sock.recvfrom(dezh_gateway.MAX_DATAGRAM)
        return data.decode("ascii").strip()
    finally:
        sock.close()


def free_port() -> int:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind((HOST, 0))
    port = s.getsockname()[1]
    s.close()
    return port


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / "scratch"
        dezh_gateway.init_repo(repo)
        port = free_port()

        # A daemon thread: the process exits without joining it, which is what
        # we want for a server whose only job is to answer this test.
        thread = threading.Thread(
            target=lambda: dezh_gateway.serve(repo, HOST, port),
            daemon=True,
        )
        thread.start()
        # The socket is bound inside the thread; give it a moment before the
        # first datagram, or the test races the server it just started.
        for _ in range(50):
            try:
                if request(port, f"{dezh_gateway.MAGIC} 0 ping", timeout=0.2).startswith(
                    f"{dezh_gateway.MAGIC} OK"
                ):
                    break
            except socket.timeout:
                time.sleep(0.05)
        else:
            print("gateway did not come up", file=sys.stderr)
            return 1

        base = git(repo, "rev-list", "--count", "HEAD")
        print("effect:", flush=True)
        reply = request(port, f"{dezh_gateway.MAGIC} 7 git.commit nightly-report")
        check(reply.startswith(f"{dezh_gateway.MAGIC} OK "), f"commit accepted: {reply}")
        token = reply.split(" ")[2] if " OK " in reply else ""

        check((repo / "nightly-report.txt").is_file(), "the file exists on disk")
        check(
            git(repo, "rev-list", "--count", "HEAD") == str(int(base) + 1),
            "history advanced by exactly one commit",
        )
        subject = git(repo, "log", "-1", "--format=%s")
        check("Ahd#7" in subject, f"the commit carries the intent id: {subject!r}")
        check(
            git(repo, "status", "--porcelain") == "",
            "the working tree is clean (the effect is committed, not staged)",
        )

        print("refusals:", flush=True)
        check(
            "ERR already-exists" in request(port, f"{dezh_gateway.MAGIC} 7 git.commit nightly-report"),
            "the same effect twice is refused",
        )
        check(
            "ERR bad-slug" in request(port, f"{dezh_gateway.MAGIC} 7 git.commit ../escape"),
            "a slug that would escape the scratch tree is refused",
        )
        check(
            "ERR unknown-verb" in request(port, f"{dezh_gateway.MAGIC} 7 git.push origin"),
            "an unknown verb is refused",
        )
        check(
            "ERR bad-intent" in request(port, f"{dezh_gateway.MAGIC} zz git.commit x"),
            "a non-numeric intent is refused",
        )
        root = git(repo, "rev-list", "--max-parents=0", "HEAD")
        check(
            "ERR not-a-dezh-effect" in request(port, f"{dezh_gateway.MAGIC} 7 git.revert {root[:10]}"),
            "reverting a commit the gateway did not create is refused",
        )

        print("compensation:", flush=True)
        reply = request(port, f"{dezh_gateway.MAGIC} 7 git.revert {token}")
        check(reply.startswith(f"{dezh_gateway.MAGIC} OK "), f"revert accepted: {reply}")
        check(
            not (repo / "nightly-report.txt").exists(),
            "the file is gone -- the external system really changed back",
        )
        check(
            git(repo, "rev-list", "--count", "HEAD") == str(int(base) + 2),
            "history was KEPT: compensation is a new commit, not a rewrite",
        )
        check(
            git(repo, "cat-file", "-t", token) == "commit",
            "the original effect commit is still reachable",
        )

    print()
    if FAILURES:
        print(f"gateway test FAILED ({len(FAILURES)} checks)", file=sys.stderr)
        for f in FAILURES:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print("gateway test passed: the effect and its compensation are both real")
    return 0


if __name__ == "__main__":
    sys.exit(main())

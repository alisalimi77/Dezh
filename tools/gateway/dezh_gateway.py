#!/usr/bin/env python3
"""A host-side effect gateway for Dezh.

Dezh has ARP, ICMP and UDP egress. It does not have TCP, DNS or TLS, so it
cannot speak git or HTTP itself; W12 records that as a workstream rather than a
step. This daemon is the tractable alternative: it runs OUTSIDE Dezh, receives
one UDP datagram over the egress path that already exists, performs a real
effect on a real external system, and reports what it did.

The honesty boundary, stated here because it is the whole point of the design:

    This gateway is NOT in Dezh's trusted computing base. Dezh authorizes the
    request, records it, and can ask for it to be compensated. It cannot verify
    that the gateway did what it said. A compromised gateway can lie.

That is a smaller claim than "the OS speaks git", and it is the true one. What
Dezh does prove is the part it owns: the effect was authorized by a capability
for a named destination, it left the machine on the wire, it was recorded on the
Sand ledger under an intent, and a registered compensating action for it also
really runs.

Effects are git commits in a scratch repository, because a commit is real
external state that a test can inspect afterwards, and `git revert` is a genuine
compensating action rather than a delete pretending to be one.

Wire protocol (ASCII, one datagram per message, <= 512 bytes):

    request       DEZHFX1 <intent> <verb> <arg>
    reply, ok     DEZHFX1 OK <token> <detail>
    reply, error  DEZHFX1 ERR <reason>

`<intent>` is the Ahd id the effect is attributed to; the gateway echoes it back
in the commit message so the external system carries the attribution too.
`<token>` is the created commit's short hash, which is what a later
`git.revert` names. Fields are space-separated and never contain spaces
themselves except `<detail>`, which is last and may.

Verbs:

    git.commit <slug>   create a file and commit it; token = commit hash
    git.revert <token>  revert that commit; the compensating action
    ping                liveness, performs nothing
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import socket
import subprocess
import sys
from pathlib import Path

MAGIC = "DEZHFX1"
MAX_DATAGRAM = 512

# A slug names the file an effect creates. Anything outside this set could
# escape the scratch tree, and the gateway is the only thing standing between a
# datagram and a real filesystem.
SLUG_RE = re.compile(r"\A[A-Za-z0-9][A-Za-z0-9._-]{0,63}\Z")
TOKEN_RE = re.compile(r"\A[0-9a-f]{7,40}\Z")


class GatewayError(Exception):
    """A request the gateway refuses, reported to Dezh as ERR."""


def run_git(repo: Path, *args: str) -> str:
    """Run one git command in `repo` and return its stdout, stripped."""
    proc = subprocess.run(
        ["git", *args],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        detail = (proc.stderr or proc.stdout).strip().replace("\n", "; ")
        raise GatewayError(f"git-{args[0]}-failed:{detail[:120]}")
    return proc.stdout.strip()


def init_repo(repo: Path) -> None:
    """Create the scratch repository, replacing any previous one."""
    if repo.exists():
        shutil.rmtree(repo)
    repo.mkdir(parents=True)
    run_git(repo, "init", "--quiet", "--initial-branch=main")
    run_git(repo, "config", "user.email", "gateway@dezh.invalid")
    run_git(repo, "config", "user.name", "Dezh Effect Gateway")
    # An empty repository has no HEAD, and `git revert` needs a parent. The
    # root commit is infrastructure, not an effect.
    (repo / "README").write_text("scratch repository for Dezh effect gateway\n")
    run_git(repo, "add", "README")
    run_git(repo, "commit", "--quiet", "-m", "root")


def do_commit(repo: Path, intent: str, slug: str) -> tuple[str, str]:
    """Create a file and commit it. Returns (token, detail)."""
    if not SLUG_RE.match(slug):
        raise GatewayError("bad-slug")
    path = repo / f"{slug}.txt"
    if path.exists():
        raise GatewayError("already-exists")
    path.write_text(f"written by Dezh under intent Ahd#{intent}\n")
    run_git(repo, "add", path.name)
    run_git(repo, "commit", "--quiet", "-m", f"dezh: {slug} (Ahd#{intent})")
    token = run_git(repo, "rev-parse", "--short=10", "HEAD")
    return token, f"committed {path.name} as {token}"


def do_revert(repo: Path, intent: str, token: str) -> tuple[str, str]:
    """Revert a commit this gateway made. The compensating action."""
    if not TOKEN_RE.match(token):
        raise GatewayError("bad-token")
    subject = run_git(repo, "log", "-1", "--format=%s", token)
    if not subject.startswith("dezh: "):
        # Refusing to revert anything the gateway did not create keeps the
        # compensation scoped to effects Dezh actually caused.
        raise GatewayError("not-a-dezh-effect")
    run_git(repo, "revert", "--no-edit", token)
    new = run_git(repo, "rev-parse", "--short=10", "HEAD")
    return new, f"reverted {token} in {new} (Ahd#{intent})"


def handle(repo: Path, text: str) -> str:
    """Turn one request line into one reply line."""
    parts = text.strip().split(" ", 3)
    if len(parts) < 3 or parts[0] != MAGIC:
        return f"{MAGIC} ERR malformed"
    _, intent, verb = parts[0], parts[1], parts[2]
    arg = parts[3] if len(parts) > 3 else ""
    if not intent.isdigit():
        return f"{MAGIC} ERR bad-intent"
    try:
        if verb == "ping":
            return f"{MAGIC} OK - gateway alive"
        if verb == "git.commit":
            token, detail = do_commit(repo, intent, arg)
            return f"{MAGIC} OK {token} {detail}"
        if verb == "git.revert":
            token, detail = do_revert(repo, intent, arg)
            return f"{MAGIC} OK {token} {detail}"
        return f"{MAGIC} ERR unknown-verb"
    except GatewayError as exc:
        return f"{MAGIC} ERR {exc}"


def serve(repo: Path, host: str, port: int, once: bool = False) -> None:
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((host, port))
    print(f"[gateway] listening on {host}:{port}, repo={repo}", flush=True)
    while True:
        data, peer = sock.recvfrom(MAX_DATAGRAM)
        text = data.decode("ascii", errors="replace")
        print(f"[gateway] <- {peer[0]}:{peer[1]} {text.strip()}", flush=True)
        reply = handle(repo, text)
        print(f"[gateway] -> {reply}", flush=True)
        sock.sendto(reply.encode("ascii"), peer)
        if once:
            return


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", required=True, help="scratch git repository path")
    ap.add_argument("--host", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=8888)
    ap.add_argument("--init", action="store_true", help="(re)create the repo and exit")
    ap.add_argument("--once", action="store_true", help="serve one datagram and exit")
    args = ap.parse_args(argv)

    repo = Path(args.repo).resolve()
    if args.init:
        init_repo(repo)
        print(f"[gateway] initialised {repo}", flush=True)
        return 0
    if not (repo / ".git").is_dir():
        init_repo(repo)
    try:
        serve(repo, args.host, args.port, once=args.once)
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())

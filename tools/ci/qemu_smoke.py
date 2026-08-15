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


GUEST_IP = (10, 0, 2, 15)
GUEST_VAULT_IP = (10, 0, 2, 3)
EGRESS_MARKER = b"DEZH-MARZ-EGRESS-v0"


def parse_pcap(blob: bytes) -> list[dict]:
    """Decode a classic pcap capture into per-packet facts.

    Only what the assertions need: IPv4 addresses, protocol, the ICMP type, and
    whether a UDP payload carries the egress marker. Anything unparseable is
    reported as an empty dict rather than raising, so a malformed tail cannot mask
    a real assertion failure.
    """
    if len(blob) < 24:
        return []
    magic = blob[:4]
    if magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1"):
        endian = "little"
    elif magic in (b"\xa1\xb2\xc3\xd4", b"\xa1\xb2\x3c\x4d"):
        endian = "big"
    else:
        return []

    def u32(b: bytes) -> int:
        return int.from_bytes(b, endian)

    out: list[dict] = []
    off = 24
    while off + 16 <= len(blob):
        incl = u32(blob[off + 8 : off + 12])
        off += 16
        if incl <= 0 or off + incl > len(blob):
            break
        frame = blob[off : off + incl]
        off += incl
        out.append(decode_frame(frame))
    return out


def decode_frame(frame: bytes) -> dict:
    info: dict = {"len": len(frame)}
    if len(frame) < 14:
        return info
    ethertype = int.from_bytes(frame[12:14], "big")
    info["ethertype"] = ethertype
    if ethertype != 0x0800 or len(frame) < 34:
        return info
    ip = frame[14:]
    ihl = (ip[0] & 0x0F) * 4
    if ip[0] >> 4 != 4 or len(ip) < ihl:
        return info
    info["src_ip"] = tuple(ip[12:16])
    info["dst_ip"] = tuple(ip[16:20])
    proto = ip[9]
    info["proto"] = proto
    body = ip[ihl:]
    if proto == 1 and body:  # ICMP
        info["icmp_type"] = body[0]
    elif proto == 17 and len(body) >= 8:  # UDP
        info["udp_payload"] = body[8:]
    return info


def is_guest_udp_marker(p: dict) -> bool:
    """A UDP datagram the GUEST sent carrying the egress marker.

    Requiring the guest as source is what separates a real send from the host's
    ICMP error quoting our datagram back at us.
    """
    return (
        p.get("proto") == 17
        and p.get("src_ip") == GUEST_IP
        and EGRESS_MARKER in p.get("udp_payload", b"")
    )


def run_riscv64(qemu: str, kernel: Path) -> None:
    disk = tempfile.NamedTemporaryFile(prefix="dezh-disk-", suffix=".img", delete=False)
    disk_path = Path(disk.name)
    # Marz: capture every frame that actually leaves the machine, so the egress
    # test asserts real wire output rather than a printed claim.
    pcap = tempfile.NamedTemporaryFile(prefix="dezh-egress-", suffix=".pcap", delete=False)
    pcap_path = Path(pcap.name)
    pcap.close()
    try:
        disk.truncate(2 * 1024 * 1024)
    finally:
        disk.close()
    session = QemuSession(
        [
            qemu,
            "-machine",
            "virt",
            # Four harts: the SMP proof needs more than one. QEMU parks the
            # secondaries in SBI firmware until the kernel starts them, and the
            # boot hart is chosen nondeterministically - both of which the kernel
            # handles (it reads its own id from a0 and skips absent harts).
            "-smp",
            "4",
            "-nographic",
            "-bios",
            "default",
            "-kernel",
            str(kernel),
            "-drive",
            f"file={disk_path},format=raw,if=none,id=dezhdisk",
            "-device",
            "virtio-blk-device,drive=dezhdisk",
            # Marz (egress): a NIC for the guarded egress boundary. QEMU's user
            # networking needs no host privileges; filter-dump captures every
            # frame so CI can assert what actually left the machine.
            "-netdev",
            "user,id=n0",
            "-device",
            "virtio-net-device,netdev=n0",
            "-object",
            f"filter-dump,id=f0,netdev=n0,file={pcap_path}",
        ],
        timeout=60,
    )
    try:
        session.wait_for("boot contract VALIDATED")
        # SMP: the secondary harts come up via SBI HSM and run a parallel round at
        # boot. With -smp 4 exactly three secondaries start, and the shared counter
        # must equal harts x work - proof they ran concurrently on coherent memory.
        session.wait_for("smp: 3 secondary harts online via SBI HSM")
        session.wait_for("shared-counter = 600000 (expected 600000) -> COHERENT")
        # Mutual exclusion: a NON-atomic counter under the kernel's ticket lock,
        # hammered by all four harts, must land exactly on (4 x 50000). Atomics
        # cannot prove this - only a correct lock can.
        session.wait_for("lock-guarded counter = 200000 (expected 200000) -> MUTEX-OK")
        # Shared run queue: 48 jobs drained concurrently by several harts, each
        # exactly once - the core correctness property of a symmetric scheduler.
        session.wait_for("run-queue 48 jobs drained by")
        session.wait_for("each exactly once -> QUEUE-OK")
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
            # --- Intent as mechanism (W8 / Ahd): derived capability <= intent ---
            ("intent-open writer", "opened Ahd #1 kind=writer"),
            ("intent-open compute", "opened Ahd #2 kind=compute"),
            ("intent-list", "Ahd #2 kind=compute ceiling=print"),
            (
                "intent-demo",
                [
                    "intent (Ahd) is the ONLY path to authority",
                    "[intent-demo] agent finished within intent",
                    "beyond-intent DENIED (dropped): cairn-read cairn-write",
                    "kernel DENIED an out-of-intent hostcall",
                    "[intent-demo] PASS",
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
            # --- Sand effect ledger (W8 P2): effects accountable to intent -----
            (
                "sand-demo",
                [
                    "Sand = the Cairn commit log as an effect ledger",
                    "[sand-demo] opened Ahd #3 kind=writer",
                    "[sand] effect ledger ns=agent",
                    "intent=Ahd#3 derived=print,cairn-read,cairn-write reversibility=reversible status=committed",
                    "[sand-demo] PASS",
                ],
            ),
            ("sand-log agent", "actor -> intent -> derived cap -> effect"),
            ("sand-info agent", "head effect ns=agent"),
            ("events", "sand.effect"),
            # --- Sfar mission rollback (W8 P3): honest whole-mission rollback --
            (
                "sfar-demo",
                [
                    "[sfar-demo] 1/4 mission Ahd#4",
                    "reversibility=irreversible",
                    "[sfar] plan: reversible=2 compensatable=0 irreversible=1 unknown=0 confidence=partial",
                    "REFUSED at ns=agent",
                    "already happened in the outside world; cannot be undone",
                    "reversible effects retracted=2 compensations performed=0 refused_irreversible=1",
                    "[sfar-demo] PASS",
                ],
            ),
            # After rollback the live forecast drops the two retracted writes;
            # only the irreversible send is still standing.
            ("sfar-plan 4", "reversible=0 compensatable=0 irreversible=1"),
            ("events", "sfar.demo"),
            # --- Sfar mission authority spans every namespace (W8 P3 slice 2) ---
            # A mission across ns=lab + ns=calc: a rollback holding authority over
            # only ns=lab is refused (naming ns=calc); full authority undoes it.
            (
                "sfar-cross-demo",
                [
                    "one reversible effect to ns=lab and one to ns=calc",
                    "reversible=2 compensatable=0 irreversible=0 unknown=0 confidence=full",
                    "DENIED: mission authority requires the capability for every namespace it touched",
                    "missing capability CAIRN_NS_2 (ns=calc)",
                    "reversible effects retracted=2 compensations performed=0 refused_irreversible=0",
                    "[sfar-cross-demo] PASS",
                ],
            ),
            # --- Compensation for compensatable effects (W8 P3 slice 2) --------
            # A compensatable effect ships a registered compensating action;
            # rollback RUNS and RECORDS it (a saga step) instead of refusing.
            (
                "comp-demo",
                [
                    "one compensatable effect (with a registered compensation) below two reversible writes",
                    "compensatable=1 irreversible=0 unknown=0 confidence=full-with-compensation",
                    'ran compensating action "resource.delete:cache/42"',
                    "reversible effects retracted=2 compensations performed=1",
                    "status=compensation",
                    "[comp-demo] PASS",
                ],
            ),
            ("events", "comp.demo"),
            # --- Package signing (capability-native): verify + attenuate -------
            # A build-time-signed package: valid Ed25519 signature from a trusted
            # publisher installs, attenuated to the publisher's ceiling (ipc is
            # dropped); tampered + revoked are refused.
            (
                "sig-demo",
                [
                    "trusted publisher 'demo-publisher' found",
                    "signature VALID (Ed25519 over inner .dzp + counter)",
                    "requested=print ipc cairn-read cairn-write | publisher ceiling=print cairn-read cairn-write | GRANTED=print cairn-read cairn-write",
                    "dropped beyond publisher ceiling: ipc",
                    "a flipped inner byte is REJECTED",
                    "a revoked signer key is REFUSED",
                    "[sig-demo] PASS",
                ],
            ),
            ("events", "sig.demo"),
            # --- Tbar provenance graph (W8 P5): actor -> intent -> effect ------
            # Query the provenance of the sfar-demo mission (Ahd#4). Its head is
            # the irreversible send rollback refused to undo; Tbar attributes it
            # to its actor + intent + derived cap, unforgeably.
            (
                "tbar 4",
                [
                    "provenance graph for intent Ahd#4",
                    "actor task1 -> intent Ahd#4 (derived print,cairn-read,cairn-write) -> effect ns=agent",
                    "class=irreversible",
                    "attributed to intent Ahd#4",
                ],
            ),
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
            # --- The adversary (W8 P4): five escapes, five named boundaries ----
            # A malicious agent tries to escape containment five ways; each is
            # stopped at a real, named boundary and the console survives. Runs
            # last (its rogue/spy/preempt tasks reset the task table) so escape 1
            # still finds the live storage daemon.
            (
                "redteam",
                [
                    "escape 1/5",
                    "DENIED: ns=vault requires capability CAIRN_NS_3",
                    "escape 1 STOPPED at boundary: storage-service capability check",
                    "DENIED: faulted on 0x10000000 (outside its grant)",
                    "escape 2 STOPPED at boundary: hardware memory boundary",
                    "holds no PRINT capability",
                    "escape 3 STOPPED at boundary: kernel syscall capability check",
                    "beyond-intent dropped by the derivation ceiling: cairn-read cairn-write",
                    "kernel DENIED the out-of-intent Cairn write",
                    "escape 4 STOPPED at boundary: intent-derivation ceiling",
                    "escape 5 STOPPED at boundary: preemptive scheduler",
                    "[redteam] PASS: all five escapes were stopped at named boundaries",
                ],
            ),
            ("events", "redteam"),
            # --- Explainable denial (W8 P5): why-denied names the boundary -----
            # After the adversary run, why-denied attributes the last denial to a
            # real mechanism (the intent-derivation ceiling from escape 4).
            (
                "why-denied",
                [
                    "last denial: actor=redteam action=intent.derive",
                    "boundary: intent-derivation ceiling",
                ],
            ),
            # --- Flagship narrative (W8 P7): the whole night in one story ------
            # A coding agent loose overnight under one intent: mixed effects
            # across two namespaces, forecast + provenance in the morning, an
            # honest rollback (retract / compensate / refuse-with-reason), and a
            # contained escape. Collapses P1-P5 into a single command.
            (
                "overnight",
                [
                    "opened the agent's intent Ahd",
                    "reversible=2 compensatable=1 irreversible=1 unknown=0 confidence=partial",
                    "effect(s) attributed to intent Ahd",
                    "reversible effects retracted=2 compensations performed=1 refused_irreversible=1",
                    "kernel DENIED the out-of-intent Cairn write",
                    "boundary: intent-derivation ceiling",
                    "[overnight] PASS: the whole night is accounted for",
                ],
            ),
            # Audit the whole run: every denial attributed to a named boundary.
            (
                "why-denied all",
                [
                    "boundary: intent-derivation ceiling",
                    "denial(s) recorded; each attributable to a named boundary",
                ],
            ),
            # --- Leases + revocation (W8): bounded / withdrawable intent --------
            # A lease of 1 authorizes exactly one run then auto-revokes; a revoked
            # intent authorizes nothing. Provenance outlives the authority.
            (
                "lease-demo",
                [
                    "with lease=1",
                    "use #1 -> AUTHORIZED",
                    "use #2 -> DENIED (lease exhausted, intent auto-revoked)",
                    "use after revoke -> DENIED",
                    "[lease-demo] PASS",
                ],
            ),
            # --- Object-capabilities (the 'one big change') --------------------
            # A first-class handle to one object: attenuated delegation + per-
            # object generation-stamped revocation, which a bitmask cannot do.
            (
                "cap-demo",
                [
                    "B write (never delegated): DENIED (insufficient rights)",
                    "A read after revoke: REVOKED (stale generation)",
                    "B read after revoke (whole delegation subtree): REVOKED",
                    "C read after revoke (object 5, untouched): OK",
                    "forged: REVOKED (stale generation)",
                    "[cap-demo] PASS",
                ],
            ),
            # --- ocap migration: a LIVE namespace capability revoked at runtime -
            # The Cairn namespace capability is backed by ocap: revoking it bumps
            # the generation so the operator's held handle goes stale and the
            # storage path refuses further commits until re-granted.
            (
                "nsrevoke-demo",
                [
                    "runtime revocation of a LIVE namespace capability",
                    "[cap] DENIED: namespace 'calc' capability was REVOKED",
                    "[nsrevoke-demo] PASS",
                ],
            ),
            # --- ocap gate now covers the UNTRUSTED AGENT path ----------------
            # Revoking ns=lab refuses the built-in agent's Cairn write (it traps),
            # and re-granting restores it - enforcement spans the agent path.
            (
                "agentrevoke-demo",
                [
                    "cairn_put DENIED: ns capability revoked (ocap generation stale)",
                    "the agent's Cairn write was REFUSED by the ocap gate (agent trapped=true)",
                    "[agentrevoke-demo] PASS",
                ],
            ),
            # --- Confidentiality / anti-exfiltration (DIFC) --------------------
            # Reading a secret taints the agent so it can no longer write to a
            # public sink - the exfiltration defense the effect ledger cannot give.
            (
                "exfil-demo",
                [
                    "agent reads ns=vault (SECRET) -> its taint rises",
                    "send secret-tainted data -> public sink: DENIED (would leak a secret to a lower sink)",
                    "[exfil-demo] PASS",
                ],
            ),
            # --- Marz M1: the NIC the egress boundary will be built on ---------
            # The kernel discovers the virtio-net device itself; a Marz daemon
            # will be granted only that one page, never the whole MMIO window.
            (
                "net-probe",
                [
                    "virtio-net present: mmio_pa=",
                    "granted ONLY this page (cap TASK_DEVICE_VIRTIO_NET)",
                ],
            ),
            # Marz M2: the gate. Two sends are authorized and reach the wire; two
            # are refused (no destination capability / would export a secret) and
            # must leave NOTHING behind - the pcap frame count proves it.
            (
                "marz-demo",
                [
                    "[marz] virtio-net ready",
                    "EGRESS: frame left the machine",
                    "no capability for destination 'ops' -- egress authority names a destination",
                    "would export secret-tainted data to a destination cleared for",
                    "[marz-demo] PASS",
                ],
            ),
            # Marz M3: a real send is an irreversible, attributable effect that
            # rollback refuses - the wire cannot be undone and we do not pretend.
            (
                "marz-effect-demo",
                [
                    "recorded on the ledger as IRREVERSIBLE",
                    "irreversible=1",
                    "effect(s) attributed to intent Ahd#",
                    "already happened in the outside world; cannot be undone",
                    "[marz-effect-demo] PASS",
                ],
            ),
            # Device authority sits above the finer gates: revoking the NIC stops
            # every send regardless of destination capability.
            (
                "dev-demo",
                [
                    "device 'net' capability REVOKED (generation bumped)",
                    "DENIED: device 'net' capability was REVOKED",
                    "re-minted at the current generation",
                    "[dev-demo] PASS",
                ],
            ),
            # --- DIFC ENFORCED on the real storage path -----------------------
            # Read vault (secret) taints the operator; a commit to a public ns is
            # then refused (no write-down) until an explicit declassify.
            (
                "taintflow-demo",
                [
                    "read ns=vault (secret) -> the operator is tainted",
                    "[difc] DENIED: writing to ns='lab' would leak secret-tainted data to a lower sink",
                    "[declassify] operator taint cleared",
                    "[taintflow-demo] PASS",
                ],
            ),
            # Persisted namespace revocation: revoke ns=calc at the object owner
            # (the daemon writes it to the superblock). The reboot phase proves it
            # survives a power cycle.
            # The ingress half of information flow: what comes off the wire is not
            # secret, it is UNVALIDATED, and it must not become trusted state
            # without an explicit endorsement. Secrecy alone never catches this.
            (
                "ingress-demo",
                [
                    "operator integrity LOWERED by consuming input from the network",
                    "would let UNVALIDATED input become trusted state",
                    "operator integrity restored (privileged endorsement",
                    "the gate to ns=note reopens",
                    "PASS: INGRESS-OK",
                ],
            ),
            # The network is bidirectional now: the daemon arms the NIC's receive
            # queue, resolves the destination with ARP, sends a real ICMP echo and
            # PARSES the reply that comes back off the wire. Receiving is what a
            # transmit-only stack cannot fake.
            (
                "marz-ping ops",
                [
                    "receive queue armed (buffers offered to the NIC)",
                    "ARP reply received: the destination is reachable",
                    "PING-OK: ICMP echo reply received and matched (id+seq)",
                    "NET-RX-OK",
                ],
            ),
            # SMP again, on demand: re-run a parallel round from the console and
            # confirm the shared counter is coherent and >1 hart participated.
            (
                "smp-demo",
                [
                    "secondary harts started via SBI HSM = 3, checked in = 3",
                    "harts each applied 200000 atomic increments to ONE shared counter",
                    "COHERENT - the harts truly share memory and their atomics serialise",
                    "lock-guarded counter = 200000 (expected 200000) -> MUTEX-OK",
                    "each job ran exactly once -> QUEUE-OK",
                    "the core of a symmetric scheduler",
                ],
            ),
            # A real U-mode task dispatched onto a SECONDARY hart: its own
            # syscalls are serviced off the boot hart via the per-hart trap path,
            # and it runs to completion while the boot hart stays on the console.
            (
                "smp-task",
                [
                    "hello from a U-mode task running on a SECONDARY hart",
                    "my syscalls are being serviced off the boot hart",
                    "-> U-MODE-ON-AP",
                    "a U-mode task ran to completion on a hart other than the boot hart",
                ],
            ),
            # Symmetric scheduling: one task queue, every hart pulling from it,
            # several U-mode tasks executing at the same instant on different harts.
            (
                "smp-sched",
                [
                    "task -> hart placement:",
                    "-> SCHED-OK",
                ],
            ),
            # Parallelism did not cost isolation: each task has its own address
            # space, so a task reaching into a neighbour's stack page-faults.
            (
                "smp-isolate",
                [
                    "page-faulted on the cross-task write, killed on its own hart",
                    "-> ISOLATION-OK",
                ],
            ),
            ("ns-revoke calc", "namespace 'calc' REVOKED (persisted)"),
            # Devices now report completion instead of being polled blind.
            (
                "irq-stat",
                [
                    "external device interrupts serviced = ",
                    # The drivers sleep on hardware now; a nonzero wake count is
                    # the proof they are not busy-waiting.
                    "woken by a device interrupt (not by spinning) = ",
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

    # Marz: the traffic must exist on the wire, not merely in the transcript. The
    # capture is parsed as real packets rather than searched as a byte blob,
    # because the host also replies with ICMP errors that QUOTE our datagram - a
    # substring count would score those quotes as extra egress.
    blob = pcap_path.read_bytes()
    packets = parse_pcap(blob)
    egress = [p for p in packets if is_guest_udp_marker(p)]
    if len(egress) != 4:
        raise AssertionError(
            f"expected exactly 4 egress frames from the guest (the AUTHORIZED sends), "
            f"found {len(egress)} in a {len(packets)}-packet capture. More means a "
            "refused send leaked; fewer means an authorized send never left."
        )
    # The write-up send must have reached the destination cleared for secrets.
    if not any(p.get("dst_ip") == GUEST_VAULT_IP for p in egress):
        raise AssertionError("no frame addressed to vault-sync (10.0.2.3) in the capture")
    # The receive path: our echo request went out AND the host's reply came back.
    echo_req = [p for p in packets if p.get("icmp_type") == 8 and p.get("src_ip") == GUEST_IP]
    echo_rep = [p for p in packets if p.get("icmp_type") == 0 and p.get("dst_ip") == GUEST_IP]
    if not echo_req or not echo_rep:
        raise AssertionError(
            f"marz-ping did not produce a real exchange on the wire: "
            f"{len(echo_req)} echo requests out, {len(echo_rep)} replies back"
        )
    print(
        f"[marz] capture confirms the gate: {len(egress)} authorized frames on the wire, "
        f"refused sends left nothing; and the ping is real "
        f"({len(echo_req)} ICMP echo out, {len(echo_rep)} back) "
        f"across {len(packets)} captured packets"
    )

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
            # Marz (egress): a NIC for the guarded egress boundary. QEMU's user
            # networking needs no host privileges; filter-dump captures every
            # frame so CI can assert what actually left the machine.
            "-netdev",
            "user,id=n0",
            "-device",
            "virtio-net-device,netdev=n0",
        ],
        timeout=60,
    )
    try:
        session.wait_for("Dezh console. Every command requires an explicit capability.")
        session.wait_for("dezh> ")
        reboot_commands = [
            # Persisted namespace revocation survived the power cycle: the kernel
            # gate is fresh (live) after reboot, but the daemon still refuses
            # ns=calc from its superblock flag - object-owner-enforced revocation.
            ("cairn-commit calc reboot-x", "namespace 'calc' is REVOKED (persisted across reboot"),
            ("ns-grant calc", "re-granted (persisted revocation cleared)"),
            ("cairn-get note", "cairn value = \"note-v2"),
            ("cairn-get vault", "cairn value = \"ci-vault-secret"),
            ("cairn-verify note", "hash MATCH"),
            ("cairn-log note", "reversible=yes"),
            # Sand provenance is durable: after the mission rollback the head of
            # ns=agent is the irreversible send that rollback refused to undo,
            # and it — with its intent — survives a power cycle.
            ("sand-info agent", "intent=Ahd#4 derived=print,cairn-read,cairn-write reversibility=irreversible"),
            # The Ahd session itself is gone after reboot, but the mission's
            # provenance persists on the commits, so the forecast still resolves.
            ("sfar-plan 4", "irreversible=1"),
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
        for tmp in (disk_path, pcap_path):
            try:
                os.unlink(tmp)
            except OSError:
                pass


def run_x86_64(qemu: str, kernel: Path, iso: Path | None = None) -> None:
    # Two boot paths, same kernel, same asserted output: the QEMU `-kernel` PVH
    # note (developer loop) and the GRUB Multiboot2 ISO (`-cdrom`, the path that
    # also boots VirtualBox/VMware). Running both here keeps them honest.
    if iso is not None:
        boot = ["-cdrom", str(iso)]
    else:
        boot = ["-kernel", str(kernel)]
    session = QemuSession(
        [
            qemu,
            "-display",
            "none",
            "-serial",
            "stdio",
            "-no-reboot",
            *boot,
        ],
        timeout=30,
    )
    try:
        session.wait_for("Dezh x86_64")
        session.wait_for("long mode reached. 64-bit kernel running.")
        session.wait_for("IDT installed: 32 CPU-exception vectors")
        session.wait_for("plus 224 interrupt vectors on a path that saves state and returns")
        session.wait_for("Legacy 8259 PICs remapped to 0x20..0x2F and fully masked")
        session.wait_for("Dezh .dzp agent package (sum 1..=5 with a loop) on x86_64:")
        session.wait_for(".dzp verified: kind=dezh-ir, name=agent-sum")
        session.wait_for("[ir] => 15")
        session.wait_for("[ir] DENIED: agent holds no PRINT capability")
        # W16.1: a hardware timer interrupt that returns. The exception
        # assertions further down still end in halt — this path is the other
        # kind, where the interrupted work has to carry on afterwards.
        session.wait_for("[timer] Local APIC enabled, id=")
        # The rate is measured against the PIT, never assumed, so the count
        # itself is not asserted — only that a measurement was taken.
        session.wait_for("LAPIC counts in 10 ms at divide-16 (APIC bus ")
        session.wait_for("[timer] armed: vector 0x30, periodic, 100 Hz")
        session.wait_for("ticks; the work loop completed ")
        session.wait_for(
            "[timer] interrupts returned: work resumed after every tick, checksum OK"
        )
        # Masking the timer stops the ticks while the same loop keeps running:
        # what rules out the ticks having come from some other source.
        session.wait_for("[timer] masked: tick count frozen at ")
        # W16.2: the same interrupt path declining to resume what it interrupted.
        # Turn counts are asserted and round counts are not: a turn can only be
        # granted by the interrupt handler, while rounds only say how fast the
        # host is.
        session.wait_for("[sched] 3 tasks spawned, round-robin with the boot task")
        session.wait_for("[sched] first turns went to task 1 2 3 0 1 2 3 0")
        for task in (1, 2, 3):
            session.wait_for(f"[sched] task {task}: ")
            session.wait_for("checksum OK")
        session.wait_for("[sched] preemption works: every task was stopped and resumed")
        session.wait_for("[sched] each task read its own page through its own cr3")
        # W16.3: containment. Two CPL3 tasks, each in its own address space; one
        # reaches for an address it was never given. The privilege is asserted
        # from the `cs` the CPU saved (0x23 = ring 3), which a task cannot forge.
        session.wait_for("[user] task 4 (")
        session.wait_for("nothing else marked USER")
        session.wait_for("[trap] task 5 faulted at CPL3: page-fault touching 0x0000000000000000")
        session.wait_for("[trap] killing the task; the machine keeps running")
        session.wait_for("[user] calls arrived from CPL 3 (cs=0x0000000000000023)")
        session.wait_for("[user] task 5: 1 syscalls, then killed by the kernel (1 task killed, id 5)")
        session.wait_for(
            "[user] containment: the faulting task died, its neighbour ran on and finished"
        )
        # W16.4: authority on x86 is derived from an intent, not ambient. Two
        # CPL3 tasks, byte-identical code and manifest, different ceilings —
        # `granted = requested & ceiling`, computed by the same `dezh_core::mcap`
        # the RISC-V kernel uses.
        session.wait_for("[cap] manifest requests print uptime")
        session.wait_for("[cap] task 6 intent ceiling print uptime -> granted print uptime")
        session.wait_for("[cap] task 7 intent ceiling uptime -> granted uptime")
        session.wait_for("[cap] task 6 printed 42")
        session.wait_for("[cap] DENIED: task 7 holds no PRINT capability")
        session.wait_for(
            "[cap] authority is derived, not ambient: identical code, one refusal"
        )
        # M2: the IDT catches a deliberately-raised breakpoint instead of
        # triple-faulting the machine. The exception path still halts for a
        # kernel-mode fault; only a CPL3 fault costs just the task.
        session.wait_for("[trap] CPU exception 3 (breakpoint)")
        session.wait_for("[trap] halting")
    finally:
        transcript = session.text()
        print(transcript)
        session.stop()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("target", choices=["riscv64", "x86_64"])
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--qemu", required=True)
    parser.add_argument(
        "--iso",
        type=Path,
        default=None,
        help="x86_64 only: boot this GRUB ISO via -cdrom instead of -kernel",
    )
    args = parser.parse_args()

    if args.iso is None and not args.kernel.exists():
        print(f"kernel not found: {args.kernel}", file=sys.stderr)
        return 2
    if args.iso is not None and not args.iso.exists():
        print(f"iso not found: {args.iso}", file=sys.stderr)
        return 2

    try:
        if args.target == "riscv64":
            run_riscv64(args.qemu, args.kernel)
        else:
            run_x86_64(args.qemu, args.kernel, args.iso)
    except Exception as exc:
        msg = str(exc).replace("%", "%25").replace("\n", "%0A").replace("\r", "%0D")
        print(f"::error title=QEMU smoke failed::{msg}", file=sys.stderr)
        print(f"qemu smoke failed: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

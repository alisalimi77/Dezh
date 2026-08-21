#!/usr/bin/env python3
"""Ask a wedged kernel where each hart is.

A hang is the worst failure this project has to debug, because it produces
nothing: QEMU reports a timeout, the console stops answering, and the boot hart
— the only thing that can print — is often the one that is stuck. Every W13
defect note so far has been reasoning from what *stopped* appearing.

QEMU knows the answer. This asks it over QMP, resolves each hart's program
counter against the kernel's own symbol table, and prints one line per hart. No
gdb, no debug build flags, no change to the kernel.

    python tools/debug/hart_pcs.py --after 8 -c smp-task -c smp-console

Symbols are read straight out of the ELF rather than shelled out to `nm`,
because a RISC-V binutils is exactly the thing not installed on the machine that
needs this most.
"""
from __future__ import annotations

import argparse
import bisect
import json
import socket
import struct
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_KERNEL = ROOT / "dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot"


# --- the ELF symbol table ---------------------------------------------------

def load_symbols(path: Path) -> tuple[list[int], list[tuple[str, int]]]:
    """`(sorted addresses, [(name, size)])` for every function symbol."""
    blob = path.read_bytes()
    if blob[:4] != b"\x7fELF" or blob[4] != 2 or blob[5] != 1:
        raise SystemExit(f"{path.name}: not a little-endian ELF64")

    e_shoff, = struct.unpack_from("<Q", blob, 0x28)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", blob, 0x3A)

    sections = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        name_off, sh_type = struct.unpack_from("<II", blob, off)
        sh_offset, sh_size = struct.unpack_from("<QQ", blob, off + 0x18)
        sh_link, = struct.unpack_from("<I", blob, off + 0x28)
        sh_entsize, = struct.unpack_from("<Q", blob, off + 0x38)
        sections.append((name_off, sh_type, sh_offset, sh_size, sh_link, sh_entsize))

    SHT_SYMTAB = 2
    symtab = next((s for s in sections if s[1] == SHT_SYMTAB), None)
    if symtab is None:
        raise SystemExit(f"{path.name}: no .symtab (built with symbols stripped?)")

    _, _, sym_off, sym_size, str_idx, sym_entsize = symtab
    _, _, str_off, str_size, _, _ = sections[str_idx]
    strtab = blob[str_off : str_off + str_size]

    found: dict[int, tuple[str, int]] = {}
    for off in range(sym_off, sym_off + sym_size, sym_entsize or 24):
        st_name, st_info = struct.unpack_from("<IB", blob, off)
        st_value, st_size = struct.unpack_from("<QQ", blob, off + 8)
        if st_value == 0 or (st_info & 0xF) not in (1, 2):  # OBJECT or FUNC
            continue
        end = strtab.find(b"\0", st_name)
        name = strtab[st_name:end].decode("utf-8", "replace")
        if not name:
            continue
        # Prefer the symbol with a real size when two share an address.
        prev = found.get(st_value)
        if prev is None or (prev[1] == 0 and st_size > 0):
            found[st_value] = (name, st_size)

    addrs = sorted(found)
    return addrs, [found[a] for a in addrs]


def resolve(addrs: list[int], syms: list[tuple[str, int]], pc: int) -> str:
    if not addrs or pc < addrs[0]:
        return "?"
    i = bisect.bisect_right(addrs, pc) - 1
    name, size = syms[i]
    delta = pc - addrs[i]
    if size and delta >= size:
        return f"? (past {name})"
    return f"{name}+0x{delta:x}" if delta else name


# --- QEMU -------------------------------------------------------------------

class Machine:
    def __init__(self, kernel: Path, qemu: str, smp: int, port: int) -> None:
        self.proc = subprocess.Popen(
            [
                qemu, "-machine", "virt", "-smp", str(smp), "-display", "none",
                "-bios", "default", "-kernel", str(kernel),
                "-serial", "stdio",
                "-qmp", f"tcp:127.0.0.1:{port},server,nowait",
            ],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            bufsize=0,
        )
        self.output = bytearray()
        import threading
        threading.Thread(target=self._read, daemon=True).start()
        self.qmp = self._connect(port)

    def _read(self) -> None:
        assert self.proc.stdout
        while True:
            chunk = self.proc.stdout.read(1)
            if not chunk:
                return
            self.output.extend(chunk)

    def _connect(self, port: int, tries: int = 40):
        for _ in range(tries):
            try:
                sock = socket.create_connection(("127.0.0.1", port), timeout=5)
                f = sock.makefile("rwb")
                f.readline()  # greeting
                f.write(b'{"execute":"qmp_capabilities"}\n')
                f.flush()
                f.readline()
                return f
            except OSError:
                time.sleep(0.25)
        raise SystemExit("could not reach the QMP port")

    def hmp(self, command: str) -> str:
        payload = {"execute": "human-monitor-command",
                   "arguments": {"command-line": command}}
        self.qmp.write((json.dumps(payload) + "\n").encode())
        self.qmp.flush()
        while True:
            line = self.qmp.readline()
            if not line:
                return ""
            reply = json.loads(line)
            if "return" in reply:
                return reply["return"]
            if "error" in reply:
                return f"(error) {reply['error']}"

    def hart_pcs(self) -> list[int]:
        pcs: list[int] = []
        for line in self.hmp("info registers -a").splitlines():
            stripped = line.strip()
            if stripped.startswith("pc "):
                pcs.append(int(stripped.split()[1], 16))
        return pcs

    def send(self, line: str, per_char: float = 0.05) -> None:
        """Type it, one character at a time.

        The console loses pasted input — issue #19, a 16-byte UART FIFO with no
        flow control against a console that spends most of its time not in
        `getc`. Writing the whole line at once turned `smp-console` into `s-`,
        which is that defect, not a new one. A driver that trips over a known
        bug while measuring a different one is worse than no driver.
        """
        assert self.proc.stdin
        for ch in line + "\n":
            self.proc.stdin.write(ch.encode())
            self.proc.stdin.flush()
            time.sleep(per_char)

    def text(self) -> str:
        return self.output.decode("utf-8", "replace")

    def wait_for(self, needle: str, timeout: float) -> bool:
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if needle in self.text():
                return True
            if self.proc.poll() is not None:
                return False
            time.sleep(0.1)
        return False

    def kill(self) -> None:
        self.proc.kill()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--kernel", type=Path, default=DEFAULT_KERNEL)
    ap.add_argument("--qemu", default="qemu-system-riscv64")
    ap.add_argument("--smp", type=int, default=4)
    ap.add_argument("--port", type=int, default=45820)
    ap.add_argument("-c", "--command", action="append", default=[],
                    help="console command to send, in order; repeatable")
    ap.add_argument("--settle", type=float, default=6.0,
                    help="seconds to wait for the prompt before sending commands")
    ap.add_argument("--after", type=float, default=8.0,
                    help="seconds to wait after the last command before sampling")
    ap.add_argument("--samples", type=int, default=2,
                    help="how many times to sample, to tell a spin from a crawl")
    args = ap.parse_args()

    kernel = args.kernel if args.kernel.is_absolute() else ROOT / args.kernel
    addrs, syms = load_symbols(kernel)
    print(f"{len(addrs)} symbols from {kernel.name}")

    m = Machine(kernel, args.qemu, args.smp, args.port)
    try:
        if not m.wait_for("dezh>", args.settle):
            print("never reached the console prompt; sampling anyway")
        for command in args.command:
            print(f"--> {command}")
            m.send(command)
            time.sleep(1.0)

        time.sleep(args.after)
        previous: list[int] | None = None
        for shot in range(max(1, args.samples)):
            pcs = m.hart_pcs()
            print(f"\n=== sample {shot + 1} ===")
            for hart, pc in enumerate(pcs):
                moved = ""
                if previous is not None and hart < len(previous):
                    moved = "  (moved)" if previous[hart] != pc else "  (same)"
                print(f"  hart {hart}  pc=0x{pc:016x}  {resolve(addrs, syms, pc)}{moved}")
            previous = pcs
            if shot + 1 < args.samples:
                time.sleep(1.5)

        tail = m.text().rstrip().splitlines()[-6:]
        print("\n=== last console output ===")
        for line in tail:
            print("  " + line)
    finally:
        m.kill()
    return 0


if __name__ == "__main__":
    sys.exit(main())

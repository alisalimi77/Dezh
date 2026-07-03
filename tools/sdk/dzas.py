#!/usr/bin/env python3
"""dzas — the Dezh-IR assembler.

Turns human-writable `.dzs` text into Dezh-IR bytecode (the portable,
capability-gated stack machine in `dezh-core`). The same output bytes run on
every Dezh kernel (RISC-V today, x86_64 as it reaches parity), which is what
makes `.dzp` packages ISA-portable by construction.

Syntax (one instruction per line; `;` or `#` starts a comment):

    label:                ; define a jump/call target
    push 42               ; push a 64-bit signed immediate
    pop / dup / swap
    add sub mul div mod   ; pop b, pop a, push a OP b
    lt gt eq              ; comparisons -> 1/0
    load8 store8 load64 store64
    jmp LABEL / jz LABEL / jnz LABEL / call LABEL / ret
    hostcall print_num|print_str|cairn_put|cairn_get
    halt

Pseudo-instructions:

    string ADDR "text"    ; store the bytes of "text" at ADDR.. in linear memory
    prints ADDR LEN       ; push ADDR, push LEN, hostcall print_str
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

OPS0 = {
    "halt": 0x00,
    "pop": 0x02,
    "dup": 0x03,
    "swap": 0x04,
    "add": 0x10,
    "sub": 0x11,
    "mul": 0x12,
    "div": 0x13,
    "mod": 0x14,
    "lt": 0x18,
    "gt": 0x19,
    "eq": 0x1A,
    "load8": 0x20,
    "store8": 0x21,
    "load64": 0x22,
    "store64": 0x23,
    "ret": 0x41,
}
OP_PUSH = 0x01
BRANCHES = {"jmp": 0x30, "jz": 0x31, "jnz": 0x32, "call": 0x40}
OP_HOSTCALL = 0x50
HOSTCALLS = {"print_num": 0, "print_str": 1, "cairn_put": 2, "cairn_get": 3}


class AsmError(Exception):
    pass


def _parse_int(tok: str, line_no: int) -> int:
    try:
        return int(tok, 0)
    except ValueError:
        raise AsmError(f"line {line_no}: expected a number, got {tok!r}")


def _tokenize(source: str):
    """Yield (line_no, tokens) with comments stripped and strings intact."""
    for line_no, raw in enumerate(source.splitlines(), start=1):
        line = raw.strip()
        # Strip comments, but not inside a quoted string.
        out, in_str = [], False
        for ch in line:
            if ch == '"':
                in_str = not in_str
            if ch in ";#" and not in_str:
                break
            out.append(ch)
        line = "".join(out).strip()
        if not line:
            continue
        if '"' in line:
            head, _, rest = line.partition('"')
            text, _, tail = rest.partition('"')
            tokens = head.split() + [f'"{text}"'] + tail.split()
        else:
            tokens = line.split()
        yield line_no, tokens


def _expand(source: str):
    """First pass: expand pseudo-instructions into core instructions."""
    prog = []  # (line_no, mnemonic, args)
    for line_no, tokens in _tokenize(source):
        head = tokens[0].lower()
        if head.endswith(":") and len(tokens) == 1:
            prog.append((line_no, "label", [tokens[0][:-1]]))
        elif head == "string":
            if len(tokens) != 3 or not tokens[2].startswith('"'):
                raise AsmError(f'line {line_no}: usage: string ADDR "text"')
            addr = _parse_int(tokens[1], line_no)
            for i, byte in enumerate(tokens[2][1:-1].encode("utf-8")):
                prog.append((line_no, "push", [str(addr + i)]))
                prog.append((line_no, "push", [str(byte)]))
                prog.append((line_no, "store8", []))
        elif head == "prints":
            if len(tokens) != 3:
                raise AsmError(f"line {line_no}: usage: prints ADDR LEN")
            prog.append((line_no, "push", [tokens[1]]))
            prog.append((line_no, "push", [tokens[2]]))
            prog.append((line_no, "hostcall", ["print_str"]))
        else:
            prog.append((line_no, head, tokens[1:]))
    return prog


def assemble(source: str) -> bytes:
    prog = _expand(source)

    # Pass 1: compute each instruction's offset so labels resolve.
    def size_of(mnemonic: str) -> int:
        if mnemonic == "push":
            return 9
        if mnemonic in BRANCHES:
            return 3
        if mnemonic == "hostcall":
            return 2
        return 1

    labels, offset = {}, 0
    for line_no, mnemonic, args in prog:
        if mnemonic == "label":
            if args[0] in labels:
                raise AsmError(f"line {line_no}: duplicate label {args[0]!r}")
            labels[args[0]] = offset
        elif mnemonic in OPS0 or mnemonic == "push" or mnemonic in BRANCHES or mnemonic == "hostcall":
            offset += size_of(mnemonic)
        else:
            raise AsmError(f"line {line_no}: unknown instruction {mnemonic!r}")

    # Pass 2: emit.
    out = bytearray()
    for line_no, mnemonic, args in prog:
        if mnemonic == "label":
            continue
        if mnemonic == "push":
            if len(args) != 1:
                raise AsmError(f"line {line_no}: push takes one immediate")
            out.append(OP_PUSH)
            out += struct.pack("<q", _parse_int(args[0], line_no))
        elif mnemonic in BRANCHES:
            if len(args) != 1:
                raise AsmError(f"line {line_no}: {mnemonic} takes a label")
            target = labels.get(args[0])
            if target is None:
                raise AsmError(f"line {line_no}: undefined label {args[0]!r}")
            out.append(BRANCHES[mnemonic])
            out += struct.pack("<H", target)
        elif mnemonic == "hostcall":
            if len(args) != 1 or args[0] not in HOSTCALLS:
                raise AsmError(
                    f"line {line_no}: hostcall needs one of {', '.join(HOSTCALLS)}"
                )
            out.append(OP_HOSTCALL)
            out.append(HOSTCALLS[args[0]])
        else:
            if args:
                raise AsmError(f"line {line_no}: {mnemonic} takes no arguments")
            out.append(OPS0[mnemonic])
    return bytes(out)


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: dzas.py <input.dzs> <output.ir>", file=sys.stderr)
        return 2
    source = Path(sys.argv[1]).read_text(encoding="utf-8")
    try:
        code = assemble(source)
    except AsmError as exc:
        print(f"dzas: {exc}", file=sys.stderr)
        return 1
    Path(sys.argv[2]).write_bytes(code)
    print(f"dzas: {len(code)} bytes -> {sys.argv[2]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

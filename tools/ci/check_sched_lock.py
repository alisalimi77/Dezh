#!/usr/bin/env python3
"""Static check for a re-entrant take of the scheduler's ticket lock.

`sync::TicketLock` is not reentrant and it masks the acquiring hart's
interrupts, so taking it twice on one hart does not panic and corrupts nothing:
the hart waits for a lock only it can release, with the release on the far side
of the wait. The machine stops. In CI that surfaces as a QEMU timeout with no
diagnosis, which is why it is checked statically instead of being left to a run.

What it checks: for every scope opened by `SCHED_LOCK.lock()` in `sched.rs`, no
call inside that scope reaches a function that also takes the lock. The set of
lock-taking functions is computed transitively from the file itself, so a newly
added one is covered without editing this script.

What it does not check: a lock taken through a function pointer or a trait
object, and ordering against any other lock. Neither exists in this file today.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[2] / "dezh-boot" / "src" / "sched.rs"
LOCK = "SCHED_LOCK.lock()"
FN_RE = re.compile(
    r"^\s*(?:pub\(crate\)\s+|pub\s+)?(?:unsafe\s+)?(?:extern\s+\"C\"\s+)?fn\s+([A-Za-z0-9_]+)"
)
CALL_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
STRING_RE = re.compile(r'"(?:[^"\\]|\\.)*"')
DROP_RE = re.compile(r"\bdrop\s*\(\s*_held\s*\)")


def strip_noise(line: str) -> str:
    """Drop string literals and line comments so neither can fake a call."""
    return STRING_RE.sub('""', line).split("//", 1)[0]


def function_spans(lines: list[str]) -> dict[str, tuple[int, int]]:
    """Map each function name to the line range of its body, by brace depth."""
    spans: dict[str, tuple[int, int]] = {}
    stack: list[tuple[str, int, int]] = []
    depth = 0
    for i, line in enumerate(lines):
        match = FN_RE.match(line)
        pending = match.group(1) if match else None
        for ch in line:
            if ch == "{":
                if pending is not None:
                    stack.append((pending, depth, i))
                    pending = None
                depth += 1
            elif ch == "}":
                depth -= 1
                if stack and depth == stack[-1][1]:
                    name, _, start = stack.pop()
                    spans[name] = (start, i)
    return spans


def lock_taking(lines: list[str], spans: dict[str, tuple[int, int]]) -> set[str]:
    """Functions that take the lock directly, closed over their callers."""
    locking = {
        name
        for name, (start, end) in spans.items()
        if any(LOCK in lines[k] for k in range(start, end + 1))
    }
    changed = True
    while changed:
        changed = False
        for name, (start, end) in spans.items():
            if name in locking:
                continue
            for k in range(start, end + 1):
                if any(callee in locking for callee in CALL_RE.findall(lines[k])):
                    locking.add(name)
                    changed = True
                    break
    return locking


def reentrant_takes(lines: list[str], locking: set[str]) -> list[str]:
    """Every call to a lock-taking function that sits inside a live guard."""
    problems: list[str] = []
    for i, line in enumerate(lines):
        if LOCK not in line:
            continue
        depth = 0
        for j in range(i, len(lines)):
            body = lines[j]
            if j > i:
                for callee in CALL_RE.findall(body):
                    if callee in locking:
                        problems.append(
                            f"{SRC.name}:{j + 1}: `{callee}()` takes SCHED_LOCK, and sits "
                            f"inside the guard opened at line {i + 1}"
                        )
                # An explicit drop ends the guard early, which is how a run entry
                # hands the machine to `run_first` without leaking the lock.
                if DROP_RE.search(body):
                    break
            depth += body.count("{") - body.count("}")
            if j > i and depth < 0:
                break
    return problems


def main() -> int:
    lines = [strip_noise(l) for l in SRC.read_text(encoding="utf-8").splitlines()]
    spans = function_spans(lines)
    locking = lock_taking(lines, spans)
    problems = reentrant_takes(lines, locking)

    if problems:
        print("scheduler lock check FAILED -- a re-entrant take would hang the hart:")
        for problem in problems:
            print("  " + problem)
        return 1

    guards = sum(1 for line in lines if LOCK in line)
    print(
        f"scheduler lock check passed: {len(spans)} functions, "
        f"{len(locking)} of them take SCHED_LOCK, {guards} guard scopes, none re-entrant"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

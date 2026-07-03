#!/usr/bin/env python3
"""build-pkg — build a `.dzp` Dezh package from an app directory.

An app directory contains `app.toml` plus the entry source/binary:

    hello/
      app.toml          # name, version, kind, entry, caps
      hello.dzs         # Dezh-IR assembly (kind = "dezh-ir"), or
      hello.elf         # static riscv64 ELF (kind = "elf-riscv64")

app.toml (the kernel reads name/version/caps; kind/entry drive this tool):

    name = "hello"
    version = "0.1.0"
    kind = "dezh-ir"            # or "elf-riscv64"
    entry = "hello.dzs"         # .dzs (assembled), .ir (raw bytecode), or ELF
    caps = ["print"]            # print ipc uptime cairn-read cairn-write

Usage: build_pkg.py <app-dir> [-o out.dzp]
"""

from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import dzas  # noqa: E402

MAGIC = b"DZP1"
VERSION = 1
KINDS = {"dezh-ir": 1, "elf-riscv64": 2}
KNOWN_CAPS = {"print", "ipc", "uptime", "cairn-read", "cairn-write"}


def parse_manifest(text: str) -> dict:
    """The same key = value / quoted-list subset the kernel parses."""
    values: dict[str, object] = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key, value = key.strip(), value.strip()
        if value.startswith("["):
            items, rest = [], value[1 : value.rindex("]")] if "]" in value else value[1:]
            while '"' in rest:
                _, _, rest = rest.partition('"')
                item, _, rest = rest.partition('"')
                items.append(item)
            values[key] = items
        elif value.startswith('"'):
            values[key] = value[1:].partition('"')[0]
    return values


def build(app_dir: Path, out: Path | None) -> Path:
    manifest_path = app_dir / "app.toml"
    if not manifest_path.exists():
        raise SystemExit(f"build-pkg: no app.toml in {app_dir}")
    manifest_text = manifest_path.read_text(encoding="utf-8")
    manifest = parse_manifest(manifest_text)

    for key in ("name", "version", "kind", "entry"):
        if key not in manifest:
            raise SystemExit(f"build-pkg: app.toml is missing {key!r}")
    kind = manifest["kind"]
    if kind not in KINDS:
        raise SystemExit(f"build-pkg: kind must be one of {sorted(KINDS)}")
    caps = manifest.get("caps", [])
    unknown = sorted(set(caps) - KNOWN_CAPS)
    if unknown:
        raise SystemExit(
            f"build-pkg: unknown caps {unknown}; known: {sorted(KNOWN_CAPS)}"
        )

    entry = app_dir / str(manifest["entry"])
    if not entry.exists():
        raise SystemExit(f"build-pkg: entry not found: {entry}")
    if kind == "dezh-ir":
        if entry.suffix == ".dzs":
            payload = dzas.assemble(entry.read_text(encoding="utf-8"))
        else:
            payload = entry.read_bytes()  # pre-assembled .ir bytecode
    else:
        payload = entry.read_bytes()
        if payload[:4] != b"\x7fELF":
            raise SystemExit(f"build-pkg: {entry} is not an ELF")

    manifest_bytes = manifest_text.encode("utf-8")
    header = MAGIC + struct.pack(
        "<HHIII",
        VERSION,
        KINDS[kind],
        len(manifest_bytes),
        len(payload),
        zlib.crc32(manifest_bytes + payload) & 0xFFFFFFFF,
    )
    package = header + manifest_bytes + payload

    out = out or app_dir / f"{manifest['name']}-{manifest['version']}.dzp"
    out.write_bytes(package)
    print(
        f"build-pkg: {out} ({len(package)} bytes: manifest {len(manifest_bytes)}, "
        f"payload {len(payload)}, kind {kind}, caps {caps})"
    )
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app_dir", type=Path)
    parser.add_argument("-o", "--out", type=Path, default=None)
    args = parser.parse_args()
    build(args.app_dir, args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

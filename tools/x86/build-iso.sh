#!/usr/bin/env bash
# Build a bootable BIOS ISO of the Dezh x86_64 kernel using GRUB (multiboot2).
#
# This is the "install it like a real OS" path: the resulting ISO boots on any
# BIOS PC-class machine or VM (QEMU `-cdrom`, VirtualBox, VMware) — unlike the
# QEMU `-kernel` PVH path, which is a QEMU-only developer convenience.
#
# Requires: grub-mkrescue, xorriso, mtools, and the i386-pc GRUB modules
#   (Ubuntu: sudo apt-get install grub-pc-bin grub2-common xorriso mtools)
#
# Usage: tools/x86/build-iso.sh [kernel-elf] [output.iso]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

kernel="${1:-$repo/dezh-boot-x86/target/x86_64-unknown-none/debug/dezh-boot-x86}"
out="${2:-$repo/dezh-x86.iso}"

if [ ! -f "$kernel" ]; then
    echo "kernel not found: $kernel" >&2
    echo "build it first: (cd dezh-boot-x86 && cargo build)" >&2
    exit 2
fi

if ! grub-file --is-x86-multiboot2 "$kernel"; then
    echo "kernel is not a valid multiboot2 image: $kernel" >&2
    exit 3
fi

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
mkdir -p "$staging/boot/grub"
cp "$kernel" "$staging/boot/dezh-boot-x86"
cp "$here/grub.cfg" "$staging/boot/grub/grub.cfg"

grub-mkrescue -o "$out" "$staging" 2>/dev/null
echo "built $out ($(du -h "$out" | cut -f1))"

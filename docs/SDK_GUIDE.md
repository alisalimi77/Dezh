# Write Your First Dezh App in 10 Minutes

Dezh apps ship as `.dzp` packages: a manifest that *declares every capability
the app wants*, plus a payload. The kernel records those grants at install
time and checks them on every use at run time. Nothing is ambient: an app
that didn't declare `print` cannot print — the kernel denies the host call.

## Prerequisites

- Rust toolchain + `riscv64gc-unknown-none-elf` target (to build the kernel)
- `qemu-system-riscv64`
- Python 3.10+

Build the kernel once:

```sh
cd dezh-boot && cargo build
```

## 1. Copy the template (1 minute)

```sh
cp -r tools/sdk/templates/hello my-app
```

Two files:

- `app.toml` — name, version, and the capability list:

  ```toml
  name = "hello"
  version = "0.1.0"
  kind = "dezh-ir"
  entry = "hello.dzs"
  caps = ["print"]
  ```

- `hello.dzs` — the program, in Dezh-IR assembly (a tiny, verifiable stack
  machine; the same bytecode runs on every ISA Dezh is ported to):

  ```asm
      string 0 "hello from a .dzp package!"
      prints 0 26
      push 6
      push 7
      mul
      hostcall print_num
      halt
  ```

Edit the string, do some arithmetic — `dzas.py` documents the full
instruction set in its header.

## 2. Build the package (1 minute)

```sh
python tools/sdk/build_pkg.py my-app
# -> my-app/hello-0.1.0.dzp
```

## 3. Install and run it (2 minutes)

```sh
python tools/sdk/install_pkg.py my-app/hello-0.1.0.dzp --run hello
```

This boots Dezh in QEMU, streams the package over the UART through the
capability-gated console (`pkg-recv`), and runs it:

```
[pkg] installed 'hello' 0.1.0 kind=dezh-ir payload=536 bytes
[pkg] grants recorded at install time: print (kernel-enforced at run time)
--- pkg-run hello ---
  [ir] hello from a .dzp package!
  [ir] print -> 42
```

At install the kernel: checks a CRC-32, statically verifies the bytecode
(malformed programs never become runnable), and records the manifest grants.

## 4. See the denial (2 minutes)

Remove `"print"` from `caps` in `app.toml`, rebuild, reinstall, rerun:

```
[pkg-run] DENIED by kernel: missing required capability for this host call
```

Same bytes, different grant — the *authority* lives in the installed grant,
not in the program. That is the Dezh thesis in one demo.

## 5. Poke around (4 minutes)

Boot interactively (`pwsh dezh-boot/scripts/console-test.ps1` or the QEMU
one-liner in `dezh-boot/README.md`) and try:

- `pkg-list` — installed packages and their grants
- `pkg-info hello` — GRANTED vs DENIED for one package
- `pkg-remove hello` — grants are revoked with the package
- `audit` — install/run/deny events are recorded

## Capability vocabulary (v1)

| cap in `app.toml` | grants | payload kinds |
| --- | --- | --- |
| `print` | write to the console | dezh-ir, elf-riscv64 |
| `ipc` | send/receive typed IPC | elf-riscv64 |
| `uptime` | read the system clock | elf-riscv64 |
| `cairn-read` | read the app's Cairn namespace | dezh-ir (service lands in W2) |
| `cairn-write` | write the app's Cairn namespace | dezh-ir (service lands in W2) |

Unknown capability names make the *install fail* — an undeclared string never
silently grants anything. Device/DMA/MMIO authority is never grantable from a
manifest.

## Native (ELF) packages

`kind = "elf-riscv64"` with `entry = <static ELF>` packages a native program;
the kernel loads it into its own address space with exactly the manifest
grants. The Rust app crates under `dezh-boot/*-app/` show the target setup.
The end-to-end ELF story (including running unmodified Linux binaries) is the
W4 workstream.

## Honest limits (v1)

- Package registry is in RAM: installs don't survive reboot yet (W6).
- Payload arena is 128 KiB, 8 packages max; remove doesn't compact.
- Dezh-IR linear memory is 256 bytes, programs ≤ 4 KiB — demo-scale on
  purpose; it keeps the verifier and engine small enough to review.
- The reproducible test for everything on this page:
  `python tools/ci/sdk_test.py`

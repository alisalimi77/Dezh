# Dezh OS Review Demo Script

This script assumes the RISC-V kernel has been built and QEMU is available.

## Run The Automated Demo

```sh
python tools/demo/run_review_demo.py \
  --qemu-riscv qemu-system-riscv64 \
  --transcript docs/demo-transcript-riscv64.md
```

On Windows, pass the full QEMU path if it is not on `PATH`.

## Manual Command Sequence

At the `dezh>` prompt, run:

```text
version
about
ipc-typed-demo
ipcstat
services
install --dry-run
install run
apps installed
app-permissions lab
app-run lab
calc 7 + 5
calc-history
vault-put demo-secret
vault-get
app-deny vault
svc-stop virtio-block
read
svc-restart virtio-block
write recovered
read
svc-fault-demo virtio-block
read
svc-restart virtio-block
bench-all
halt
```

## Expected Signals

The transcript should include:

- `boot contract VALIDATED`
- `[typed-ipc] PASS`
- `VirtioBlock state=Running`
- `dry-run complete; disk not modified`
- `Install Report: Dezh Root v1`
- `[installed] lab`
- `[installed] calc`
- `[installed] vault`
- `Dezh Lab :: installable app system probe`
- `PASS: scheduler, IPC, installer launch, and UI path cooperated`
- `[calc] 7 + 5 = 12`
- `calc last = "7 + 5 = 12`
- `vault value = "demo-secret`
- `vault device/block direct access denied; console survived`
- `svc-stop virtio-block status=0 state=Stopped`
- `virtio-block unavailable; command failed cleanly`
- `svc-restart virtio-block state=Running`
- `svc-fault-demo virtio-block request_status=0 state=Faulted`
- `[bench-all] PASS`

## Short Review Path

For a shorter run, use:

```sh
python tools/demo/run_review_demo.py --mode short --qemu-riscv qemu-system-riscv64
```

The short run exercises boot, typed IPC, service startup, app install/run,
service stop/restart, service fault/restart, and halt.

# Contributing To Dezh OS

Dezh is a research operating-system prototype. Contributions should preserve
the core rule:

> No program, app, service, package, driver, or recovery path starts with
> ambient authority.

Every change that adds a behavior should make the authority path explicit,
testable, and auditable.

## Development Branches

- `develop` is the integration branch.
- `main` is the public review branch and should move by fast-forward only after
  validation.

## Expected Local Checks

Run the checks that match the change:

```sh
cargo test --locked --workspace
```

```sh
cd dezh-boot
cargo build --locked
cd ../dezh-boot-x86
cargo build --locked
cd ..
```

```sh
python tools/review/scan_public.py
```

For bare-metal changes, run the RISC-V smoke test:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

For package lifecycle changes, run:

```sh
python tools/ci/sdk_test.py \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64
```

## Design Rules

- Do not add hidden kernel block I/O paths.
- Do not give apps ambient filesystem, device, MMIO, DMA, IPC, or storage
  authority.
- Do not make package states other than `Active` runnable.
- Do not widen capabilities during recovery.
- Do not introduce silent repair for package, service, or root state.
- Do not make the service registry a global configuration database.
- Do not make app storage path-based or globally shared.
- Do not add a driver feature without an explicit grant model.
- Do not let a foreground task cleanup reclaim live daemon frames.

## Pull Request Requirements

Each pull request should include:

- motivation and scope
- changed security boundary, if any
- changed disk layout, package state, or IPC contract, if any
- tests run
- expected reviewer focus

## Commit Style

Prefer concise, specific commit messages:

```text
dezh-boot: add typed IPC timeout accounting
docs: clarify package recovery state machine
tools: add review demo runner
```

Avoid commit messages that depend on private context.

## Documentation

Any change that affects public behavior should update at least one of:

- `README.md`
- `docs/ARCHITECTURE.md`
- `docs/SECURITY_MODEL.md`
- `docs/SDK_GUIDE.md`
- `docs/REVIEWER_GUIDE.md`
- `docs/DEMO_SCRIPT.md`
- `CHANGELOG.md`

## Security Reports

Do not open public issues for suspected vulnerabilities. Follow
`SECURITY.md`.

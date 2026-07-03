# Git Workflow

Dezh uses a two-branch integration flow:

- `develop` is the active integration branch.
- `main` is the stable branch for coherent, tested milestones.

## Feature Work

Create focused branches from `develop`:

```sh
git switch develop
git pull
git switch -c feature/<short-name>
```

Use these prefixes:

- `feature/<name>` for product or kernel functionality.
- `fix/<name>` for bug fixes.
- `docs/<name>` for documentation-only work.
- `spike/<name>` for exploratory work.

## Required Validation

Before merging to `develop`:

```sh
cargo test --locked --workspace
cd dezh-boot && cargo build --locked && cd ..
cd dezh-boot-x86 && cargo build --locked && cd ..
```

For bare-metal changes, also run:

```sh
python tools/ci/qemu_smoke.py riscv64 \
  --kernel dezh-boot/target/riscv64gc-unknown-none-elf/debug/dezh-boot \
  --qemu qemu-system-riscv64

python tools/ci/qemu_smoke.py x86_64 \
  --kernel dezh-boot-x86/target/x86_64-unknown-none/debug/dezh-boot-x86 \
  --qemu qemu-system-x86_64
```

For external-review states, also run:

```sh
python tools/demo/run_review_demo.py --qemu-riscv qemu-system-riscv64
python tools/review/scan_public.py
```

## External Review Snapshot

External review material should be exported from a clean snapshot, not from a
branch with internal work-in-progress history. Use the review package tool:

```sh
python tools/review/make_review_package.py
```

The exported package should pass the public hygiene scan before distribution.

## Main Branch

Fast-forward `main` only after the milestone is coherent and the validation
commands above are green.

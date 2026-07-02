# Dezh Git Workflow

Dezh uses a simple two-branch flow:

- `main` is the stable branch. It should always represent a buildable,
  explainable project state.
- `develop` is the integration branch for active work. New development starts
  here and is merged to `main` only at coherent milestones.

## Branches

Create focused feature branches from `develop`:

```sh
git switch develop
git pull
git switch -c feature/docs-sync
```

Use short, descriptive prefixes:

- `feature/<name>` for new functionality.
- `fix/<name>` for bug fixes.
- `docs/<name>` for documentation-only changes.
- `spike/<name>` for throwaway research that may not merge.

## Merge Expectations

Before merging to `develop`:

```sh
cargo test
cargo build --manifest-path dezh-boot/Cargo.toml
```

Before merging `develop` to `main`, the project should also have an updated
status note in the docs when the milestone changes. For bare-metal work, prefer
running the QEMU console smoke test too:

```sh
pwsh dezh-boot/scripts/console-test.ps1
```

## Tags

Tag stable milestones from `main`:

```sh
git switch main
git pull
git tag -a v0.1-capability-kernel-demo -m "Dezh v0.1 capability kernel demo"
git push origin v0.1-capability-kernel-demo
```

Use tags for states worth showing, benchmarking, or referring to in architecture
docs. Routine work stays on `develop` and feature branches.

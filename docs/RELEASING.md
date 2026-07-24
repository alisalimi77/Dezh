# Releasing

Cutting a release, how packages relate to releases, and the branch and profile
conventions around them.

The notes for shipped releases stay in their own file,
[RELEASE_NOTES.md](RELEASE_NOTES.md), because the release workflow feeds that file
to GitHub verbatim.

---

## Release process

<!-- was docs/RELEASE_PROCESS.md until the 2026-07-23 consolidation -->

Dezh uses review releases rather than production releases at this stage.

### Release Goals

A review release should give an external reviewer:

- a fixed source revision
- repeatable CI evidence
- bootable QEMU kernel artifacts
- a review demo transcript
- SDK `.dzp` sample packages
- checksums and an artifact manifest
- a containerized review environment

### Version Names

Use tags in this shape:

```text
v0.1-review
v0.2-review
```

The suffix makes the release status explicit. These are not production OS
releases.

### Before Tagging

Run:

```sh
python tools/review/run_full_review.py --full
```

The full review suite validates:

- public hygiene
- host workspace tests
- RISC-V kernel build
- x86_64 kernel build
- RISC-V QEMU smoke
- x86_64 QEMU smoke
- review demo transcript
- SDK package lifecycle acceptance
- release artifact generation

### Create A Release

From `main`, after it has fast-forwarded from `develop`:

```sh
git tag -a v0.1-review -m "Dezh OS v0.1-review"
git push origin v0.1-review
```

Pushing the tag starts `.github/workflows/release.yml`.

### Release Artifacts

The release workflow attaches:

- `dezh-<tag>-riscv64-qemu-kernel.elf`
- `dezh-<tag>-x86_64-qemu-kernel.elf`
- `transcripts/riscv64.md`
- `dezh-<tag>-hello.dzp`
- `dezh-<tag>-review-docs.zip`
- `release-manifest.json`
- `SHA256SUMS`

### Container Package

The release workflow also publishes:

```text
ghcr.io/alisalimi77/dezh-review-env:<tag>
ghcr.io/alisalimi77/dezh-review-env:latest
```

These are GitHub Container Registry images, not Docker Hub images. They appear
under GitHub Packages when the package visibility is public.

This image contains Rust, Python, QEMU, and the Rust targets needed for review.

### Release Discipline

- Do not tag from a dirty tree.
- Do not create a release without passing the full review suite.
- Do not attach local disk images or ad-hoc binaries.
- Do not publish production claims in review release notes.
- Do not use GitHub Packages for app storage semantics; Dezh packages are
  `.dzp` artifacts and OS-managed package-store entries.

---

## Packages and releases

<!-- was docs/PACKAGES_AND_RELEASES.md until the 2026-07-23 consolidation -->

GitHub shows two related surfaces: Releases and Packages. Dezh uses both, but
for different purposes.

### Releases

Releases are the public review checkpoints.

Each release should contain:

- QEMU kernel artifacts
- a generated review transcript
- a sample SDK `.dzp` package
- documentation archive
- artifact manifest
- checksums

This lets a reviewer inspect a fixed point in the project without guessing
which commit, transcript, or binary was used.

### GitHub Packages

GitHub Packages is used for the review environment container image:

```text
ghcr.io/alisalimi77/dezh-review-env:<tag>
```

This is **not** a Docker Hub image. Dezh publishes the review environment to
GitHub Container Registry (GHCR), which is the container backend shown under
GitHub's Packages section.

Pull it with:

```sh
docker pull ghcr.io/alisalimi77/dezh-review-env:v0.1-review
```

The image is not the OS. It is the build-and-review environment: Rust targets,
Python, and QEMU.

If the package does not appear publicly on the repository sidebar, the GHCR
package visibility may need to be changed to public in GitHub's package
settings. The release workflow still publishes to GHCR, not Docker Hub.

### Dezh `.dzp` Packages

Dezh application packages are `.dzp` artifacts. They are installed into the OS
through the console and the service-mediated package store.

They are intentionally separate from GitHub Packages:

- GitHub Packages distributes host-side review tooling.
- `.dzp` packages exercise Dezh's own app installation model.
- OS package state remains capability-scoped, transactional, and auditable.

### Why Not Publish Every App To GitHub Packages?

The package-store design is part of the OS thesis. Treating every Dezh app as a
generic host package would hide the lifecycle that Dezh is trying to make
explicit: capability requests, install journal, registry state, rollback,
quarantine, and garbage collection.

For public review, release assets are enough. Later, Dezh can add a dedicated
package index with signatures, reproducible builds, and capability review.

---

## Git workflow

<!-- was docs/GIT_WORKFLOW.md until the 2026-07-23 consolidation -->

Dezh uses a two-branch integration flow:

- `develop` is the active integration branch.
- `main` is the stable branch for coherent, tested milestones.

### Feature Work

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

### Required Validation

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

### External Review Snapshot

External review material should be exported from a clean snapshot, not from a
branch with internal work-in-progress history. Use the review package tool:

```sh
python tools/review/make_review_package.py
```

The exported package should pass the public hygiene scan before distribution.

### Main Branch

Fast-forward `main` only after the milestone is coherent and the validation
commands above are green.

---

## GitHub profile

<!-- was docs/GITHUB_PROFILE.md until the 2026-07-23 consolidation -->

Use this text for the public GitHub repository profile.

### Description

Intent-native, capability-secure OS research prototype with user-space drivers,
typed IPC, transactional package lifecycle, and reboot-safe QEMU demos.

Shorter variant:

Capability-secure OS research prototype with user-space drivers, typed IPC, and
transactional package lifecycle.

### Suggested Topics

- operating-system
- research-os
- capability-security
- microkernel
- riscv
- qemu
- user-space-drivers
- typed-ipc
- package-management
- rust
- systems-programming
- sandboxing
- agent-sandbox
- ai-agents
- rollback

### Website

Leave empty for now unless a dedicated documentation site is published.

### Social Preview

Recommended preview concept:

- dark technical diagram background
- title: `Dezh OS`
- subtitle: `Intent-native. Capability-secure. Effect-accountable.`
- small visual motif: kernel boundary, U-mode services, package lifecycle

Do not use screenshots that expose local paths, private terminals, or
development-only artifacts.

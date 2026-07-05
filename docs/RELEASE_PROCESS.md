# Release Process

Dezh uses review releases rather than production releases at this stage.

## Release Goals

A review release should give an external reviewer:

- a fixed source revision
- repeatable CI evidence
- bootable QEMU kernel artifacts
- a review demo transcript
- SDK `.dzp` sample packages
- checksums and an artifact manifest
- a containerized review environment

## Version Names

Use tags in this shape:

```text
v0.1-review
v0.2-review
```

The suffix makes the release status explicit. These are not production OS
releases.

## Before Tagging

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

## Create A Release

From `main`, after it has fast-forwarded from `develop`:

```sh
git tag -a v0.1-review -m "Dezh OS v0.1-review"
git push origin v0.1-review
```

Pushing the tag starts `.github/workflows/release.yml`.

## Release Artifacts

The release workflow attaches:

- `dezh-<tag>-riscv64-qemu-kernel.elf`
- `dezh-<tag>-x86_64-qemu-kernel.elf`
- `demo-transcript-riscv64.md`
- `dezh-<tag>-hello.dzp`
- `dezh-<tag>-review-docs.zip`
- `release-manifest.json`
- `SHA256SUMS`

## Container Package

The release workflow also publishes:

```text
ghcr.io/alisalimi77/dezh-review-env:<tag>
ghcr.io/alisalimi77/dezh-review-env:latest
```

These are GitHub Container Registry images, not Docker Hub images. They appear
under GitHub Packages when the package visibility is public.

This image contains Rust, Python, QEMU, and the Rust targets needed for review.

## Release Discipline

- Do not tag from a dirty tree.
- Do not create a release without passing the full review suite.
- Do not attach local disk images or ad-hoc binaries.
- Do not publish production claims in review release notes.
- Do not use GitHub Packages for app storage semantics; Dezh packages are
  `.dzp` artifacts and OS-managed package-store entries.

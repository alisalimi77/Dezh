# Packages And Releases

GitHub shows two related surfaces: Releases and Packages. Dezh uses both, but
for different purposes.

## Releases

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

## GitHub Packages

GitHub Packages is used for the review environment container image:

```text
ghcr.io/alisalimi77/dezh-review-env:<tag>
```

The image is not the OS. It is the build-and-review environment: Rust targets,
Python, and QEMU.

## Dezh `.dzp` Packages

Dezh application packages are `.dzp` artifacts. They are installed into the OS
through the console and the service-mediated package store.

They are intentionally separate from GitHub Packages:

- GitHub Packages distributes host-side review tooling.
- `.dzp` packages exercise Dezh's own app installation model.
- OS package state remains capability-scoped, transactional, and auditable.

## Why Not Publish Every App To GitHub Packages?

The package-store design is part of the OS thesis. Treating every Dezh app as a
generic host package would hide the lifecycle that Dezh is trying to make
explicit: capability requests, install journal, registry state, rollback,
quarantine, and garbage collection.

For public review, release assets are enough. Later, Dezh can add a dedicated
package index with signatures, reproducible builds, and capability review.

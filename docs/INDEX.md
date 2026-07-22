# Documentation Index

This page is the primary navigation point for Dezh OS public review.

## Start Here

- [README](../README.md): project thesis, current capabilities, and quick commands.
- [Getting Started](GETTING_STARTED.md): shortest path from clone to first
  validation.
- [Build And Run](BUILD_AND_RUN.md): detailed build, QEMU, Windows, Linux, and
  macOS notes.
- [Reviewer Guide](REVIEWER_GUIDE.md): what to inspect and how to evaluate the
  prototype, organized around the four flagship demos.
- [Status and Limitations](STATUS.md): one honest page on what is and is not
  true today.
- [VM Quickstart](QUICKSTART_VM.md): boot a release in VirtualBox/VMware or QEMU.

## Architecture

- [Architecture](ARCHITECTURE.md): boot flow, process model, IPC, service
  registry, user-space driver path, package lifecycle.
- [Architecture Diagrams](ARCHITECTURE_DIAGRAMS.md): Mermaid diagrams for the
  major system flows.
- [Whitepaper](WHITEPAPER.md): the v1 design paper — thesis, system model,
  mechanisms, evaluation, novelty, and limitations.
- [Related Work and Novelty](RELATED_WORK.md): where Dezh sits in the literature
  (capabilities, DIFC/provenance, sagas), what is reused, and the precise,
  honest novelty claim.
- [Security Model](SECURITY_MODEL.md): capability boundaries, known gaps, and
  review focus.
- [Threat Model](THREAT_MODEL.md): what Dezh defends, what it does not, the
  trusted computing base, and the head-to-head vs user-space sandboxes.
- [Strategic Direction](STRATEGIC_DIRECTION.md): long-term architecture plan.
- [Roadmap](ROADMAP.md): milestone sequence and acceptance criteria.

## Running Evidence

- [Demo Script](DEMO_SCRIPT.md): interactive review script and expected output.
- [RISC-V Demo Transcript](demo-transcript-riscv64.md): recorded review demo.
- [Agent Demo Transcript](demo-transcript-agent-f1.md): agent-containment demo.
- [SDK Guide](SDK_GUIDE.md): `.dzp` package format and package lifecycle.

## Project Process

- [Architecture Decisions](DECISIONS.md): decision log.
- [Repo Structure](REPO_STRUCTURE.md): workspace map and ownership boundaries.
- [Git Workflow](GIT_WORKFLOW.md): branch and release flow.
- [Outreach](OUTREACH.md): public-review outreach templates.
- [FAQ](FAQ.md): direct answers for reviewers.
- [Release Notes](RELEASE_NOTES.md): current review-candidate summary.
- [Release Process](RELEASE_PROCESS.md): how review releases are tagged,
  validated, and published.
- [Packages And Releases](PACKAGES_AND_RELEASES.md): how GitHub Releases,
  GitHub Packages, and Dezh `.dzp` packages differ.

## Governance

- [License](../LICENSE)
- [Security Policy](../SECURITY.md)
- [Contributing](../CONTRIBUTING.md)
- [Code Of Conduct](../CODE_OF_CONDUCT.md)
- [Changelog](../CHANGELOG.md)

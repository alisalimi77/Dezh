# Repository Structure

This repository mixes a bare-metal OS prototype, host-side research crates, SDK
tooling, QEMU test harnesses, and public review documentation. This file is the
map for reviewers.

## Bare-Metal Targets

| Path | Role |
| --- | --- |
| `dezh-boot/` | Main RISC-V QEMU `virt` boot target. Contains kernel entry, console, task model, service registry, package store, package lifecycle, embedded apps, and user-space process launch. |
| `dezh-boot/virtio-blk/` | User-space `virtio-block` daemon. It receives explicit MMIO and DMA grants and performs the prototype disk I/O path. |
| `dezh-boot/userprog/` | Small user program used by legacy demos and process-launch smoke paths. |
| `dezh-boot/bench-app/` | U-mode benchmark app used by `bench-all`. |
| `dezh-boot/note-app/` | Embedded note demo app. |
| `dezh-boot/lab-app/` | Embedded multi-task lab demo app. |
| `dezh-boot/calc-app/` | Embedded calculator demo app. |
| `dezh-boot/vault-app/` | Embedded private-value demo app. |
| `dezh-boot-x86/` | Smaller x86_64 boot/smoke target for multi-ISA validation. |

## Shared Crates

| Path | Role |
| --- | --- |
| `dezh-core/` | Shared `.dzp`, base64, and Dezh-IR support used by the boot target and SDK-adjacent code. |
| `dezh-kernel/` | Boot contract, kernel plan, install manifest, and plan validation logic. |
| `dezh-ir/` | Dezh IR contract crate. |
| `dezh-cairn/` | Host-side persistent object/ref prototype. |
| `dezh-host/` | Host capability model experiments and tests. |
| `dezh-ipc/` | Host-side IPC/capability experiments. |
| `dezh-identity/` | Delegation and invocation-chain experiments. |
| `dezh-runtime/` | Host-side runtime boundary experiments. |
| `dezh-linux/` | Compatibility and authority experiments for Linux-like paths. |
| `dezh-scheduler/` | Scheduling-policy experiments. |

## Tools

| Path | Role |
| --- | --- |
| `tools/ci/qemu_smoke.py` | Boots RISC-V or x86_64 QEMU targets and asserts expected console behavior. |
| `tools/ci/sdk_test.py` | End-to-end SDK/package lifecycle acceptance test across multiple QEMU reboots. |
| `tools/sdk/build_pkg.py` | Builds `.dzp` packages from app directories. |
| `tools/sdk/install_pkg.py` | Boots Dezh in QEMU and streams packages through the console upload protocol. |
| `tools/sdk/dzas.py` | Tiny Dezh-IR assembler for SDK apps. |
| `tools/demo/run_review_demo.py` | Runs the review demo and captures a transcript. |
| `tools/demo/run_agent_demo.py` | Runs an agent-containment demo transcript. |
| `tools/review/scan_public.py` | Public hygiene scan for review-package readiness. |
| `tools/review/make_review_package.py` | Builds a clean review package snapshot. |

## Documentation

| Path | Role |
| --- | --- |
| `README.md` | Public landing page and quick review path. |
| `docs/ARCHITECTURE.md` | Architecture explanation. |
| `docs/ARCHITECTURE_DIAGRAMS.md` | Mermaid diagrams for the current prototype. |
| `docs/SECURITY_MODEL.md` | Threat model and enforced/not-yet-enforced boundaries. |
| `docs/STRATEGIC_DIRECTION.md` | Intent-native/effect-accountable direction and open review questions. |
| `docs/SDK_GUIDE.md` | How to build, install, update, and run `.dzp` packages. |
| `docs/REVIEWER_GUIDE.md` | Short path for external technical review. |
| `docs/ROADMAP.md` | Roadmap and current milestone direction. |
| `docs/DECISIONS.md` | Architecture decision notes. |
| `docs/DEMO_SCRIPT.md` | Manual demo script. |
| `docs/WHITEPAPER.md` | Technical whitepaper draft. |
| `docs/OUTREACH.md` | Draft outreach templates. |

## Generated/Local Artifacts

These should not be committed:

- `target/`
- `dist/`
- `graphify-out/`
- raw QEMU disk images (`*.img`)
- Python bytecode caches

The repository intentionally keeps reproducible tools and transcripts, but not
local generated build output.

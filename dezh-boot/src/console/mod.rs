//! The operator console: the REPL, the dispatcher, and the reporting verbs.
//!
//! Reads a line, looks the command up in `commands::COMMANDS`, refuses it if
//! the console does not hold the required capability, and otherwise runs it.
//! The status/tasks/memory/version reporting verbs live here because they are
//! the console talking about itself.
//!
//! Last of the big splits. It went last because it depends on everything: the
//! dispatcher names every subsystem, so it could only be moved once each of
//! them had a real interface to name.

pub(crate) mod cmds;
pub(crate) mod commands;

// The dispatcher's job is to name every subsystem, so it imports every
// subsystem. Enumerating ~180 names here would measure nothing except how many
// verbs the console has - the same reasoning `abi` got its glob for, and the
// opposite of `proc::loader`, where the list was evidence of coupling that
// should shrink. This list should not shrink; it should grow with the console.
use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::abi::*;
use crate::console::cmds::*;
use crate::dev::plic::{irq_stat, plic_init};
use crate::apps::*;
use crate::arch::finisher::{shutdown, FINISH_FAIL, FINISH_PASS};
use crate::arch::timer::{rdtime, sbi_set_timer, SKIP_LF_AFTER_CR, TICKS, TIMER_DELTA};
use crate::audit::{print_audit, print_events, record_event, why_denied};
use crate::bench::*;
use crate::cairn::console::*;
use crate::console::commands::COMMANDS;
use crate::demos::cairn::*;
use crate::demos::difc::*;
use crate::demos::egress::*;
use crate::demos::ocap::*;
use crate::demos::smp::*;
use crate::difc::{declassify, endorse, taint_show};
use crate::mm::frames::{frame_alloc, frame_free, frames_init, FRAME_FREE, FRAME_TOTAL};
use crate::mm::paging::{build_page_tables, enable_paging};
use crate::net::marz::{marz_dest_authority, marz_effect, run_marz_ping, run_marz_send};
use crate::ocap::device::dev_authority_set;
use crate::ocap::ns::{ns_grant, ns_revoke};
use crate::proc::loader::{ProcessSpec, TaskKind};
use crate::sched::*;
use crate::service::*;
use crate::smp::{smp_bringup, smp_report_boot};
use crate::vblk::*;
use crate::*;

pub(crate) fn cap_names(set: u32) -> &'static str {
    match set {
        s if s == cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT | cap::SPAWN => {
            "INSPECT TIME ECHO HALT SPAWN"
        }
        _ => "(custom set)",
    }
}

/// The order `help` prints the command groups in, and - because it is a hand
/// written list next to a table that grows - the thing that decides whether a
/// command is discoverable at all.
const GROUPS: &[&str] = &[
    "Inspect", "Storage", "Install", "Packages", "Apps", "Services", "Intent", "Effects", "Audit",
    "Safety", "Demos", "Power",
];

/// Every group named in `COMMANDS` must appear in `GROUPS`, or `help` skips its
/// commands in silence.
///
/// That is not a hypothetical. `Intent` and `Effects` were absent from this list
/// while 40 of the 151 commands claimed them - the entire intent-to-effect
/// surface, `overnight` and `redteam` included - so the one screen a reviewer
/// types first did not mention the thing the system is about. Nothing failed; a
/// list simply fell behind a table.
///
/// A runtime check would report that to a console nobody was reading, so the
/// check happens when the kernel is built: add a group to a command and forget
/// this list, and the build stops.
const _: () = {
    const fn str_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    let mut i = 0;
    while i < COMMANDS.len() {
        let mut listed = false;
        let mut j = 0;
        while j < GROUPS.len() {
            if str_eq(COMMANDS[i].group, GROUPS[j]) {
                listed = true;
            }
            j += 1;
        }
        assert!(
            listed,
            "a command's group is missing from console::GROUPS, so `help` would not list it"
        );
        i += 1;
    }
};

pub(crate) fn print_help(held: u32) {
    kprintln!("commands (cap required -> held?):");
    for group in GROUPS {
        kprintln!("  [{}]", group);
        for c in COMMANDS {
            if c.group != *group {
                continue;
            }
            let ok = if c.cap == 0 || held & c.cap == c.cap {
                "yes"
            } else {
                "DENIED"
            };
            kprintln!("    {:<13} {:<8} [{}]  {}", c.name, c.cap_name, ok, c.help);
        }
    }
}

pub(crate) fn print_command_help(name: &str, held: u32) {
    let wanted = name.trim();
    if wanted.is_empty() {
        print_help(held);
        return;
    }
    for c in COMMANDS {
        if c.name == wanted {
            let ok = if c.cap == 0 || held & c.cap == c.cap {
                "yes"
            } else {
                "DENIED"
            };
            kprintln!("help: {}", c.name);
            kprintln!("  group: {}", c.group);
            kprintln!("  requires: {} ({})", c.cap_name, ok);
            kprintln!("  usage: {}", command_usage(c.name));
            kprintln!("  about: {}", c.help);
            return;
        }
    }
    kprintln!("help: unknown command '{wanted}'");
}

pub(crate) fn command_usage(name: &str) -> &str {
    match name {
        "install" => "install plan|check|run|verify|report|rollback|--dry-run",
        "pkg-info" => "pkg-info <name>",
        "pkg-run" => "pkg-run <name>",
        "pkg-remove" => "pkg-remove <name>",
        "pkg-store" => "pkg-store",
        "pkg-journal" => "pkg-journal",
        "pkg-recover" => "pkg-recover",
        "pkg-verify" => "pkg-verify <name>",
        "pkg-fault" => {
            "pkg-fault <install-after-blob|install-pending-registry|remove-pending|corrupt-journal>"
        }
        "pkg-gc" => "pkg-gc [plan|run]",
        "pkg-update" => "pkg-update <name> [--allow-new-caps]",
        "pkg-rollback" => "pkg-rollback <name> [--force]",
        "pkg-versions" => "pkg-versions <name>",
        "pkg-review" => "pkg-review <name>",
        "pkg-pin" => "pkg-pin <name>",
        "pkg-unpin" => "pkg-unpin <name>",
        "pkg-retire" => "pkg-retire <name>",
        "pkg-lifecycle" => "pkg-lifecycle",
        "pkg-audit" => "pkg-audit <name>",
        "calc" => "calc <n> <+|-|*|/> <n>",
        "vault-put" => "vault-put <text>",
        "app-permissions" => "app-permissions <note|lab|calc|vault>",
        "explain" => "explain <command>",
        "svc-stop" => "svc-stop virtio-block",
        "svc-restart" => "svc-restart virtio-block",
        "svc-fault-demo" => "svc-fault-demo virtio-block",
        _ => name,
    }
}

pub(crate) fn print_status(plan: &KernelPlan, memory: &[MemoryRegion], held: u32) {
    refresh_virtio_service_state();
    let ticks = TICKS.load(Ordering::Relaxed);
    let usable_regions = memory
        .iter()
        .filter(|r| r.kind == MemoryKind::Usable)
        .count();
    let running_services = running_service_count();
    kprintln!("status:");
    kprintln!("  target: {:?}", plan.target);
    kprintln!(
        "  uptime: {} ticks (~{}.{} s)",
        ticks,
        ticks / TIMER_HZ,
        ticks % TIMER_HZ
    );
    kprintln!(
        "  memory: {} bytes usable across {} usable region(s)",
        plan.usable_bytes,
        usable_regions
    );
    kprintln!(
        "  services: {} declared, {} running",
        plan.services.len(),
        running_services
    );
    kprintln!(
        "  install: root={} block={} marker_sector={} root_metadata_sector={}",
        plan.install_manifest.root_service,
        plan.install_manifest.block_service,
        plan.install_manifest.layout.marker_sector,
        plan.install_manifest.layout.root_metadata_sector
    );
    kprintln!("  console caps: {}", cap_names(held));
}

pub(crate) fn task_state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Unused => "Unused",
        TaskState::Ready => "Ready",
        TaskState::Blocked => "Blocked",
        TaskState::Done => "Done",
    }
}

pub(crate) fn print_tasks() {
    refresh_virtio_service_state();
    {
        kprintln!("tasks:");
        let mut i = 0usize;
        while i < MAX_TASKS {
            let row = task_row(i);
            kprintln!(
                "  task{} state={:<7} kind={:<10} frames={:<3} caps={:#x} exit={} service={}",
                i,
                task_state_name(row.state),
                task_kind_name(row.kind),
                task_owned_frames(i),
                row.caps,
                row.exit,
                service_for_task(i)
            );
            i += 1;
        }
    }
}

pub(crate) fn print_memstat() {
    let total = unsafe { *FRAME_TOTAL.get() };
    let free = unsafe { *FRAME_FREE.get() };
    let used = total.saturating_sub(free);
    let process_owned = process_owned_frames();
    let daemon_owned = owned_frames_by_kind(TaskKind::Daemon);
    let foreground_owned = owned_frames_by_kind(TaskKind::Foreground);
    let unowned = used.saturating_sub(process_owned);
    kprintln!("memstat:");
    kprintln!("  frames: total={} free={} used={}", total, free, used);
    kprintln!(
        "  owned: process={} daemon={} foreground={}",
        process_owned,
        daemon_owned,
        foreground_owned
    );
    kprintln!("  unowned allocated estimate={}", unowned);
}

pub(crate) fn print_version() {
    kprintln!("Dezh OS review prototype v0.2-control-surface");
    kprintln!("  kernel: riscv64 qemu-virt S-mode");
    kprintln!("  ipc: typed v0 with timeout/status");
    kprintln!("  installer: v1 UX over v0 disk layout");
}

pub(crate) fn print_about() {
    kprintln!("Dezh OS: capability-secure research prototype");
    kprintln!("  thesis: no ambient authority; every effect needs an explicit grant");
    kprintln!("  current: U-mode apps, user-space virtio-block, typed IPC, installer/app registry");
    kprintln!("  review focus: authority visibility, service recovery, app install/run/storage");
}

pub(crate) fn print_caps_why(arg: &str) {
    match arg.trim() {
        "note-get" | "read" | "root-status" => {
            kprintln!("caps why {}:", arg.trim());
            kprintln!("  console requires: INSPECT");
            kprintln!("  foreground client receives: PRINT IPC BLOCK_READ BLOCK_WRITE");
            kprintln!("  device access remains only in virtio-block daemon");
        }
        "app-run lab" | "app-run" | "calc" | "vault-put" => {
            kprintln!("caps why {}:", arg.trim());
            kprintln!("  console requires: SPAWN");
            kprintln!("  app receives: PRINT IPC only");
            kprintln!("  denied: DEVICE_VIRTIO_BLK DMA BLOCK_DIRECT MMIO");
        }
        "install run" | "install" => {
            kprintln!("caps why install run:");
            kprintln!("  console requires: SPAWN");
            kprintln!("  installer path uses: registered virtio-block service");
            kprintln!("  service owns: DEVICE_VIRTIO_BLK DMA BLOCK_READ BLOCK_WRITE");
        }
        _ => kprintln!(
            "caps why: try `caps why install run`, `caps why app-run lab`, or `caps why note-get`"
        ),
    }
}

pub(crate) fn console(plan: &KernelPlan, memory: &[MemoryRegion], held: u32) -> ! {
    kprintln!();
    kprintln!("Dezh console. Every command requires an explicit capability.");
    kprintln!("Type 'help'. The console holds: {}", cap_names(held));

    let mut buf = [0u8; 128];
    loop {
        kprint!("dezh> ");
        let len = read_line(&mut buf);
        let line = core::str::from_utf8(&buf[..len]).unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        dispatch(cmd, arg, plan, memory, held);
    }
}

pub(crate) fn dispatch(cmd: &str, arg: &str, plan: &KernelPlan, memory: &[MemoryRegion], held: u32) {
    let spec = match COMMANDS.iter().find(|c| c.name == cmd) {
        Some(s) => s,
        None => {
            kprintln!("unknown command: {cmd} (try 'help')");
            return;
        }
    };

    if spec.cap != 0 && held & spec.cap != spec.cap {
        kprintln!(
            "denied: '{}' requires capability {} (not held)",
            cmd,
            spec.cap_name
        );
        return;
    }

    match cmd {
        "help" => {
            print_command_help(arg, held);
        }
        "version" => print_version(),
        "about" => print_about(),
        "clear" => kprint!("\x1b[2J\x1b[H"),
        "explain" => explain_command(arg),
        "caps" => {
            if let Some(rest) = arg.strip_prefix("why ") {
                print_caps_why(rest);
            } else {
                kprintln!("console capabilities: {}", cap_names(held));
            }
        }
        "mem" => {
            kprintln!("usable memory: {} bytes", plan.usable_bytes);
            for r in memory {
                let end = r.start + r.len;
                kprintln!("  {:#012x}..{:#012x}  {:?}", r.start, end, r.kind);
            }
        }
        "status" => print_status(plan, memory, held),
        "tasks" => print_tasks(),
        "memstat" => print_memstat(),
        "ipcstat" => print_ipcstat(),
        "ipc-typed-demo" => run_ipc_typed_demo(),
        "disk" => {
            kprintln!("[kernel] user-space virtio-blk: first prove no device cap means no MMIO");
            run_virtio_no_grant_probe();
            kprintln!("[kernel] no-grant probe returned; console survived");
            run_registered_virtio_client(plan, BLK_REQ_PROBE, "");
        }
        "bwrite" => run_registered_virtio_client(plan, BLK_REQ_BWRITE, ""),
        "bread" => run_registered_virtio_client(plan, BLK_REQ_BREAD, ""),
        "pset" => run_registered_virtio_client(plan, BLK_REQ_PSET, arg),
        "pget" => run_registered_virtio_client(plan, BLK_REQ_PGET, ""),
        "prollback" => run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, ""),
        "write" => run_registered_virtio_client(plan, BLK_REQ_PSET, arg),
        "read" => run_registered_virtio_client(plan, BLK_REQ_PGET, ""),
        "rollback" => run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, ""),
        "history" => {
            kprintln!("[storage] Cairn v0 keeps current sector 2 and previous sector 3");
            kprintln!("[storage] current value:");
            run_registered_virtio_client(plan, BLK_REQ_PGET, "");
            kprintln!("[storage] for the full commit history use `cairn-log <ns>` (Cairn v1)");
        }
        "cairn-status" => {
            let _ = run_registered_virtio_client_ns(plan, BLK_REQ_CAIRN_STATUS, "", 0);
        }
        "cairn-commit" => cairn_cmd_commit(plan, arg),
        "cairn-get" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_GET, arg),
        "cairn-log" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_LOG, arg),
        "cairn-verify" => cairn_cmd_simple(plan, BLK_REQ_CAIRN_VERIFY, arg),
        "cairn-rollback" => cairn_cmd_rollback(plan, arg),
        "cairn-demo" => run_cairn_demo(plan),
        "sand-log" => sand_cmd(plan, BLK_REQ_SAND_LOG, arg),
        "sand-info" => sand_cmd(plan, BLK_REQ_SAND_INFO, arg),
        "sand-demo" => run_sand_demo(plan),
        "sfar-plan" => sfar_cmd(plan, BLK_REQ_SFAR_PLAN, arg),
        "sfar-rollback" => sfar_cmd(plan, BLK_REQ_SFAR_ROLLBACK, arg),
        "sfar-demo" => run_sfar_demo(plan),
        "sfar-cross-demo" => run_sfar_cross_demo(plan),
        "comp-demo" => run_comp_demo(plan),
        "redteam" => run_redteam(plan),
        "why-denied" => why_denied(arg),
        "tbar" => sfar_cmd(plan, BLK_REQ_TBAR, arg),
        "overnight" => run_overnight(plan),
        "vblkd" => {
            kprintln!("[kernel] exercising registered virtio-blk daemon with IPC client");
            kprintln!("[kernel] daemon gets DEVICE+DMA+IPC; client gets IPC+DMA only (no MMIO)");
            run_virtio_blk_daemon_demo(plan);
            kprintln!("[kernel] virtio-blk daemon demo done; back in the console");
        }
        "agent" => {
            use dezh_core::ir;
            kprintln!("[kernel] Dezh-IR (shared dezh-core engine): verified, capability-gated");
            let mut buf = [0u8; 512];
            let sum = ir::demo_sum(&mut buf);
            if let Err(t) = ir::verify(sum) {
                kprintln!("  verify failed: {}", t.msg());
            } else {
                kprintln!("  prog 1 (loop: sum 1..=5, then print) WITH the PRINT capability:");
                let mut h = KHost {
                    caps: ir::CAP_PRINT,
                    cairn: None,
                    intent: 0,
                    derived: 0,
                };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
                kprintln!("  prog 1 again WITHOUT the PRINT capability:");
                let mut h = KHost {
                    caps: 0,
                    cairn: None,
                    intent: 0,
                    derived: 0,
                };
                if let Err(t) = ir::run(sum, &mut h) {
                    kprintln!("  [ir] TRAP: {}", t.msg());
                }
            }
            let mut buf2 = [0u8; 512];
            let cairn = ir::demo_cairn(&mut buf2);
            kprintln!("  prog 2 (write to Cairn, then read it back) with WRITE+READ+PRINT:");
            kprintln!("  (durable: lands in Cairn v1 ns=agent via the user-space storage daemon)");
            let mut h = KHost {
                caps: ir::CAP_WRITE | ir::CAP_READ | ir::CAP_PRINT,
                cairn: cairn_ns_id("agent").map(|ns| (plan, ns)),
                intent: 0,
                derived: 0,
            };
            if let Err(t) = ir::run(cairn, &mut h) {
                kprintln!("  [ir] TRAP: {}", t.msg());
            }
        }
        "frames" => {
            let free0 = unsafe { *FRAME_FREE.get() };
            kprintln!("frames: {} total, {} free", unsafe { *FRAME_TOTAL.get() }, free0);
            let a = frame_alloc();
            let b = frame_alloc();
            let c = frame_alloc();
            let first = unsafe { *(a as *const u64) };
            kprintln!("  allocated {a:#x} {b:#x} {c:#x}; first word of {a:#x} = {first} (zeroed)");
            kprintln!("  free now: {}", unsafe { *FRAME_FREE.get() });
            frame_free(a);
            frame_free(b);
            frame_free(c);
            kprintln!(
                "  after free: {} (back to {})",
                unsafe { *FRAME_FREE.get() },
                free0
            );
        }
        "services" => {
            let _ = ensure_virtio_block_service(plan);
            print_services();
        }
        "svc-stop" => match arg {
            "virtio-block" => svc_stop_virtio(plan),
            _ => kprintln!("usage: svc-stop virtio-block"),
        },
        "svc-restart" => match arg {
            "virtio-block" => svc_restart_virtio(plan),
            _ => kprintln!("usage: svc-restart virtio-block"),
        },
        "svc-fault-demo" => match arg {
            "virtio-block" => svc_fault_demo_virtio(plan),
            _ => kprintln!("usage: svc-fault-demo virtio-block"),
        },
        "root" => {
            kprintln!("[install] root summary:");
            kprintln!(
                "  manifest root={} block={} marker_sector={} metadata_sector={}",
                plan.install_manifest.root_service,
                plan.install_manifest.block_service,
                plan.install_manifest.layout.marker_sector,
                plan.install_manifest.layout.root_metadata_sector
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
            run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
        }
        "install" => install_command(plan, arg),
        "pkg-recv" => pkg::pkg_recv(plan),
        "sig-demo" => pkg::sig_demo(plan),
        "pkg-list" => pkg::pkg_list(plan),
        "pkg-info" => pkg::pkg_info(plan, arg),
        "pkg-run" => pkg::pkg_run(plan, arg),
        "intent-open" => pkg::intent_open(arg),
        "intent-revoke" => pkg::intent_revoke(arg),
        "lease-demo" => pkg::lease_demo(),
        "cap-demo" => run_cap_demo(),
        "smp-demo" => run_smp_demo(),
        "smp-task" => run_smp_task_demo(),
        "smp-preempt" => run_smp_preempt_demo(),
        "smp-sched" => run_smp_sched_demo(),
        "smp-isolate" => run_smp_isolate_demo(),
        "ns-revoke" => ns_revoke(plan, arg),
        "ns-grant" => ns_grant(plan, arg),
        "nsrevoke-demo" => run_nsrevoke_demo(plan),
        "agentrevoke-demo" => run_agentrevoke_demo(plan),
        "irq-stat" => irq_stat(),
        "net-probe" => net_probe(),
        "marz-send" => run_marz_send(plan, arg),
        "marz-effect" => marz_effect(plan, arg, 0),
        "marz-ping" => run_marz_ping(arg),
        "dev-revoke" => dev_authority_set(arg, false),
        "dev-grant" => dev_authority_set(arg, true),
        "dev-demo" => run_dev_demo(plan),
        "marz-grant" => marz_dest_authority(arg, true),
        "marz-revoke" => marz_dest_authority(arg, false),
        "marz-demo" => run_marz_demo(plan),
        "marz-effect-demo" => run_marz_effect_demo(plan),
        "exfil-demo" => run_exfil_demo(),
        "taintflow-demo" => run_taintflow_demo(plan),
        "taint" => taint_show(),
        "declassify" => declassify(),
        "endorse" => endorse(),
        "ingress-demo" => run_ingress_demo(plan),
        "intent-list" => pkg::intent_list(),
        "intent-run" => pkg::intent_run(plan, arg),
        "intent-demo" => pkg::intent_demo(plan),
        "pkg-remove" => pkg::pkg_remove(plan, arg),
        "pkg-store" => pkg::pkg_store(plan),
        "pkg-journal" => pkg::pkg_journal(plan),
        "pkg-recover" => pkg::pkg_recover(plan),
        "pkg-verify" => pkg::pkg_verify(plan, arg),
        "pkg-fault" => pkg::pkg_fault(plan, arg),
        "pkg-gc" => pkg::pkg_gc(plan, arg),
        "pkg-update" => pkg::pkg_update(plan, arg),
        "pkg-rollback" => pkg::pkg_rollback(plan, arg),
        "pkg-versions" => pkg::pkg_versions(plan, arg),
        "pkg-review" => pkg::pkg_review(plan, arg),
        "pkg-pin" => pkg::pkg_pin(plan, arg, true),
        "pkg-unpin" => pkg::pkg_pin(plan, arg, false),
        "pkg-retire" => pkg::pkg_retire(plan, arg),
        "pkg-lifecycle" => pkg::pkg_lifecycle(plan),
        "pkg-audit" => pkg::pkg_audit(plan, arg),
        "apps" => print_apps(plan, arg),
        "app-info" => app_info(plan, arg),
        "app-install" => app_install(plan, arg),
        "app-run" => app_run(plan, arg),
        "app-remove" => app_remove(plan, arg),
        "app-deny" => app_deny(plan, arg),
        "app-permissions" => app_permissions(arg),
        "note-set" => run_registered_virtio_client(plan, BLK_REQ_NOTE_SET, arg),
        "note-get" => run_registered_virtio_client(plan, BLK_REQ_NOTE_GET, ""),
        "lab-set" => run_registered_virtio_client(plan, BLK_REQ_LAB_SET, arg),
        "lab-get" => run_registered_virtio_client(plan, BLK_REQ_LAB_GET, ""),
        "calc" => calc_command(plan, arg),
        "calc-history" => run_registered_virtio_client(plan, BLK_REQ_CALC_GET, ""),
        "vault-put" => vault_put(plan, arg),
        "vault-get" => run_registered_virtio_client(plan, BLK_REQ_VAULT_GET, ""),
        "install-check" => {
            kprintln!("[install] validating boot/install manifest v0");
            kprintln!(
                "[install] target={:?} root={} block={} marker_sector={}",
                plan.install_manifest.target,
                plan.install_manifest.root_service,
                plan.install_manifest.block_service,
                plan.install_manifest.layout.marker_sector
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
        }
        "install-init" => {
            kprintln!(
                "[install] initializing Dezh root marker and metadata via user-space block service"
            );
            run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
        }
        "root-status" => {
            kprintln!("[install] reading Dezh root metadata via registered block service");
            run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
        }
        "events" => print_events(),
        "audit" => print_audit(),
        "uptime" => {
            let t = TICKS.load(Ordering::Relaxed);
            kprintln!("uptime: {} ticks (~{}.{} s)", t, t / TIMER_HZ, t % TIMER_HZ);
        }
        "echo" => kprintln!("{arg}"),
        "run" => {
            kprintln!("[kernel] spawning U-mode task; granted capability: PRINT (not TIME)");
            run_tasks(&[(user_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            kprintln!("[kernel] task returned; back in the S-mode console");
        }
        "load" => {
            kprintln!(
                "[kernel] loading a separate program into its own address space (cap: PRINT)"
            );
            run_processes(&[ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 0).uart()]);
            kprintln!("[kernel] program exited; back in the console");
        }
        "procs" => {
            kprintln!("[kernel] loading TWO copies as separate processes (own address spaces)");
            run_processes(&[
                ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 1),
                ProcessSpec::new(USERPROG_ELF, TASK_PRINT, 2),
            ]);
            kprintln!("[kernel] all processes exited; back in the console");
        }
        "rogue" => {
            kprintln!(
                "[kernel] spawning a rogue U-mode task (it will try to touch the UART directly)"
            );
            run_tasks(&[(rogue_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            kprintln!("[kernel] rogue task handled; console survived");
        }
        "multi" => {
            kprintln!("[kernel] spawning 3 cooperative U-mode tasks (round-robin via yield)");
            run_tasks(&[
                (worker_a as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (worker_b as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (worker_c as *const () as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] all tasks done; back in the console");
        }
        "linux" => {
            kprintln!("[kernel] running a Linux-ABI app through the Pol personality layer");
            run_tasks(&[(linux_app as *const () as usize, TASK_PRINT, PERS_LINUX)]);
            kprintln!("[kernel] Linux app done; back in the console");
        }
        "linux-elf" => {
            kprintln!(
                "[kernel] loading a REAL unmodified static Linux/RISC-V ELF ({} bytes,",
                LINUX_GUEST_ELF.len()
            );
            kprintln!("         target=riscv64gc-unknown-linux-musl) into its own address space.");
            kprintln!("[kernel] --- WITH the print capability: Pol services its syscalls ---");
            record_event("kernel", "pol.elf.run", "process", "start");
            run_foreground_processes(&[ProcessSpec::new(LINUX_GUEST_ELF, TASK_PRINT, 0).linux()]);
            kprintln!("[kernel] --- WITHOUT the print capability: kernel DENIES write ---");
            run_foreground_processes(&[ProcessSpec::new(LINUX_GUEST_ELF, 0, 0).linux()]);
            record_event("kernel", "pol.elf.run", "process", "OK");
            kprintln!("[kernel] the same ELF also runs on real riscv64 Linux; back in the console");
        }
        "bench" => {
            kprintln!("[kernel] running ecall round-trip microbenchmark (500000 calls)...");
            run_tasks(&[(bench_task as *const () as usize, 0, PERS_NATIVE)]);
            kprintln!("[kernel] benchmark done");
        }
        "bench-pol" => {
            // Same zero-work syscall via two paths: native SYS_PRINT vs the Linux
            // write(2) ABI routed through Pol. The kernel times both; the delta is
            // the per-syscall translation overhead (F4, D015). QEMU-emulated.
            kprintln!(
                "[kernel] Pol translation microbenchmark ({} calls each): native SYS_PRINT vs Linux write(2)...",
                BENCH_POL_ITERS
            );
            let n = BENCH_POL_ITERS as u64;
            let t0 = rdtime();
            run_tasks(&[(bench_native_print_task as *const () as usize, TASK_PRINT, PERS_NATIVE)]);
            let t1 = rdtime();
            run_tasks(&[(bench_pol_write_task as *const () as usize, TASK_PRINT, PERS_LINUX)]);
            let t2 = rdtime();
            let native_ns = t1.wrapping_sub(t0).saturating_mul(100) / n;
            let pol_ns = t2.wrapping_sub(t1).saturating_mul(100) / n;
            let overhead = pol_ns.saturating_sub(native_ns);
            kprintln!("  [bench-pol] native SYS_PRINT round-trip:   ~{native_ns} ns/call (QEMU-emulated)");
            kprintln!("  [bench-pol] Pol Linux write(2) round-trip: ~{pol_ns} ns/call (QEMU-emulated)");
            kprintln!(
                "  [bench-pol] Pol translation overhead: ~{overhead} ns/call (delta over native, emulated)"
            );
            kprintln!("[kernel] benchmark done");
        }
        "bench-os" => run_bench_os(),
        "bench-ipc" => run_bench_ipc(),
        "bench-storage" => run_bench_storage(plan),
        "bench-caps" => run_bench_caps(),
        "bench-all" => run_bench_all(plan),
        "stress-lab" => stress_lab(plan, arg),
        "preempt" => {
            kprintln!("[kernel] two CPU-bound tasks that never yield (watch them interleave)");
            run_tasks(&[
                (preempt_a as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (preempt_b as *const () as usize, TASK_PRINT, PERS_NATIVE),
            ]);
            kprintln!("[kernel] preemption demo done");
        }
        "spy" => {
            kprintln!(
                "[kernel] isolation: task0 owns a private stack; task1 (spy) tries to read it"
            );
            kprintln!("[kernel] (task0 stack region base = {:#x})", stack_base());
            run_tasks(&[
                (victim_task as *const () as usize, TASK_PRINT, PERS_NATIVE),
                (spy_task as *const () as usize, 0, PERS_NATIVE),
            ]);
            kprintln!("[kernel] isolation demo done");
        }
        "ipc" => {
            kprintln!("[kernel] IPC: a no-authority service + an agent that delegates PRINT to it");
            // task 0 = service (no caps), task 1 = agent (holds PRINT)
            run_tasks(&[
                (service_task as *const () as usize, TASK_IPC, PERS_NATIVE),
                (agent_task as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] IPC demo done; back in the console");
        }
        "ipcq" => {
            if virtio_service_is_running() {
                kprintln!(
                    "[kernel] IPC queue demo skipped to keep running services alive; use it before starting services"
                );
                return;
            }
            kprintln!("[kernel] IPC queue: two clients enqueue while the service is busy");
            run_tasks(&[
                (
                    queue_service_task as *const () as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] IPC queue demo done; back in the console");
        }
        "queues" => {
            if virtio_service_is_running() {
                kprintln!(
                    "[kernel] queues demo skipped to keep running services alive; use it before starting services"
                );
                return;
            }
            kprintln!("[kernel] queues: bounded FIFO IPC mailbox demo");
            run_tasks(&[
                (
                    queue_service_task as *const () as usize,
                    TASK_PRINT | TASK_IPC,
                    PERS_NATIVE,
                ),
                (queue_agent_a as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
                (queue_agent_b as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] queue demo done; back in the console");
        }
        "cairn" => {
            kprintln!(
                "[kernel] Cairn store service + an agent doing a rollbackable action over IPC"
            );
            // task 0 = cairn store service, task 1 = agent (holds PRINT)
            run_tasks(&[
                (cairn_service as *const () as usize, TASK_IPC, PERS_NATIVE),
                (agent_cairn as *const () as usize, TASK_PRINT | TASK_IPC, PERS_NATIVE),
            ]);
            kprintln!("[kernel] Cairn demo done; back in the console");
        }
        "deny" => {
            kprintln!("[safety] denial tour: no ambient authority across caps, MMIO, and Pol");
            kprintln!("denied: 'secret' requires capability SECRET (not held)");
            run_virtio_no_grant_probe();
            kprintln!("[safety] no-grant MMIO fault returned; console survived");
            if virtio_service_is_running() {
                kprintln!(
                    "[safety] Pol denial demo skipped here to keep running services alive; use `linux` before starting services"
                );
            } else {
                run_tasks(&[(linux_app as *const () as usize, TASK_PRINT, PERS_LINUX)]);
                kprintln!("[safety] unsupported Linux syscall returned ENOSYS; console survived");
            }
        }
        "halt" => {
            kprintln!("halting.");
            shutdown(FINISH_PASS);
        }
        other => kprintln!("'{other}' has no handler"),
    }
}

pub(crate) fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let c = Uart.getc();
        match c {
            b'\n' => {
                if SKIP_LF_AFTER_CR.swap(false, Ordering::Relaxed) {
                    continue;
                }
                kprintln!();
                return len;
            }
            b'\r' => {
                SKIP_LF_AFTER_CR.store(true, Ordering::Relaxed);
                kprintln!();
                return len;
            }
            0x7f | 0x08 => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
                if len > 0 {
                    len -= 1;
                    kprint!("\x08 \x08");
                }
            }
            c if (c == b' ' || c.is_ascii_graphic()) && len < buf.len() => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
                buf[len] = c;
                len += 1;
                Uart.putc(c);
            }
            _ => {
                SKIP_LF_AFTER_CR.store(false, Ordering::Relaxed);
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn kmain(hart_id: usize, _fdt: usize) -> ! {
    Uart.init();
    // SBI hands the boot hart's id in a0. Capture it before anything else needs a0.
    let boot_hart = hart_id;

    let memory = vec![
        MemoryRegion::new(0x8000_0000, 0x20_0000, MemoryKind::Reserved),
        MemoryRegion::new(0x8020_0000, 0x7E0_0000, MemoryKind::Usable),
        MemoryRegion::new(0x1000_0000, 0x1000, MemoryKind::Mmio),
    ];
    let info = BootInfo::qemu_minimal_riscv(memory.clone());

    let plan = match plan_boot(&info) {
        Ok(plan) => plan,
        Err(e) => {
            kprintln!("[dezh-boot] BOOT CONTRACT VIOLATION: {e:?}");
            shutdown(FINISH_FAIL);
        }
    };

    // Dezh banner (ASCII so it renders on any serial console). The info line is
    // filled from the validated boot plan.
    kprintln!();
    kprintln!(r"   ____            _");
    kprintln!(r"  |  _ \  ___  ___| |__");
    kprintln!(r"  | | | |/ _ \/_  / '_ \");
    kprintln!(r"  | |_| |  __/ / /| | | |");
    kprintln!(r"  |____/ \___//___|_| |_|");
    kprintln!("  Dezh OS - capability-secure - no ambient authority");
    kprintln!(
        "  v0 - riscv64 - {} MiB usable - {} services",
        plan.usable_bytes / 1024 / 1024,
        plan.services.len()
    );
    kprintln!();
    kprintln!("[dezh-boot] alive on bare metal (qemu virt, riscv64, S-mode)");
    kprintln!("[dezh-boot] boot contract VALIDATED");
    kprintln!("[dezh-boot] banner: {}", boot_banner(&plan));
    kprintln!("[dezh-boot] no ambient authority: capability seeds bound to declared services only");

    kprintln!("[dezh-boot] installing trap vector + supervisor timer...");
    unsafe {
        asm!("csrw stvec, {}", in(reg) trap_entry as *const () as usize);
        sbi_set_timer(rdtime() + TIMER_DELTA);
        asm!("csrs sie, {}", in(reg) STIE);
        asm!("csrs sstatus, {}", in(reg) 1usize << 1); // SIE: global supervisor interrupts
        asm!("csrw scounteren, {}", in(reg) 0x7usize); // let U-mode read cycle/time/instret
    }

    plic_init(boot_hart);
    kprintln!("[dezh-boot] PLIC up: virtio device interrupts routed to boot hart {boot_hart} S-mode (no longer polled-only)");

    smp_bringup(boot_hart);
    smp_report_boot();

    kprintln!("[dezh-boot] enabling Sv39 paging (U-mode confined to its own region)...");
    build_page_tables();
    enable_paging();
    frames_init();
    {
        let (total, free) = unsafe { ((*FRAME_TOTAL.get()), (*FRAME_FREE.get())) };
        kprintln!(
            "[dezh-boot] frame allocator: {} x 4 KiB frames ({} MiB free)",
            total,
            (free * FRAME_SIZE) / (1024 * 1024)
        );
    }
    kprintln!(
        "[dezh-boot] embedded user ELFs: userprog={} bytes, virtio-blk={} bytes, dezh-bench={} bytes, dezh-note={} bytes, dezh-lab={} bytes, dezh-calc={} bytes, dezh-vault={} bytes",
        USERPROG_ELF.len(),
        VIRTIO_BLK_ELF.len(),
        BENCH_ELF.len(),
        NOTE_ELF.len(),
        LAB_ELF.len(),
        CALC_ELF.len(),
        VAULT_ELF.len()
    );
    kprintln!(
        "[dezh-boot] install manifest v0: root={} block={} marker_sector={}",
        plan.install_manifest.root_service,
        plan.install_manifest.block_service,
        plan.install_manifest.layout.marker_sector
    );
    build_service_registry(&plan);

    let held = cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT | cap::SPAWN;
    console(&plan, &memory, held);
}

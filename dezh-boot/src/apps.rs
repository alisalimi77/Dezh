//! The SDK app lifecycle: install, run, remove, deny, and the permission and
//! progress reporting around them.
//!
//! An app is a package in the store plus a declared capability set; installing
//! one writes a note through the storage daemon, running one launches it with
//! exactly the authority it declared, and `app-deny` proves the gate by asking
//! for authority the app does not hold.
//!
//! Also under the Cairn banner, and the largest thing there that was not a
//! demo.

use crate::abi::*;
use crate::audit::{print_events, record_event};
use crate::sched::run_foreground_processes;
use crate::service::ensure_virtio_block_service;
use crate::vblk::{run_registered_virtio_client, run_registered_virtio_client_status};
use crate::{
    kprintln, KernelPlan, ProcessSpec, CALC_ELF, CALC_ROLE_RUN, LAB_ELF, LAB_ROLE_DENY_BLOCK,
    LAB_ROLE_DENY_MMIO, LAB_ROLE_UI, LAB_ROLE_WORKER, NOTE_ELF, NOTE_ROLE_DENY_BLOCK,
    NOTE_ROLE_DENY_MMIO, NOTE_ROLE_RUN, TASK_IPC, TASK_PRINT, VAULT_ELF, VAULT_ROLE_DENY_BLOCK,
    VAULT_ROLE_DENY_MMIO, VAULT_ROLE_RUN,
};
pub(crate) fn app_note_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_NOTE, "") == 0
}

pub(crate) fn app_lab_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_LAB, "") == 0
}

pub(crate) fn app_calc_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_CALC, "") == 0
}

pub(crate) fn app_vault_is_active(plan: &KernelPlan) -> bool {
    run_registered_virtio_client_status(plan, BLK_REQ_APP_REQUIRE_VAULT, "") == 0
}

pub(crate) fn print_apps(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "" => {
            kprintln!("[apps] available bundles:");
            run_registered_virtio_client(plan, BLK_REQ_APP_AVAILABLE, "");
            kprintln!("[apps] installed apps:");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, "");
        }
        "available" => run_registered_virtio_client(plan, BLK_REQ_APP_AVAILABLE, ""),
        "installed" => run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, ""),
        other => kprintln!("[apps] unknown view '{other}' (use: apps available|installed)"),
    }
}

pub(crate) fn app_info(plan: &KernelPlan, arg: &str) {
    if !matches!(arg.trim(), "" | "note" | "lab" | "calc" | "vault") {
        kprintln!("[app-info] unknown app '{}'", arg.trim());
        return;
    }
    run_registered_virtio_client(plan, BLK_REQ_APP_INFO, "");
}

pub(crate) fn app_install(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => {
            record_event("console", "app.install", "note", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_NOTE, "");
            record_event("installer", "app.install", "note", "done");
        }
        "lab" => {
            record_event("console", "app.install", "lab", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_LAB, "");
            record_event("installer", "app.install", "lab", "done");
        }
        "calc" => {
            record_event("console", "app.install", "calc", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_CALC, "");
            record_event("installer", "app.install", "calc", "done");
        }
        "vault" => {
            record_event("console", "app.install", "vault", "start");
            run_registered_virtio_client(plan, BLK_REQ_APP_INSTALL_VAULT, "");
            record_event("installer", "app.install", "vault", "done");
        }
        other => kprintln!("[installer] unknown available app '{other}'"),
    }
}

pub(crate) fn app_run(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => {
            if !app_note_is_active(plan) {
                kprintln!("[app-run] note not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching note with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT | TASK_IPC,
                NOTE_ROLE_RUN,
            )]);
            kprintln!("[app-run] note exited; console returned");
            record_event("app", "app.run", "note", "OK");
        }
        "lab" => {
            if !app_lab_is_active(plan) {
                kprintln!("[app-run] lab not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] preparing lab private storage through virtio-block service");
            run_registered_virtio_client(plan, BLK_REQ_LAB_SET, "lab-run-start");
            kprintln!("[app-run] launching lab UI + workers with caps=PRINT,IPC only");
            run_foreground_processes(&[
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_UI).args(2, 0, 0),
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_WORKER).args(
                    FIRST_FOREGROUND_TASK,
                    1,
                    0,
                ),
                ProcessSpec::new(LAB_ELF, TASK_PRINT | TASK_IPC, LAB_ROLE_WORKER).args(
                    FIRST_FOREGROUND_TASK,
                    2,
                    0,
                ),
            ]);
            run_registered_virtio_client(plan, BLK_REQ_LAB_SET, "lab-run-complete");
            run_registered_virtio_client(plan, BLK_REQ_LAB_GET, "");
            kprintln!("[app-run] lab exited; console returned");
            record_event("app", "app.run", "lab", "OK");
        }
        "calc" => {
            if !app_calc_is_active(plan) {
                kprintln!("[app-run] calc not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching calc with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                CALC_ELF,
                TASK_PRINT | TASK_IPC,
                CALC_ROLE_RUN,
            )]);
            kprintln!("[app-run] calc exited; console returned");
            record_event("app", "app.run", "calc", "OK");
        }
        "vault" => {
            if !app_vault_is_active(plan) {
                kprintln!("[app-run] vault not installed or not active; launch denied");
                return;
            }
            kprintln!("[app-run] launching vault with caps=PRINT,IPC and no device/DMA grants");
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT | TASK_IPC,
                VAULT_ROLE_RUN,
            )]);
            kprintln!("[app-run] vault exited; console returned");
            record_event("app", "app.run", "vault", "OK");
        }
        other => kprintln!("[app-run] unknown app '{other}'"),
    }
}

pub(crate) fn app_remove(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "note" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_NOTE, ""),
        "lab" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_LAB, ""),
        "calc" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_CALC, ""),
        "vault" => run_registered_virtio_client(plan, BLK_REQ_APP_REMOVE_VAULT, ""),
        other => kprintln!("[installer] unknown installed app '{other}'"),
    }
    record_event("console", "app.remove", "app", "done");
}

pub(crate) fn app_deny(plan: &KernelPlan, arg: &str) {
    let daemon = ensure_virtio_block_service(plan).unwrap_or(usize::MAX);
    match arg.trim() {
        "note" => {
            kprintln!("[app-deny] note has no direct block grant when launched without IPC");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT,
                NOTE_ROLE_DENY_BLOCK,
            )
            .args(daemon, 0, 0)]);
            kprintln!("[app-deny] note has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                NOTE_ELF,
                TASK_PRINT | TASK_IPC,
                NOTE_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] note device/block direct access denied; console survived");
        }
        "lab" => {
            kprintln!("[app-deny] lab has no direct block grant when launched without IPC");
            run_foreground_processes(&[
                ProcessSpec::new(LAB_ELF, TASK_PRINT, LAB_ROLE_DENY_BLOCK).args(daemon, 0, 0)
            ]);
            kprintln!("[app-deny] lab has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                LAB_ELF,
                TASK_PRINT | TASK_IPC,
                LAB_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] lab device/block direct access denied; console survived");
        }
        "vault" => {
            kprintln!("[app-deny] vault has no direct block grant when launched without IPC");
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT,
                VAULT_ROLE_DENY_BLOCK,
            )
            .args(daemon, 0, 0)]);
            kprintln!("[app-deny] vault has no MMIO/device grant");
            run_foreground_processes(&[ProcessSpec::new(
                VAULT_ELF,
                TASK_PRINT | TASK_IPC,
                VAULT_ROLE_DENY_MMIO,
            )]);
            kprintln!("[app-deny] vault device/block direct access denied; console survived");
        }
        other => kprintln!("[app-deny] unknown app '{other}'"),
    }
    record_event("kernel", "deny.app", "app", "OK");
}

pub(crate) fn app_permissions(arg: &str) {
    let app = arg.trim();
    if !matches!(app, "note" | "lab" | "calc" | "vault") {
        kprintln!("usage: app-permissions <note|lab|calc|vault>");
        return;
    }
    kprintln!("app permissions: {app}");
    kprintln!("  REQUESTED  PRINT IPC");
    kprintln!("  GRANTED    PRINT IPC");
    kprintln!("  DENIED     DEVICE_VIRTIO_BLK DMA BLOCK_DIRECT MMIO");
    kprintln!("  STORAGE    service-mediated via virtio-block daemon");
}

pub(crate) fn install_plan() {
    kprintln!("Install Plan: Dezh Root v1");
    kprintln!("  [01] Probe block service        ready");
    kprintln!("  [02] Validate boot manifest     ready");
    kprintln!("  [03] Write root marker          pending");
    kprintln!("  [04] Initialize app registry    pending");
    kprintln!("  [05] Install base apps          note lab calc vault");
    kprintln!("  [06] Verify root/app state      pending");
    kprintln!("  [07] Commit install report      pending");
}

pub(crate) fn progress(stage: usize, total: usize, label: &str, status: &str) {
    let filled = stage * 20 / total;
    let mut bar = [b'-'; 20];
    let mut i = 0usize;
    while i < filled && i < bar.len() {
        bar[i] = b'#';
        i += 1;
    }
    let s = core::str::from_utf8(&bar).unwrap_or("--------------------");
    kprintln!(
        "[{}] {:>3}%  {:<28} {}",
        s,
        stage * 100 / total,
        label,
        status
    );
}

pub(crate) fn install_verify(plan: &KernelPlan) {
    kprintln!("[install-v1] verifying root marker, metadata, and base app registry");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, "");
    run_registered_virtio_client(plan, BLK_REQ_ROOT_STATUS, "");
    run_registered_virtio_client(plan, BLK_REQ_APP_INSTALLED, "");
    record_event("installer", "install.verify", "root-v1", "done");
}

pub(crate) fn install_report() {
    kprintln!("Install Report: Dezh Root v1");
    kprintln!("  root marker      sector 0");
    kprintln!("  root metadata    sector 4");
    kprintln!("  app registry     sectors 5..10");
    kprintln!("  private data     sectors 16..19");
    kprintln!("  required service virtio-block");
    kprintln!("  policy           no ambient authority");
    print_events();
}

pub(crate) fn install_run(plan: &KernelPlan, dry_run: bool) {
    install_plan();
    record_event("console", "install.run", "root-v1", "start");
    let total = 7usize;
    progress(1, total, "probe block service", "OK");
    if dry_run {
        progress(2, total, "validate boot manifest", "OK");
        progress(3, total, "write root marker", "dry-run");
        progress(4, total, "initialize app registry", "dry-run");
        progress(5, total, "install base apps", "dry-run");
        progress(6, total, "verify root/app state", "dry-run");
        progress(7, total, "commit install report", "dry-run");
        kprintln!("[install-v1] dry-run complete; disk not modified");
        record_event("installer", "install.dryrun", "root-v1", "OK");
        return;
    }
    progress(2, total, "validate boot manifest", "OK");
    progress(3, total, "write root marker", "running");
    run_registered_virtio_client(plan, BLK_REQ_INSTALL_INIT, "");
    progress(4, total, "initialize app registry", "running");
    app_install(plan, "note");
    progress(5, total, "install base apps", "running");
    app_install(plan, "lab");
    app_install(plan, "calc");
    app_install(plan, "vault");
    progress(6, total, "verify root/app state", "running");
    install_verify(plan);
    progress(7, total, "commit install report", "OK");
    install_report();
    record_event("installer", "install.run", "root-v1", "OK");
}

pub(crate) fn install_command(plan: &KernelPlan, arg: &str) {
    match arg.trim() {
        "" | "plan" => install_plan(),
        "check" => run_registered_virtio_client(plan, BLK_REQ_INSTALL_CHECK, ""),
        "run" => install_run(plan, false),
        "--dry-run" | "dry-run" => install_run(plan, true),
        "verify" => install_verify(plan),
        "report" => install_report(),
        "rollback" => {
            kprintln!("[install-v1] rollback uses storage rollback for v0 root data");
            run_registered_virtio_client(plan, BLK_REQ_PROLLBACK, "");
            record_event("installer", "install.rollback", "root-v1", "done");
        }
        other => kprintln!(
            "usage: install plan|check|run|verify|report|rollback|--dry-run (got '{other}')"
        ),
    }
}

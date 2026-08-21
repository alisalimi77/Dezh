//! The small console commands that are not part of another subsystem.
//!
//! The calculator, `explain`, `stress`, and the vault put helper, plus the two
//! token parsers they share. These were the last things filed under the "Cairn
//! v1 console front-end" banner, and they are console verbs rather than Cairn.

use alloc::format;

use crate::abi::*;
use crate::apps::{app_calc_is_active, app_install, app_run, app_vault_is_active};
use crate::audit::record_event;
use crate::mm::frames::FRAME_FREE;
use crate::proc::loader::ProcessSpec;
use crate::sched::foreground_exit_code;
use crate::sched::run_foreground_processes;
use crate::vblk::run_registered_virtio_client;
use crate::console::print_memstat;
use crate::{kprintln, KernelPlan, CALC_ELF, TASK_IPC, TASK_PRINT};

pub(crate) fn parse_usize_token(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
        i += 1;
    }
    Some(n)
}

pub(crate) fn calc_op_token(s: &str) -> Option<usize> {
    match s {
        "+" => Some(CALC_OP_ADD),
        "-" => Some(CALC_OP_SUB),
        "*" | "x" | "X" => Some(CALC_OP_MUL),
        "/" => Some(CALC_OP_DIV),
        _ => None,
    }
}

pub(crate) fn calc_eval(op: usize, a: usize, b: usize) -> Option<usize> {
    match op {
        CALC_OP_ADD => Some(a.saturating_add(b)),
        CALC_OP_SUB => Some(a.saturating_sub(b)),
        CALC_OP_MUL => Some(a.saturating_mul(b)),
        // checked_div is the divide-by-zero guard, not an optimisation: it
        // returns None for b == 0, which is exactly this arm's contract.
        CALC_OP_DIV => a.checked_div(b),
        _ => None,
    }
}

pub(crate) fn calc_command(plan: &KernelPlan, arg: &str) {
    if !app_calc_is_active(plan) {
        kprintln!("[calc] calc not installed; run `app-install calc` or `install run`");
        return;
    }
    let mut parts = arg.split_whitespace();
    let Some(a_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let Some(op_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let Some(b_s) = parts.next() else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    let (Some(a), Some(op), Some(b)) = (
        parse_usize_token(a_s),
        calc_op_token(op_s),
        parse_usize_token(b_s),
    ) else {
        kprintln!("usage: calc <n> <+|-|*|/> <n>");
        return;
    };
    run_foreground_processes(&[
        ProcessSpec::new(CALC_ELF, TASK_PRINT | TASK_IPC, CALC_ROLE_EVAL).args(op, a, b),
    ]);
    // A let-chain, which edition 2024 is what makes available here. Both halves
    // are the same condition - the app exited cleanly *and* the expression is
    // one we can restate - and nesting them said that less directly.
    if foreground_exit_code() == 0
        && let Some(result) = calc_eval(op, a, b)
    {
        let expr = format!("{} {} {} = {}", a_s, op_s, b_s, result);
        run_registered_virtio_client(plan, BLK_REQ_CALC_SET, &expr);
        record_event("app", "calc.eval", "calc", "OK");
    }
}

pub(crate) fn vault_put(plan: &KernelPlan, arg: &str) {
    if !app_vault_is_active(plan) {
        kprintln!("[vault] vault not installed; run `app-install vault` or `install run`");
        return;
    }
    run_registered_virtio_client(plan, BLK_REQ_VAULT_SET, arg);
    record_event("app", "vault.put", "vault", "OK");
}

pub(crate) fn explain_command(arg: &str) {
    match arg.trim() {
        "app-run lab" | "app-run" => {
            kprintln!("explain app-run lab:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: app registry -> foreground U-mode app -> IPC workers -> virtio-block storage");
            kprintln!("  denied direct: MMIO DMA BLOCK_DIRECT");
        }
        "install" | "install run" => {
            kprintln!("explain install run:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: boot manifest -> virtio-block service -> disk marker/app registry -> verify");
            kprintln!("  rollback point: v0 current/previous sectors and registry checkpoints");
        }
        "calc" => {
            kprintln!("explain calc:");
            kprintln!("  requires: SPAWN");
            kprintln!("  path: installed calc ELF computes in U-mode, last result stored via app registry");
            kprintln!("  denied direct: DEVICE DMA BLOCK_DIRECT");
        }
        "vault" | "vault-put" => {
            kprintln!("explain vault:");
            kprintln!("  requires: SPAWN for put, INSPECT for get");
            kprintln!("  path: console -> virtio-block typed IPC -> vault private sector");
            kprintln!("  denied direct: MMIO DMA BLOCK_DIRECT");
        }
        other => kprintln!("explain: no detailed path for '{other}' yet"),
    }
}

pub(crate) fn parse_small_count(arg: &str, default: usize) -> usize {
    let bytes = arg.trim().as_bytes();
    if bytes.is_empty() {
        return default;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if !b.is_ascii_digit() {
            return default;
        }
        n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
        i += 1;
    }
    n.clamp(1, 8)
}

pub(crate) fn stress_lab(plan: &KernelPlan, arg: &str) {
    let count = parse_small_count(arg, 3);
    kprintln!("[stress-lab] ensuring lab app is installed");
    app_install(plan, "lab");
    print_memstat();
    let free_before = unsafe { *FRAME_FREE.get() };
    let mut i = 0usize;
    while i < count {
        kprintln!("[stress-lab] iteration {}/{}", i + 1, count);
        app_run(plan, "lab");
        i += 1;
    }
    let free_after = unsafe { *FRAME_FREE.get() };
    print_memstat();
    if free_before == free_after {
        kprintln!("[stress-lab] PASS: free frames stable at {}", free_after);
    } else {
        kprintln!(
            "[stress-lab] WARN: free frames changed before={} after={}",
            free_before,
            free_after
        );
    }
}

// Worker tasks (run in U-mode, so they live in the user region). Each prints a
// couple of steps and yields between them, so their output interleaves.

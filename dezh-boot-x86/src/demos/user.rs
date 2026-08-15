//! A task the kernel does not trust, running at CPL3.
//!
//! Everything scheduled so far ran at ring 0, where "isolated" meant nothing: a
//! CPL0 task can reload `cr3`, rewrite the page tables it is confined by, or
//! call any kernel function directly. This task cannot. It runs at CPL3 in its
//! own address space, on pages that are the only ones marked USER anywhere in
//! it, and the single door back into the kernel is one IDT gate.
//!
//! The program is machine code copied into a page of its own rather than a Rust
//! function called by address, because a Rust function lives in the kernel's
//! `.text` — which is mapped in the task's address space (it must be; interrupts
//! run there) but carries no USER bit at any level, so ring 3 cannot execute it.
//! That is the isolation working, not an inconvenience around it.

use crate::arch::paging;
use crate::arch::timer;
use crate::console::{print, print_hex, print_i64};
use crate::sched;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

// The whole user program. Position-independent by construction: every jump is
// relative and every operand is an immediate, so it runs correctly at whatever
// address it is mapped at.
//
// It asks the kernel for something five times, checking each answer, and then
// asks to exit — reporting through the exit argument whether every answer came
// back as expected. `int 0x80` preserves rbx because the interrupt stub saves
// and restores all fifteen registers; only rax is written back.
global_asm!(
    r#"
.code64
.section .text
.balign 16
.global user_blob_start
user_blob_start:
    xor rbx, rbx
1:
    mov rax, 1              /* SYS_NOTE */
    mov rdi, rbx            /* argument: which note this is */
    int 0x80
    lea rcx, [rbx + rbx]    /* the kernel promised to answer with 2 * arg */
    cmp rax, rcx
    jne 3f
    inc rbx
    cmp rbx, 5
    jb 1b
    mov rax, 2              /* SYS_EXIT */
    xor rdi, rdi            /* 0: every answer matched */
    int 0x80
3:
    mov rax, 2              /* SYS_EXIT */
    mov rdi, 1              /* 1: an answer did not match */
    int 0x80
2:
    jmp 2b
.global user_blob_end
user_blob_end:
"#
);

extern "C" {
    static user_blob_start: u8;
    static user_blob_end: u8;
}

const SYS_NOTE: u64 = 1;
const SYS_EXIT: u64 = 2;

/// Above everything the kernel uses, and in a different PML4 slot from both the
/// identity map and the private pages of the ring-0 tasks, so nothing the task
/// can name overlaps anything it was not given.
const USER_CODE_VA: u64 = 2 << 39;
const USER_STACK_VA: u64 = USER_CODE_VA + 0x2000;
const USER_STACK_TOP: u64 = USER_STACK_VA + paging::PAGE_SIZE as u64;

/// The task index used here. Tasks 1..3 belong to the scheduler demo and are
/// finished by the time this runs.
const USER_TASK: usize = 4;

static NOTES: AtomicU64 = AtomicU64::new(0);
static LAST_ARG: AtomicU64 = AtomicU64::new(u64::MAX);
static EXIT_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);
static EXITED: AtomicU64 = AtomicU64::new(0);

/// Runs in the interrupt handler, on the user task's kernel stack, with
/// interrupts off. Everything it is told comes from registers the task set, so
/// nothing here may be trusted beyond being a number.
fn syscall(nr: u64, arg: u64) -> u64 {
    match nr {
        SYS_NOTE => {
            NOTES.fetch_add(1, Ordering::Relaxed);
            LAST_ARG.store(arg, Ordering::Relaxed);
            arg * 2
        }
        SYS_EXIT => {
            EXIT_STATUS.store(arg, Ordering::Relaxed);
            EXITED.store(1, Ordering::Relaxed);
            sched::exit_current();
            0
        }
        // An unknown call number is not a fault; it is a refusal with a value
        // the task can see, which is what the RISC-V kernel does for an
        // unsupported syscall.
        _ => u64::MAX,
    }
}

pub(crate) fn run() {
    print("\nUser mode: a task at CPL3, in its own address space, with one way back in.\n");
    if !timer::rearm() {
        print("  [user] no calibrated timer; user task not started.\n");
        return;
    }

    let cr3 = match paging::new_address_space() {
        Some(cr3) => cr3,
        None => {
            print("  [user] out of pages; user task not started.\n");
            return;
        }
    };

    // The program, in a page the task may execute and may not write.
    let blob = core::ptr::addr_of!(user_blob_start) as usize;
    let blob_end = core::ptr::addr_of!(user_blob_end) as usize;
    let len = blob_end - blob;
    let code = match paging::map_new_page(cr3, USER_CODE_VA, paging::USER) {
        Some(p) => p,
        None => {
            print("  [user] out of pages; user task not started.\n");
            return;
        }
    };
    if len > paging::PAGE_SIZE {
        print("  [user] program does not fit one page; user task not started.\n");
        return;
    }
    unsafe { core::ptr::copy_nonoverlapping(blob as *const u8, code, len) };

    // A stack it may write and, being a separate mapping, cannot grow out of:
    // one page below it is unmapped.
    if paging::map_new_page(cr3, USER_STACK_VA, paging::USER | paging::WRITABLE).is_none() {
        print("  [user] out of pages; user task not started.\n");
        return;
    }

    print("  [user] program is ");
    print_i64(len as i64);
    print(" bytes, mapped read-execute at ");
    print_hex(USER_CODE_VA);
    print("\n  [user] stack mapped read-write at ");
    print_hex(USER_STACK_VA);
    print(", nothing else in this address space is USER\n");

    sched::set_syscall_hook(syscall);
    sched::spawn_user(USER_TASK, USER_CODE_VA, USER_STACK_TOP, cr3);
    sched::start();
    timer::sti();

    while EXITED.load(Ordering::Relaxed) == 0 {}

    sched::stop();
    timer::cli();
    timer::mask();

    // The privilege the syscalls arrived from, taken from the caller's saved
    // `cs`. The CPU wrote it on entry; the task cannot forge it.
    let cs = sched::last_syscall_cs();
    let cpl = cs & 3;
    print("  [user] the task called from CPL ");
    print_i64(cpl as i64);
    print(" (cs=");
    print_hex(cs);
    print(")\n");
    print("  [user] ");
    print_i64(NOTES.load(Ordering::Relaxed) as i64);
    print(" syscalls served, last argument ");
    print_i64(LAST_ARG.load(Ordering::Relaxed) as i64);
    print(", exit status ");
    print_i64(EXIT_STATUS.load(Ordering::Relaxed) as i64);
    print("\n");

    if cpl == 3 && NOTES.load(Ordering::Relaxed) == 5 && EXIT_STATUS.load(Ordering::Relaxed) == 0 {
        print("  [user] ring 3 reached and left only through the syscall gate\n");
    } else {
        print("  [user] FAILED: the task did not run at CPL3 or its syscalls did not add up\n");
    }
}

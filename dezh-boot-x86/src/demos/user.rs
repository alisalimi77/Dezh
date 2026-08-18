//! Two tasks the kernel does not trust, and only one of them behaves.
//!
//! Everything scheduled before this ran at ring 0, where "isolated" meant
//! nothing: a CPL0 task can reload `cr3`, rewrite the tables confining it, or
//! call any kernel function directly. These two cannot. They run at CPL3, each
//! in its own address space, on the only pages marked USER anywhere in it, and
//! the single door back into the kernel is one IDT gate.
//!
//! The programs are machine code copied into pages of their own rather than Rust
//! functions called by address, because a Rust function lives in the kernel's
//! `.text` — which *is* mapped in each task's address space (it must be;
//! interrupts run there) but carries no USER bit at any level, so ring 3 cannot
//! execute it. That is the isolation working, not an inconvenience around it.
//!
//! The second task reaches for an address nobody gave it. What that costs is the
//! whole point: the task, and not the machine.

use crate::arch::paging;
use crate::arch::timer;
use crate::console::{print, print_hex, print_i64};
use crate::sched;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

// Both programs are position-independent by construction: every jump is relative
// and every operand an immediate, so they run at whatever address they are
// mapped at. `int 0x80` preserves rbx, because the interrupt stub saves and
// restores all fifteen registers and only rax is written back.
global_asm!(
    r#"
.code64
.section .text
.balign 16

/* The well-behaved one: keep asking the kernel for something until the kernel
   answers "stop", then exit reporting how many times it asked. How long it runs
   is therefore the kernel's decision, not a function of how fast the host is —
   the same reason the scheduler demo counts turns rather than rounds. */
.global user_good_start
user_good_start:
    xor rbx, rbx
1:
    mov rax, 1              /* SYS_NOTE */
    mov rdi, rbx
    int 0x80
    inc rbx
    test rax, rax           /* 0: keep going. 1: stop */
    jz 1b
    mov rax, 2              /* SYS_EXIT */
    mov rdi, rbx            /* how many times it asked */
    int 0x80
2:
    jmp 2b
.global user_good_end
user_good_end:

.balign 16
/* The other one: announce itself once, so there is evidence it really reached
   ring 3, and then read an address nothing mapped for it. */
.global user_bad_start
user_bad_start:
    mov rax, 1              /* SYS_NOTE */
    mov rdi, 99
    int 0x80
    mov rax, [0]            /* not mapped in this address space */
2:
    jmp 2b
.global user_bad_end
user_bad_end:
"#
);

extern "C" {
    static user_good_start: u8;
    static user_good_end: u8;
    static user_bad_start: u8;
    static user_bad_end: u8;
}

const SYS_NOTE: u64 = 1;
const SYS_EXIT: u64 = 2;
/// The kernel keeps the well-behaved task running until it has made at least
/// this many calls *and* its neighbour has faulted — so the interesting part
/// happens while it is still alive, whatever the host's speed.
const MIN_NOTES: u64 = 20;
/// A backstop, in ticks rather than calls: if the neighbour never faults, the
/// good task must still stop rather than spin the run out to a CI timeout. It is
/// a count of ticks because a count of calls is a count of how fast the host is
/// — the first version used 5000 calls and the good task burned through all of
/// them inside its first turn, before the neighbour had run at all.
const BACKSTOP_TICKS: u64 = 200;

/// Above everything the kernel uses, and in a different PML4 slot from both the
/// identity map and the private pages of the ring-0 tasks, so nothing either
/// task can name overlaps anything it was not given.
const CODE_VA: u64 = 2 << 39;
const STACK_VA: u64 = CODE_VA + 0x2000;
const STACK_TOP: u64 = STACK_VA + paging::PAGE_SIZE as u64;

/// Tasks 1..3 belong to the scheduler demo and are finished by the time this
/// runs.
const GOOD_TASK: usize = 4;
const BAD_TASK: usize = 5;

static GOOD_NOTED: AtomicU64 = AtomicU64::new(0);
static BAD_NOTED: AtomicU64 = AtomicU64::new(0);
/// Syscalls the well-behaved task made *after* its neighbour was killed. This is
/// the number that says the machine carried on rather than merely not crashing.
static NOTED_AFTER_KILL: AtomicU64 = AtomicU64::new(0);
static EXIT_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);
static EXITED: AtomicU64 = AtomicU64::new(0);
static START_TICK: AtomicU64 = AtomicU64::new(0);
static BACKSTOPPED: AtomicU64 = AtomicU64::new(0);

/// Runs in the interrupt handler, on the calling task's kernel stack, with
/// interrupts off. Everything it is told came out of registers the task set, so
/// nothing here may be trusted beyond being a number.
fn syscall(nr: u64, arg: u64) -> u64 {
    match nr {
        SYS_NOTE => {
            if sched::current() == BAD_TASK {
                BAD_NOTED.fetch_add(1, Ordering::Relaxed);
                return 0;
            }
            let n = GOOD_NOTED.fetch_add(1, Ordering::Relaxed) + 1;
            let killed = sched::kills() > 0;
            if killed {
                NOTED_AFTER_KILL.fetch_add(1, Ordering::Relaxed);
            }
            let expired =
                timer::ticks() > START_TICK.load(Ordering::Relaxed) + BACKSTOP_TICKS;
            if expired {
                BACKSTOPPED.store(1, Ordering::Relaxed);
            }
            // Answering 1 stops it. Holding it at 0 until its neighbour has
            // faulted is what makes "still running afterwards" a fact rather
            // than a race the fast host happens to win.
            u64::from((n >= MIN_NOTES && killed) || expired)
        }
        SYS_EXIT => {
            EXIT_STATUS.store(arg, Ordering::Relaxed);
            EXITED.store(1, Ordering::Relaxed);
            sched::exit_current();
            0
        }
        // An unknown call number is not a fault; it is a refusal with a value the
        // task can see, which is what the RISC-V kernel does for an unsupported
        // syscall.
        _ => u64::MAX,
    }
}

/// Builds an address space holding one code page and one stack page, and nothing
/// else the task can reach. Returns the cr3.
fn load(blob: usize, len: usize) -> Option<u64> {
    if len > paging::PAGE_SIZE {
        return None;
    }
    let cr3 = paging::new_address_space()?;
    let code = paging::map_new_page(cr3, CODE_VA, paging::USER)?;
    unsafe { core::ptr::copy_nonoverlapping(blob as *const u8, code, len) };
    paging::map_new_page(cr3, STACK_VA, paging::USER | paging::WRITABLE)?;
    Some(cr3)
}

pub(crate) fn run() {
    print("\nUser mode: two tasks at CPL3, and one of them misbehaves.\n");
    if !timer::rearm() {
        print("  [user] no calibrated timer; user tasks not started.\n");
        return;
    }

    let good = core::ptr::addr_of!(user_good_start) as usize;
    let good_len = core::ptr::addr_of!(user_good_end) as usize - good;
    let bad = core::ptr::addr_of!(user_bad_start) as usize;
    let bad_len = core::ptr::addr_of!(user_bad_end) as usize - bad;

    let (good_cr3, bad_cr3) = match (load(good, good_len), load(bad, bad_len)) {
        (Some(g), Some(b)) => (g, b),
        _ => {
            print("  [user] out of pages; user tasks not started.\n");
            return;
        }
    };

    print("  [user] task ");
    print_i64(GOOD_TASK as i64);
    print(" (");
    print_i64(good_len as i64);
    print(" bytes) and task ");
    print_i64(BAD_TASK as i64);
    print(" (");
    print_i64(bad_len as i64);
    print(" bytes), each read-execute at ");
    print_hex(CODE_VA);
    print("\n  [user] each has a stack at ");
    print_hex(STACK_VA);
    print(" and nothing else marked USER\n");

    START_TICK.store(timer::ticks(), Ordering::Relaxed);
    sched::set_syscall_hook(syscall);
    sched::spawn_user(GOOD_TASK, CODE_VA, STACK_TOP, good_cr3);
    sched::spawn_user(BAD_TASK, CODE_VA, STACK_TOP, bad_cr3);
    sched::start();
    timer::sti();

    while EXITED.load(Ordering::Relaxed) == 0 {
        core::hint::spin_loop();
    }

    sched::stop();
    timer::cli();
    timer::mask();

    // The privilege the syscalls arrived from, taken from the caller's saved
    // `cs`. The CPU wrote it on entry; a task cannot forge it.
    let cs = sched::last_syscall_cs();
    let cpl = cs & 3;
    let good_noted = GOOD_NOTED.load(Ordering::Relaxed);
    let bad_noted = BAD_NOTED.load(Ordering::Relaxed);
    let after = NOTED_AFTER_KILL.load(Ordering::Relaxed);
    let status = EXIT_STATUS.load(Ordering::Relaxed);

    print("  [user] calls arrived from CPL ");
    print_i64(cpl as i64);
    print(" (cs=");
    print_hex(cs);
    print(")\n");
    print("  [user] task ");
    print_i64(GOOD_TASK as i64);
    print(": ");
    print_i64(good_noted as i64);
    print(" syscalls, and said so itself on the way out (");
    print_i64(status as i64);
    print("); ");
    print_i64(after as i64);
    print(" of them after the fault\n");
    print("  [user] task ");
    print_i64(BAD_TASK as i64);
    print(": ");
    print_i64(bad_noted as i64);
    print(" syscalls, then killed by the kernel (");
    print_i64(sched::kills() as i64);
    print(" task killed, id ");
    print_i64(sched::last_killed() as i64);
    print(")\n");

    let contained = cpl == 3
        && good_noted >= MIN_NOTES
        && BACKSTOPPED.load(Ordering::Relaxed) == 0
        && status == good_noted
        && bad_noted == 1
        && sched::kills() == 1
        && sched::last_killed() == BAD_TASK
        && after > 0;
    if contained {
        print("  [user] containment: the faulting task died, its neighbour ran on and finished\n");
    } else {
        print("  [user] FAILED: the fault was not contained to the task that caused it\n");
    }
}

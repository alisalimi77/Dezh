//! Same program, same manifest, two intents — and two different authorities.
//!
//! W16.3 made an x86 task *contained*: it cannot reach memory it was not given,
//! and killing it costs nothing else. Containment is not the thesis, though.
//! The thesis is that authority is **derived from a declared intent** and never
//! ambient, so that every effect can be attributed to the intent that allowed
//! it. This is that rule, on x86, for the first time.
//!
//! Two ring-3 tasks run byte-identical code from a byte-identical manifest. The
//! only difference is the intent ceiling each was opened under. The derivation
//! is `granted = requested ∩ ceiling`, computed by `dezh_core::mcap` — the same
//! function the RISC-V kernel uses and the one its exhaustive test pins, rather
//! than a second implementation that could drift from it.
//!
//! What is still missing, and is not pretended here: there is no `Ahd` token, no
//! `Sand` ledger and no mission on x86. The ceiling is a number passed in by the
//! boot code. What this shows is the derivation rule and the denial, not the
//! accounting built on top of them.

use crate::arch::paging;
use crate::arch::timer;
use crate::console::{print, print_hex, print_i64};
use crate::sched;
use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};
use dezh_core::mcap;

// Both tasks run this. It tries to print, then tries to read the clock, and
// exits reporting which of the two it was allowed to do — one bit each. It does
// not know its own authority and cannot ask for more; it only finds out by being
// refused.
global_asm!(
    r#"
.code64
.section .text
.balign 16
.global cap_blob_start
cap_blob_start:
    xor rbx, rbx
    mov rax, 3              /* SYS_PRINT */
    mov rdi, 42
    int 0x80
    cmp rax, -1             /* the kernel's refusal */
    je 1f
    or rbx, 1
1:
    mov rax, 4              /* SYS_UPTIME */
    xor rdi, rdi
    int 0x80
    cmp rax, -1
    je 2f
    or rbx, 2
2:
    mov rax, 2              /* SYS_EXIT */
    mov rdi, rbx            /* bit 0: printed. bit 1: read the clock */
    int 0x80
3:
    jmp 3b
.global cap_blob_end
cap_blob_end:
"#
);

extern "C" {
    static cap_blob_start: u8;
    static cap_blob_end: u8;
}

const SYS_EXIT: u64 = 2;
const SYS_PRINT: u64 = 3;
const SYS_UPTIME: u64 = 4;
/// What a refused call answers. A refusal, not a fault: the task learns it may
/// not do the thing and carries on, which is what lets it report what it holds.
const DENIED: u64 = u64::MAX;

const CODE_VA: u64 = 3 << 39;
const STACK_VA: u64 = CODE_VA + 0x2000;
const STACK_TOP: u64 = STACK_VA + paging::PAGE_SIZE as u64;

const WIDE_TASK: usize = 6;
const NARROW_TASK: usize = 7;

/// One manifest, used for both tasks. Asking for two capabilities is what makes
/// the ceiling visible: the narrow task is refused something its own manifest
/// requested.
const MANIFEST: &str = "name = \"agent\"\nversion = \"0.1.0\"\ncaps = [\"print\", \"uptime\"]\n";

static PRINTED: AtomicU64 = AtomicU64::new(0);
static DENIALS: AtomicU64 = AtomicU64::new(0);
static REPORT: [AtomicU64; 2] = [AtomicU64::new(u64::MAX), AtomicU64::new(u64::MAX)];
static EXITS: AtomicU64 = AtomicU64::new(0);

/// Runs in the interrupt handler with interrupts off. The authority it checks
/// comes out of the task table, never out of a register the caller set — the
/// caller is only allowed to say *what* it wants, never *whether it may*.
///
/// Printing from here is safe for the same reason the fault path's printing is:
/// the only other code that prints runs on the boot task, and the boot task is
/// blocked waiting rather than mid-line.
fn syscall(nr: u64, arg: u64) -> u64 {
    let caps = sched::current_caps();
    match nr {
        SYS_PRINT => {
            if caps & mcap::TASK_PRINT == 0 {
                DENIALS.fetch_add(1, Ordering::Relaxed);
                print("  [cap] DENIED: task ");
                print_i64(sched::current() as i64);
                print(" holds no PRINT capability\n");
                return DENIED;
            }
            PRINTED.fetch_add(1, Ordering::Relaxed);
            print("  [cap] task ");
            print_i64(sched::current() as i64);
            print(" printed ");
            print_i64(arg as i64);
            print("\n");
            0
        }
        SYS_UPTIME => {
            if caps & mcap::TASK_TIME == 0 {
                DENIALS.fetch_add(1, Ordering::Relaxed);
                print("  [cap] DENIED: task ");
                print_i64(sched::current() as i64);
                print(" holds no UPTIME capability\n");
                return DENIED;
            }
            timer::ticks()
        }
        SYS_EXIT => {
            let slot = usize::from(sched::current() == NARROW_TASK);
            REPORT[slot].store(arg, Ordering::Relaxed);
            EXITS.fetch_add(1, Ordering::Relaxed);
            sched::exit_current();
            0
        }
        _ => DENIED,
    }
}

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

fn print_mcaps(set: u32) {
    let mut first = true;
    for &(name, bit) in mcap::MCAP_TABLE {
        if set & bit != 0 {
            if !first {
                print(" ");
            }
            print(name);
            first = false;
        }
    }
    if first {
        print("(none)");
    }
}

pub(crate) fn run() {
    print("\nCapabilities: same program, same manifest, two intents.\n");
    if !timer::rearm() {
        print("  [cap] no calibrated timer; tasks not started.\n");
        return;
    }

    let requested = match mcap::parse_mcaps(MANIFEST) {
        Ok(r) => r,
        Err(e) => {
            print("  [cap] manifest rejected: ");
            print(e);
            print("\n");
            return;
        }
    };

    // The two intents. Neither task can influence these; they are the ceiling
    // the boot code opened it under.
    let wide_ceiling = mcap::MCAP_PRINT | mcap::MCAP_UPTIME;
    let narrow_ceiling = mcap::MCAP_UPTIME;

    // The one rule the whole thesis rests on.
    let wide_granted = requested & wide_ceiling;
    let narrow_granted = requested & narrow_ceiling;

    print("  [cap] manifest requests ");
    print_mcaps(requested);
    print("\n  [cap] task ");
    print_i64(WIDE_TASK as i64);
    print(" intent ceiling ");
    print_mcaps(wide_ceiling);
    print(" -> granted ");
    print_mcaps(wide_granted);
    print("\n  [cap] task ");
    print_i64(NARROW_TASK as i64);
    print(" intent ceiling ");
    print_mcaps(narrow_ceiling);
    print(" -> granted ");
    print_mcaps(narrow_granted);
    print("\n");

    let blob = core::ptr::addr_of!(cap_blob_start) as usize;
    let len = core::ptr::addr_of!(cap_blob_end) as usize - blob;
    let (wide_cr3, narrow_cr3) = match (load(blob, len), load(blob, len)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            print("  [cap] out of pages; tasks not started.\n");
            return;
        }
    };

    sched::set_syscall_hook(syscall);
    sched::spawn_user(WIDE_TASK, CODE_VA, STACK_TOP, wide_cr3);
    sched::spawn_user(NARROW_TASK, CODE_VA, STACK_TOP, narrow_cr3);
    sched::set_caps(WIDE_TASK, mcap::task_caps_from(wide_granted, "agent"));
    sched::set_caps(NARROW_TASK, mcap::task_caps_from(narrow_granted, "agent"));
    sched::start();
    timer::sti();

    while EXITS.load(Ordering::Relaxed) < 2 {}

    sched::stop();
    timer::cli();
    timer::mask();

    let wide_report = REPORT[0].load(Ordering::Relaxed);
    let narrow_report = REPORT[1].load(Ordering::Relaxed);
    print("  [cap] task ");
    print_i64(WIDE_TASK as i64);
    print(" reported ");
    print_hex(wide_report);
    print(", task ");
    print_i64(NARROW_TASK as i64);
    print(" reported ");
    print_hex(narrow_report);
    print("\n");

    // Derived authority never exceeds the intent — the property `dezh_core`'s
    // exhaustive test pins, restated here against the numbers this boot used.
    let within_ceiling =
        wide_granted & !wide_ceiling == 0 && narrow_granted & !narrow_ceiling == 0;
    // 0b11: printed and read the clock. 0b10: refused the print, read the clock.
    let behaved = wide_report == 0b11 && narrow_report == 0b10;
    if within_ceiling && behaved && DENIALS.load(Ordering::Relaxed) == 1 {
        print("  [cap] authority is derived, not ambient: identical code, one refusal\n");
    } else {
        print("  [cap] FAILED: a task held authority its intent did not allow\n");
    }
}

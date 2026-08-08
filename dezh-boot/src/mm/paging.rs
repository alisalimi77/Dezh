//! Sv39 paging: confining U-mode tasks to their own region.
//!
//! The kernel's own root and level-1 tables, the PTE flag vocabulary, the
//! per-task stack geometry, and the two operations that matter at runtime:
//! `set_active_task_mem`, which swaps which task's stack region is reachable,
//! and `enable_paging`, which turns Sv39 on.
//!
//! Splitting this is what shrinks `proc::loader`'s import list, which step 7
//! left as the measurement of how coupled the loader still was. Most of those
//! 31 names were this vocabulary.
//!
//! Boot hart only: the kernel tables are built once during boot and the active
//! task region is swapped by the console scheduler.

use core::arch::asm;

use crate::mm::global::Global;
use crate::sched::MAX_TASKS;
use crate::user_region;

// kernel and a user task is done with page tables: kernel + MMIO pages are
// supervisor-only (U=0); only the user region is U=1. A U-mode access anywhere
// else page-faults.
#[repr(align(4096))]
pub(crate) struct PageTable(pub(crate) [u64; 512]);
pub(crate) static ROOT: Global<PageTable> = Global::new(PageTable([0; 512]));
pub(crate) static L1: Global<PageTable> = Global::new(PageTable([0; 512]));

pub(crate) const PTE_V: u64 = 1 << 0;
pub(crate) const PTE_R: u64 = 1 << 1;
pub(crate) const PTE_W: u64 = 1 << 2;
pub(crate) const PTE_X: u64 = 1 << 3;
pub(crate) const PTE_U: u64 = 1 << 4;
pub(crate) const PTE_A: u64 = 1 << 6;
pub(crate) const PTE_D: u64 = 1 << 7;

pub(crate) const RAM_BASE: u64 = 0x8000_0000;
pub(crate) const MEGA: u64 = 0x20_0000; // 2 MiB megapage

pub(crate) fn pte(pa: u64, flags: u64) -> u64 {
    ((pa >> 12) << 10) | PTE_V | PTE_A | PTE_D | flags
}

/// Base of the per-task stack regions: the 2 MiB megapage right after the shared
/// code region. Task `i` owns the megapage `STACK_BASE + i*2MiB`.
pub(crate) fn stack_base() -> u64 {
    user_region().1 as u64
}

pub(crate) fn task_stack_top(i: usize) -> usize {
    (stack_base() + (i as u64 + 1) * MEGA) as usize
}

pub(crate) fn stack_region_l1_index(i: usize) -> usize {
    (((stack_base() - RAM_BASE) / MEGA) as usize) + i
}

pub(crate) fn build_page_tables() {
    let (us, ue) = user_region();
    let code_lo = us as u64;
    let code_hi = ue as u64;
    let sbase = stack_base();
    let stacks_hi = sbase + (MAX_TASKS as u64) * MEGA;
    unsafe {
        let root = &mut (*ROOT.get()).0;
        let l1 = &mut (*L1.get()).0;
        // 0x0..0x4000_0000 as one kernel-only gigapage (covers UART + finisher).
        root[0] = pte(0x0, PTE_R | PTE_W | PTE_X); // U=0
                                                   // 0x8000_0000..0xC000_0000 via an L1 table of 2 MiB megapages.
        let l1_pa = L1.get() as u64;
        root[2] = ((l1_pa >> 12) << 10) | PTE_V; // non-leaf pointer
        // Index form is deliberate: `L1` is still a bare `static mut`, and the
        // iterator rewrite would take a reference to it - the pattern this
        // crate no longer has anywhere. It moves to `Global<T>` with the page
        // tables (W10.3).
        #[allow(clippy::needless_range_loop)]
        for i in 0..512usize {
            let pa = RAM_BASE + (i as u64) * MEGA;
            let flags = if pa >= code_lo && pa < code_hi {
                // Shared task code: read+execute for U-mode, no write (W^X).
                PTE_R | PTE_X | PTE_U
            } else if pa >= sbase && pa < stacks_hi {
                // Per-task stack: read+write, U bit toggled per running task.
                PTE_R | PTE_W
            } else {
                // Kernel + MMIO: supervisor-only.
                PTE_R | PTE_W | PTE_X
            };
            l1[i] = pte(pa, flags);
        }
    }
}

/// Make only `active`'s stack region U-accessible; clear U on every other task's
/// stack. This is what isolates tasks from each other: while task `i` runs, it
/// can touch its own stack but a load/store into another task's region faults.
pub(crate) fn set_active_task_mem(active: usize) {
    unsafe {
        let l1 = &mut (*L1.get()).0;
        for i in 0..MAX_TASKS {
            let idx = stack_region_l1_index(i);
            if i == active {
                l1[idx] |= PTE_U;
            } else {
                l1[idx] &= !PTE_U;
            }
        }
        asm!("sfence.vma");
    }
}

pub(crate) fn enable_paging() {
    let root_pa = ROOT.get() as u64;
    let satp = (8u64 << 60) | (root_pa >> 12); // mode 8 = Sv39
    unsafe {
        asm!("sfence.vma");
        asm!("csrw satp, {}", in(reg) satp);
        asm!("sfence.vma");
        asm!("csrs sstatus, {}", in(reg) 1usize << 18); // SUM: S-mode may read U pages
    }
}

//! Page tables: a bump allocator, and one address space per task.
//!
//! The trampoline built a single address space and every task has shared it so
//! far. A task with its own `cr3` sees only what its own tables map, which is
//! the mechanism the RISC-V kernel uses for isolation and the one this kernel
//! has been missing.
//!
//! Every new address space copies the kernel's top-level entries rather than
//! rebuilding them. Those entries carry no USER bit at any level, so kernel
//! memory stays unreachable from CPL3 while remaining mapped — which it must be,
//! because an interrupt taken in a user task runs kernel code on that task's
//! `cr3`.
//!
//! Physical and virtual addresses are the same number here: everything this
//! allocator hands out lives in the first 2 MiB, which the trampoline
//! identity-maps. That equality is an assumption of this whole file.

use crate::global::Global;
use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const PAGE_SIZE: usize = 4096;

pub(crate) const PRESENT: u64 = 1 << 0;
pub(crate) const WRITABLE: u64 = 1 << 1;
pub(crate) const USER: u64 = 1 << 2;

const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Pages for page tables and for whatever tasks are given. Small on purpose: it
/// all has to fit inside the identity-mapped first 2 MiB alongside the kernel.
const POOL_PAGES: usize = 64;

#[repr(align(4096))]
struct Pool {
    pages: [[u8; PAGE_SIZE]; POOL_PAGES],
}

/// Handed out by `alloc_page` and never returned. Nothing frees anything in this
/// kernel yet, so a bump index is the whole allocator.
static POOL: Global<Pool> = Global::new(Pool {
    pages: [[0; PAGE_SIZE]; POOL_PAGES],
});
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn pages_used() -> usize {
    NEXT.load(Ordering::Relaxed)
}

/// A zeroed 4 KiB page, or `None` when the pool is spent.
pub(crate) fn alloc_page() -> Option<*mut u8> {
    let i = NEXT.fetch_add(1, Ordering::Relaxed);
    if i >= POOL_PAGES {
        return None;
    }
    unsafe {
        let pages = core::ptr::addr_of_mut!((*POOL.get()).pages) as *mut [u8; PAGE_SIZE];
        let p = pages.add(i) as *mut u8;
        core::ptr::write_bytes(p, 0, PAGE_SIZE);
        Some(p)
    }
}

/// The address a page fault was taken on. Only meaningful inside a page-fault
/// handler, and only before interrupts are re-enabled.
pub(crate) fn fault_address() -> u64 {
    let cr2: u64;
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)) };
    cr2
}

pub(crate) fn current_cr3() -> u64 {
    let cr3: u64;
    unsafe { asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)) };
    cr3
}

/// Switching address spaces. Safe to do from inside an interrupt handler only
/// because every address space maps the kernel at the same addresses, so the
/// code doing the switch stays mapped across it.
pub(crate) fn set_cr3(cr3: u64) {
    unsafe { asm!("mov cr3, {}", in(reg) cr3, options(nostack, preserves_flags)) };
}

/// A new address space sharing the kernel's mappings and nothing else.
pub(crate) fn new_address_space() -> Option<u64> {
    let pml4 = alloc_page()? as *mut u64;
    let kernel = current_cr3() & ADDR_MASK;
    unsafe {
        // The kernel's own top-level entries, verbatim. They describe the
        // identity map and the APIC window, and none of them sets USER.
        for i in 0..512 {
            let entry = core::ptr::read((kernel as *const u64).add(i));
            core::ptr::write(pml4.add(i), entry);
        }
    }
    Some(pml4 as u64)
}

/// Returns the next level down, allocating it if this entry is empty.
unsafe fn descend(table: *mut u64, index: usize) -> Option<*mut u64> {
    unsafe {
        let entry = core::ptr::read(table.add(index));
        if entry & PRESENT != 0 {
            return Some((entry & ADDR_MASK) as *mut u64);
        }
        let page = alloc_page()? as u64;
        // Permissive at every level above the leaf: the leaf entry is what decides
        // whether a page is writable and whether CPL3 may touch it.
        core::ptr::write(table.add(index), page | PRESENT | WRITABLE | USER);
        Some(page as *mut u64)
    }
}

/// Maps one 4 KiB page. Returns false only when the pool is spent.
///
/// Safety: `pml4` must be an address space made by `new_address_space`, and
/// `va` must not already be mapped to something the caller still needs.
pub(crate) unsafe fn map_page(pml4: u64, va: u64, pa: u64, flags: u64) -> bool {
    unsafe {
        let l4 = ((va >> 39) & 0x1FF) as usize;
        let l3 = ((va >> 30) & 0x1FF) as usize;
        let l2 = ((va >> 21) & 0x1FF) as usize;
        let l1 = ((va >> 12) & 0x1FF) as usize;
        let pdpt = match descend(pml4 as *mut u64, l4) {
            Some(p) => p,
            None => return false,
        };
        let pd = match descend(pdpt, l3) {
            Some(p) => p,
            None => return false,
        };
        let pt = match descend(pd, l2) {
            Some(p) => p,
            None => return false,
        };
        core::ptr::write(pt.add(l1), (pa & ADDR_MASK) | flags | PRESENT);
        true
    }
}

/// Allocates a page and maps it into `pml4` at `va`. Returns a pointer the
/// kernel can use to reach it, which is the same address the page has
/// physically.
pub(crate) fn map_new_page(pml4: u64, va: u64, flags: u64) -> Option<*mut u8> {
    let page = alloc_page()?;
    if unsafe { map_page(pml4, va, page as u64, flags) } {
        Some(page)
    } else {
        None
    }
}

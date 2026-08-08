//! The physical frame allocator: a free list of 4 KiB frames over a RAM pool
//! above every static region.
//!
//! The three counters were `static mut` until this module existed. They move to
//! `Global<T>` here rather than in a separate commit, because the split is
//! exactly when a global acquires an owner - and an owner is what makes the
//! concurrency claim below something other than a hope.
//!
//! Boot hart only: frames are allocated and freed by the console's scheduler,
//! which does not run on a secondary hart. W13 changes that, and this module is
//! one of the places that will need the lock when it does.

use crate::mm::global::Global;

// A free list of 4 KiB physical frames over a RAM pool above all static
// regions. Every frame is ZEROED on allocation, so memory handed to a new
// process can never leak a previous owner's bytes — capability hygiene, and an
// avoidable mistake we do not repeat.
pub(crate) const FRAME_SIZE: usize = 4096;
pub(crate) const FRAME_POOL_BASE: usize = 0x8100_0000; // 16 MiB into RAM (above kernel/.user/stacks)
pub(crate) const FRAME_POOL_END: usize = 0x8800_0000; // end of the 128 MiB QEMU `virt` RAM

pub(crate) static FRAME_FREE_HEAD: Global<usize> = Global::new(0); // 0 = empty; otherwise a free frame's address
pub(crate) static FRAME_TOTAL: Global<usize> = Global::new(0);
pub(crate) static FRAME_FREE: Global<usize> = Global::new(0);

pub(crate) fn frames_init() {
    unsafe {
        (*FRAME_FREE_HEAD.get()) = 0;
        (*FRAME_TOTAL.get()) = 0;
        (*FRAME_FREE.get()) = 0;
        // Link every frame into the free list (each free frame stores the next).
        let mut a = FRAME_POOL_BASE;
        while a + FRAME_SIZE <= FRAME_POOL_END {
            *(a as *mut usize) = *FRAME_FREE_HEAD.get();
            (*FRAME_FREE_HEAD.get()) = a;
            (*FRAME_TOTAL.get()) += 1;
            (*FRAME_FREE.get()) += 1;
            a += FRAME_SIZE;
        }
    }
}

/// Allocate one zeroed physical frame, or 0 if out of memory.
pub(crate) fn frame_alloc() -> usize {
    unsafe {
        let f = *FRAME_FREE_HEAD.get();
        if f == 0 {
            return 0;
        }
        (*FRAME_FREE_HEAD.get()) = *(f as *const usize);
        (*FRAME_FREE.get()) -= 1;
        core::ptr::write_bytes(f as *mut u8, 0, FRAME_SIZE); // zero on alloc
        f
    }
}

/// Return a frame to the free list.
pub(crate) fn frame_free(f: usize) {
    unsafe {
        *(f as *mut usize) = *FRAME_FREE_HEAD.get();
        (*FRAME_FREE_HEAD.get()) = f;
        (*FRAME_FREE.get()) += 1;
    }
}

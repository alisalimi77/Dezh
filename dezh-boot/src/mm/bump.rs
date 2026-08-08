//! The kernel heap: a bump allocator, never freed.
//!
//! `alloc` exists here only because dezh-kernel's boot contract returns `Vec`
//! and `String`. Nothing on the hot path allocates, so a bump pointer that
//! never reclaims is the honest shape rather than a placeholder for a real
//! allocator.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 1 << 20;

struct BumpHeap {
    arena: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpHeap {}
unsafe impl GlobalAlloc for BumpHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.arena.get() as usize;
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (base + cur + layout.align() - 1) & !(layout.align() - 1);
            let new_next = aligned - base + layout.size();
            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange(cur, new_next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
#[global_allocator]
static HEAP: BumpHeap = BumpHeap {
    arena: UnsafeCell::new([0; HEAP_SIZE]),
    next: AtomicUsize::new(0),
};

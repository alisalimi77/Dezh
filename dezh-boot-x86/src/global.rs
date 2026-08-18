//! A mutable global reached only through a raw pointer.
//!
//! Same reasoning as the RISC-V kernel's `mm::global::Global`: `static mut` hands
//! out `&mut` to storage an interrupt handler can also reach, which is aliasing
//! UB, and edition 2024 rejects it outright. `Global<T>` keeps the storage and
//! the zero cost but only ever yields a raw pointer, so no reference to the
//! global exists.
//!
//! The pointer does not make concurrent access safe by itself — it removes the
//! aliasing UB and leaves the ordering argument to the caller. Each `Global` in
//! this kernel therefore states, at its declaration, who may touch it and when.

use core::cell::UnsafeCell;

#[repr(transparent)]
pub(crate) struct Global<T>(UnsafeCell<T>);

// Safety: no reference to the inner value is ever handed out; every read and
// write goes through `get()` inside an `unsafe` block whose argument is
// recorded at the declaration site.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
    pub(crate) fn get(&self) -> *mut T {
        self.0.get()
    }
}

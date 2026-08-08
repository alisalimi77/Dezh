//! A mutable global reached only through a raw pointer.

use core::cell::UnsafeCell;

//
// `static mut` is the wrong tool now that secondary harts run: taking `&` or
// `&mut` to one is undefined behaviour the moment two harts can reach it, and
// edition 2024 rejects it outright. `Global<T>` keeps the same storage and the
// same zero cost, but its only accessor hands back a `*mut T`, so a reference
// to the global is never created and two overlapping `&mut` cannot exist.
//
// The pointer does not by itself make concurrent access safe - it removes the
// aliasing UB and leaves the ordering argument to the caller. Each `Global`
// below therefore states, at its declaration, which hart may touch it.
#[repr(transparent)]
pub(crate) struct Global<T>(UnsafeCell<T>);
// Safety: no reference to the inner value is ever handed out; every read and
// write goes through `get()` inside an `unsafe` block whose concurrency
// argument is recorded at the declaration site.
unsafe impl<T> Sync for Global<T> {}
impl<T> Global<T> {
    pub(crate) const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
    pub(crate) fn get(&self) -> *mut T {
        self.0.get()
    }
}

//! g_granted — handed a READ capability for resource A (handle 0).
//!
//! Proves the *positive* case: with a valid capability the guest can act.
//! It reads handle 0 and returns the checksum of the bytes it got back, so the
//! host test can confirm it read the *granted* resource's actual content (and
//! not some other resource it was never handed a capability for).
//!
//! This guest is `no_std` with no allocator: it imports ONLY the host functions
//! from the `dezh` module. There is no WASI, no `env`, nothing ambient.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

// The ONLY things this guest can call. Every name is resolved by the host's
// capability-gated Linker; there is no other import surface.
#[link(wasm_import_module = "dezh")]
extern "C" {
    fn cap_read(handle: u32, out_ptr: *mut u8, out_cap: u32) -> i32;
}

// Destination buffer in the guest's own linear memory.
static mut BUF: [u8; 256] = [0; 256];

#[no_mangle]
pub extern "C" fn run() -> i64 {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(BUF) as *mut u8;
        let n = cap_read(0, ptr, 256);
        if n < 0 {
            return n as i64; // surface the error code unchanged
        }
        // Checksum the bytes actually read.
        let mut sum: i64 = 0;
        let mut i = 0usize;
        while i < n as usize {
            sum += BUF[i] as i64;
            i += 1;
        }
        sum
    }
}

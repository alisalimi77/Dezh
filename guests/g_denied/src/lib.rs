//! g_denied — handed NO capability at all.
//!
//! Proves zero ambient authority: with an empty capability table, every host
//! call (read, write, print) on handle 0 must fail cleanly. The guest cannot
//! reach any resource because it holds no capability and cannot forge one.
//!
//! Returns the read error code (expected -1, NoSuchHandle) when ALL three ops
//! were denied; returns 0 (an unexpected success) otherwise.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[link(wasm_import_module = "dezh")]
extern "C" {
    fn cap_read(handle: u32, out_ptr: *mut u8, out_cap: u32) -> i32;
    fn cap_write(handle: u32, src_ptr: *const u8, src_len: u32) -> i32;
    fn cap_print(handle: u32, src_ptr: *const u8, src_len: u32) -> i32;
}

static mut BUF: [u8; 64] = [0; 64];
static SRC: [u8; 4] = [1, 2, 3, 4];

#[no_mangle]
pub extern "C" fn run() -> i64 {
    unsafe {
        let bp = core::ptr::addr_of_mut!(BUF) as *mut u8;
        let sp = core::ptr::addr_of!(SRC) as *const u8;
        let r = cap_read(0, bp, 64);
        let w = cap_write(0, sp, 4);
        let p = cap_print(0, sp, 4);
        if r < 0 && w < 0 && p < 0 {
            r as i64 // every op denied — return the (negative) read error
        } else {
            0 // at least one op leaked through: failure signal for the test
        }
    }
}

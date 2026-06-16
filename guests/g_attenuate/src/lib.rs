//! g_attenuate — handed a READ+WRITE capability for resource A (handle 0).
//!
//! Proves the delegation/attenuation chain and the never-widen guarantee, all
//! from inside the guest:
//!   1. attenuate handle 0 to READ-only      -> new handle (>= 0)
//!   2. write through the attenuated handle   -> MUST be OpNotPermitted (-2)
//!   3. read  through the attenuated handle   -> MUST succeed (>= 0)
//!   4. attempt to widen back to include PRINT-> MUST be Widening (-3)
//!   5. attempt a no-op attenuation (==parent)-> MUST be NotNarrower (-4)
//!
//! Returns 0 iff every expectation held; otherwise a bitmask of which checks
//! failed, so a non-zero return points straight at the broken invariant.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[link(wasm_import_module = "dezh")]
extern "C" {
    fn cap_read(handle: u32, out_ptr: *mut u8, out_cap: u32) -> i32;
    fn cap_write(handle: u32, src_ptr: *const u8, src_len: u32) -> i32;
    fn cap_attenuate(handle: u32, requested_ops: u32) -> i64;
}

// Must mirror dezh-host's Ops bits and CapError codes.
const READ: u32 = 1;
const WRITE: u32 = 2;
const PRINT: u32 = 4;
const OP_NOT_PERMITTED: i32 = -2;
const WIDENING: i64 = -3;
const NOT_NARROWER: i64 = -4;

static mut BUF: [u8; 64] = [0; 64];
static SRC: [u8; 4] = [9, 9, 9, 9];

#[no_mangle]
pub extern "C" fn run() -> i64 {
    unsafe {
        let bp = core::ptr::addr_of_mut!(BUF) as *mut u8;
        let sp = core::ptr::addr_of!(SRC) as *const u8;
        let mut ret: i64 = 0;

        // 1. Narrow READ+WRITE down to READ only.
        let child = cap_attenuate(0, READ);
        if child < 0 {
            return 1024 + child; // could not even attenuate — distinctive failure
        }
        let ch = child as u32;

        // 2. Write via the attenuated (READ-only) capability must be denied.
        if cap_write(ch, sp, 4) != OP_NOT_PERMITTED {
            ret |= 2;
        }
        // 3. Read via the attenuated capability must still work.
        if cap_read(ch, bp, 64) < 0 {
            ret |= 4;
        }
        // 4. Widening attempt (ask for PRINT the parent never had) must fail.
        if cap_attenuate(0, READ | WRITE | PRINT) != WIDENING {
            ret |= 8;
        }
        // 5. No-op "attenuation" equal to the parent must be rejected.
        if cap_attenuate(0, READ | WRITE) != NOT_NARROWER {
            ret |= 16;
        }
        ret
    }
}

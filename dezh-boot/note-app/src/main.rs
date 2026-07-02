//! Dezh Note, the first installable app bundle v0.
//!
//! The app is a separate U-mode ELF. It runs with PRINT | IPC only: no MMIO,
//! no DMA, and no direct block capability.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_SEND: usize = 6;
const SYS_DENIED: usize = usize::MAX;

const ROLE_RUN: usize = 1;
const ROLE_DENY_MMIO: usize = 2;
const ROLE_DENY_BLOCK: usize = 3;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
#[link_section = ".text._start"]
extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "li sp, 0x40700000",
            "j {main}",
            main = sym main,
            options(noreturn)
        )
    }
}

fn sys_print(s: &[u8]) -> usize {
    let rc: usize;
    unsafe {
        asm!("ecall",
            inout("a0") s.as_ptr() as usize => rc,
            in("a1") s.len(),
            in("a7") SYS_PRINT)
    };
    rc
}

fn sys_send(to: usize, word: usize) -> usize {
    let rc: usize;
    unsafe {
        asm!("ecall",
            inout("a0") to => rc,
            in("a1") 0usize,
            in("a2") 0usize,
            in("a3") 0usize,
            in("a4") word,
            in("a7") SYS_SEND)
    };
    rc
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

#[no_mangle]
extern "C" fn main(role: usize, daemon: usize, _arg2: usize, _arg3: usize) -> ! {
    if role == ROLE_RUN {
        sys_print(b"    [note] running with caps=PRINT,IPC only\n");
        sys_print(b"    [note] storage is service-mediated; no direct block/MMIO grant\n");
        sys_exit(0);
    }
    if role == ROLE_DENY_MMIO {
        sys_print(b"    [note] attempting forbidden MMIO without a device grant\n");
        let _ = unsafe { core::ptr::read_volatile(0x5000_0000 as *const u32) };
        sys_print(b"    [note] BUG: MMIO read succeeded\n");
        sys_exit(2);
    }
    if role == ROLE_DENY_BLOCK {
        sys_print(b"    [note] attempting direct block daemon send without IPC authority\n");
        let rc = sys_send(daemon, 0);
        if rc == SYS_DENIED {
            sys_print(b"    [note] direct block IPC denied without TASK_IPC\n");
            sys_exit(0);
        }
        sys_print(b"    [note] BUG: direct block IPC was ambient\n");
        sys_exit(2);
    }
    sys_print(b"    [note] unknown role\n");
    sys_exit(2)
}

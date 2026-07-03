//! Dezh Vault, an installable private-value app.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_SEND: usize = 6;
const SYS_DENIED: usize = usize::MAX;

const ROLE_RUN: usize = 1;
const ROLE_DENY_BLOCK: usize = 2;
const ROLE_DENY_MMIO: usize = 3;

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

fn sys_print(s: &[u8]) {
    unsafe {
        asm!("ecall",
            in("a0") s.as_ptr() as usize,
            in("a1") s.len(),
            in("a7") SYS_PRINT,
            lateout("a0") _,
            lateout("a1") _)
    };
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
        sys_print(b"\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_print(b"  | Dezh Vault :: private app storage           |\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_print(b"  | Commands: vault-put <text>, vault-get       |\n");
        sys_print(b"  | Path    : app registry -> virtio-block IPC  |\n");
        sys_print(b"  | Caps    : PRINT,IPC only; no block direct   |\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_exit(0);
    }
    if role == ROLE_DENY_BLOCK {
        sys_print(b"    [vault-deny] attempting block IPC without IPC cap\n");
        let rc = sys_send(daemon, 0);
        if rc == SYS_DENIED {
            sys_print(b"    [vault-deny] direct block IPC denied\n");
            sys_exit(0);
        }
        sys_print(b"    [vault-deny] BUG: direct block IPC was ambient\n");
        sys_exit(2);
    }
    if role == ROLE_DENY_MMIO {
        sys_print(b"    [vault-deny] attempting MMIO without device grant\n");
        let _ = unsafe { core::ptr::read_volatile(0x5000_0000 as *const u32) };
        sys_print(b"    [vault-deny] BUG: MMIO read succeeded\n");
        sys_exit(2);
    }
    sys_print(b"    [vault] unknown role\n");
    sys_exit(2)
}

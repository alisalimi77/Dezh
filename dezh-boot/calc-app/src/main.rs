//! Dezh Calc, an installable U-mode calculator app.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;

const ROLE_RUN: usize = 1;
const ROLE_EVAL: usize = 2;
const OP_ADD: usize = 1;
const OP_SUB: usize = 2;
const OP_MUL: usize = 3;
const OP_DIV: usize = 4;

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

fn print_usize(mut v: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if v == 0 {
        sys_print(b"0");
        return;
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    sys_print(&buf[i..]);
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

fn print_op(op: usize) {
    match op {
        OP_ADD => sys_print(b"+"),
        OP_SUB => sys_print(b"-"),
        OP_MUL => sys_print(b"*"),
        OP_DIV => sys_print(b"/"),
        _ => sys_print(b"?"),
    }
}

fn eval(op: usize, a: usize, b: usize) -> Option<usize> {
    match op {
        OP_ADD => Some(a.saturating_add(b)),
        OP_SUB => Some(a.saturating_sub(b)),
        OP_MUL => Some(a.saturating_mul(b)),
        OP_DIV => {
            if b == 0 {
                None
            } else {
                Some(a / b)
            }
        }
        _ => None,
    }
}

#[no_mangle]
extern "C" fn main(role: usize, op: usize, a: usize, b: usize) -> ! {
    if role == ROLE_RUN {
        sys_print(b"\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_print(b"  | Dezh Calc :: installed U-mode app           |\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_print(b"  | Commands: calc <n> <+|-|*|/> <n>            |\n");
        sys_print(b"  | Storage : last result through app registry  |\n");
        sys_print(b"  | Caps    : PRINT,IPC only; no device grant   |\n");
        sys_print(b"  +---------------------------------------------+\n");
        sys_exit(0);
    }
    if role == ROLE_EVAL {
        sys_print(b"    [calc] ");
        print_usize(a);
        sys_print(b" ");
        print_op(op);
        sys_print(b" ");
        print_usize(b);
        sys_print(b" = ");
        if let Some(result) = eval(op, a, b) {
            print_usize(result);
            sys_print(b"\n");
            sys_exit(0);
        }
        sys_print(b"error\n");
        sys_exit(2);
    }
    sys_print(b"    [calc] unknown role\n");
    sys_exit(2)
}

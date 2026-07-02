//! Dezh benchmark/validation app.
//!
//! This is a separately-built U-mode ELF. It deliberately reaches the system
//! only through the public syscall ABI, so benchmark commands exercise the same
//! boundary future installable apps will use.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_UPTIME: usize = 2;
const SYS_YIELD: usize = 3;
const SYS_NULL: usize = 4;
const SYS_REPORT: usize = 5;
const SYS_SEND: usize = 6;
const SYS_RECV: usize = 7;
const SYS_PRINTNUM: usize = 8;
const SYS_DENIED: usize = usize::MAX;

const ROLE_SYSCALL: usize = 1;
const ROLE_IPC_SERVICE: usize = 2;
const ROLE_IPC_CLIENT: usize = 3;
const ROLE_CAPS: usize = 4;

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

fn sys_printnum(v: usize) {
    unsafe { asm!("ecall", inout("a0") v => _, in("a7") SYS_PRINTNUM) }
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

fn sys_yield() {
    unsafe { asm!("ecall", in("a7") SYS_YIELD, lateout("a0") _, lateout("a1") _) };
}

fn sys_null() {
    unsafe { asm!("ecall", in("a7") SYS_NULL, lateout("a0") _, lateout("a1") _) };
}

fn sys_uptime() -> usize {
    let t: usize;
    unsafe { asm!("ecall", in("a7") SYS_UPTIME, lateout("a0") t, lateout("a1") _) };
    t
}

fn sys_report(ticks: usize, iters: usize) {
    unsafe { asm!("ecall", inout("a0") ticks => _, in("a1") iters, in("a7") SYS_REPORT) };
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

fn sys_recv() -> (usize, usize) {
    let from: usize;
    let word: usize;
    unsafe {
        asm!("ecall",
            inout("a0") 0usize => _,
            inout("a1") 0usize => from,
            out("a2") word,
            in("a7") SYS_RECV)
    };
    (word, from)
}

fn rdtime() -> usize {
    let t: usize;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

fn print_line_num(prefix: &[u8], value: usize) {
    let mut buf = [0u8; 96];
    let mut n = 0usize;
    while n < prefix.len() && n < buf.len() - 1 {
        buf[n] = prefix[n];
        n += 1;
    }
    let mut digits = [0u8; 20];
    let mut v = value;
    let mut d = 0usize;
    if v == 0 {
        digits[0] = b'0';
        d = 1;
    } else {
        while v > 0 && d < digits.len() {
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            d += 1;
        }
    }
    while d > 0 && n < buf.len() - 1 {
        d -= 1;
        buf[n] = digits[d];
        n += 1;
    }
    buf[n] = b'\n';
    n += 1;
    sys_print(&buf[..n]);
}

fn bench_syscall(iters: usize) -> ! {
    sys_print(b"    [bench-os] U-mode syscall app started\n");
    let t0 = rdtime();
    let mut i = 0usize;
    while i < iters {
        sys_null();
        i += 1;
    }
    let ticks = rdtime().wrapping_sub(t0);
    sys_report(ticks, iters);
    sys_print(b"    [bench-os] syscall boundary complete\n");
    sys_exit(0)
}

fn bench_ipc_service(iters: usize) -> ! {
    sys_print(b"    [bench-ipc-service] waiting for messages\n");
    let mut seen = 0usize;
    while seen < iters {
        let (_word, _from) = sys_recv();
        seen += 1;
        sys_yield();
    }
    print_line_num(b"    [bench-ipc-service] received messages=", seen);
    sys_exit(0)
}

fn bench_ipc_client(service: usize, iters: usize) -> ! {
    print_line_num(b"    [bench-ipc-client] sending messages=", iters);
    let t0 = rdtime();
    let mut sent = 0usize;
    let mut denied = 0usize;
    while sent < iters {
        if sys_send(service, sent) == 0 {
            sent += 1;
        } else {
            denied += 1;
        }
        sys_yield();
    }
    let ticks = rdtime().wrapping_sub(t0);
    print_line_num(b"    [bench-ipc-client] sent=", sent);
    print_line_num(b"    [bench-ipc-client] denied=", denied);
    sys_report(ticks, iters);
    sys_exit(if denied == 0 { 0 } else { 1 })
}

fn bench_caps() -> ! {
    sys_print(b"    [bench-caps] app has PRINT only; probing denied syscalls\n");
    let uptime = sys_uptime();
    if uptime == SYS_DENIED {
        sys_print(b"    [bench-caps] TIME denied as expected\n");
    } else {
        sys_print(b"    [bench-caps] BUG: TIME was ambient\n");
        sys_exit(1)
    }
    let send = sys_send(0, 0);
    if send == SYS_DENIED {
        sys_print(b"    [bench-caps] IPC denied as expected\n");
        sys_exit(0)
    } else {
        sys_print(b"    [bench-caps] BUG: IPC was ambient\n");
        sys_exit(1)
    }
}

#[no_mangle]
extern "C" fn main(role: usize, arg1: usize, arg2: usize, _arg3: usize) -> ! {
    match role {
        ROLE_SYSCALL => bench_syscall(arg1),
        ROLE_IPC_SERVICE => bench_ipc_service(arg1),
        ROLE_IPC_CLIENT => bench_ipc_client(arg1, arg2),
        ROLE_CAPS => bench_caps(),
        _ => {
            let _ = sys_print(b"    [bench] unknown role\n");
            sys_exit(2)
        }
    }
}

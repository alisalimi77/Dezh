//! Dezh Lab, an installable terminal UI app that stresses OS components.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_YIELD: usize = 3;
const SYS_SEND: usize = 6;
const SYS_RECV: usize = 7;
const SYS_DENIED: usize = usize::MAX;

const ROLE_UI: usize = 1;
const ROLE_WORKER: usize = 2;
const ROLE_DENY_BLOCK: usize = 3;
const ROLE_DENY_MMIO: usize = 4;

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

fn sys_yield() {
    unsafe { asm!("ecall", in("a7") SYS_YIELD, lateout("a0") _, lateout("a1") _) };
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

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
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

fn role_ui(expected: usize) -> ! {
    sys_print(b"\n");
    sys_print(b"  +--------------------------------------------------+\n");
    sys_print(b"  | Dezh Lab :: installable app system probe         |\n");
    sys_print(b"  +--------------------------------------------------+\n");
    sys_print(b"  | UI        terminal dashboard                     |\n");
    sys_print(b"  | Runtime   3 foreground U-mode tasks              |\n");
    sys_print(b"  | IPC       worker -> dashboard scalar messages    |\n");
    sys_print(b"  | Storage   private app sector via virtio service  |\n");
    sys_print(b"  | Caps      PRINT,IPC only; no device/DMA grant    |\n");
    sys_print(b"  +--------------------------------------------------+\n");
    sys_print(b"  [lab-ui] waiting for worker signals\n");
    let mut got = 0usize;
    while got < expected {
        let (word, from) = sys_recv();
        print_line_num(b"  [lab-ui] signal from task=", from);
        print_line_num(b"  [lab-ui] payload=", word);
        got += 1;
    }
    print_line_num(b"  [lab-ui] worker signals received=", got);
    sys_print(b"  [lab-ui] PASS: scheduler, IPC, installer launch, and UI path cooperated\n");
    sys_exit(0)
}

fn role_worker(ui_task: usize, worker_id: usize) -> ! {
    print_line_num(b"    [lab-worker] start id=", worker_id);
    let mut i = 0usize;
    while i < 4 {
        sys_yield();
        i += 1;
    }
    let rc = sys_send(ui_task, 700 + worker_id);
    if rc == 0 {
        print_line_num(b"    [lab-worker] sent signal id=", worker_id);
        sys_exit(0)
    }
    sys_print(b"    [lab-worker] BUG: IPC send failed\n");
    sys_exit(1)
}

fn role_deny_block(daemon: usize) -> ! {
    sys_print(b"    [lab-deny] attempting direct block IPC without IPC cap\n");
    let rc = sys_send(daemon, 0);
    if rc == SYS_DENIED {
        sys_print(b"    [lab-deny] direct block IPC denied\n");
        sys_exit(0)
    }
    sys_print(b"    [lab-deny] BUG: direct block IPC was ambient\n");
    sys_exit(2)
}

fn role_deny_mmio() -> ! {
    sys_print(b"    [lab-deny] attempting MMIO without device grant\n");
    let _ = unsafe { core::ptr::read_volatile(0x5000_0000 as *const u32) };
    sys_print(b"    [lab-deny] BUG: MMIO read succeeded\n");
    sys_exit(2)
}

#[no_mangle]
extern "C" fn main(role: usize, arg1: usize, arg2: usize, _arg3: usize) -> ! {
    match role {
        ROLE_UI => role_ui(arg1),
        ROLE_WORKER => role_worker(arg1, arg2),
        ROLE_DENY_BLOCK => role_deny_block(arg1),
        ROLE_DENY_MMIO => role_deny_mmio(),
        _ => {
            sys_print(b"    [lab] unknown role\n");
            sys_exit(2)
        }
    }
}

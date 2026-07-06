//! An unmodified static Linux/RISC-V program.
//!
//! There is nothing Dezh-specific here: it issues ordinary Linux riscv64
//! syscalls (`write`, `getpid`, `exit_group`) via `ecall`, exactly as a binary
//! compiled for real Linux would. Dezh loads it into its own address space and
//! the Pol personality layer services each syscall — capability-gated. Run it
//! without the print capability and the kernel denies the `write`.
#![no_std]
#![no_main]
use core::arch::asm;

// Linux riscv64 syscall numbers.
const SYS_WRITE: usize = 64;
const SYS_GETPID: usize = 172;
const SYS_EXIT_GROUP: usize = 94;

#[inline(always)]
unsafe fn syscall3(n: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    asm!(
        "ecall",
        in("a7") n,
        inlateout("a0") a0 as isize => ret,
        in("a1") a1,
        in("a2") a2,
        options(nostack)
    );
    ret
}

#[inline(always)]
unsafe fn write(fd: usize, bytes: &[u8]) -> isize {
    syscall3(SYS_WRITE, fd, bytes.as_ptr() as usize, bytes.len())
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // 1. A capability-gated write to stdout. Without the print capability the
    //    Pol layer returns -EACCES and nothing reaches the console.
    unsafe {
        write(1, b"[linux] hello from an unmodified static riscv64 Linux ELF\n");
    }

    // 2. A syscall Pol does not implement returns a clean -ENOSYS, just like an
    //    unsupported syscall on a minimal Linux — no crash, no ambient fallback.
    let pid = unsafe { syscall3(SYS_GETPID, 0, 0, 0) };
    if pid < 0 {
        unsafe {
            write(
                1,
                b"[linux] getpid() -> -ENOSYS: unsupported syscall, denied cleanly\n",
            );
        }
    }

    // 3. Exit through the real Linux exit_group syscall.
    unsafe {
        syscall3(SYS_EXIT_GROUP, 0, 0, 0);
    }
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

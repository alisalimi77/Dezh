//! Dezh on x86_64 — milestone 1: boot via Multiboot, climb to 64-bit long mode,
//! and talk to the COM1 serial port. QEMU loads this ELF directly with `-kernel`
//! (Multiboot1), entering in 32-bit protected mode; the trampoline below sets up
//! identity paging + long mode, then calls `kmain`.
//!
//! The architecture-independent Dezh logic (capabilities, Cairn, IPC, the IR
//! engine) is shared later; this crate is the x86 hardware layer (boot, paging,
//! traps, context switch) — the only part that must be written per ISA.

#![no_std]
#![no_main]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

// --- Boot trampoline: Multiboot1 -> 32-bit -> identity paging -> long mode ----
global_asm!(
    r#"
/* PVH boot note: lets QEMU's -kernel load this 64-bit ELF directly and enter
   _start in 32-bit protected mode (XEN_ELFNOTE_PHYS32_ENTRY = 18). */
.section .note.Xen, "a"
.align 4
    .long 4                  /* namesz ("Xen\0") */
    .long 4                  /* descsz (entry address) */
    .long 18                 /* type = PHYS32_ENTRY */
    .asciz "Xen"
    .long _start

.section .bss
.align 4096
pml4:  .skip 4096
pdpt:  .skip 4096
pd:    .skip 4096
pt:    .skip 4096
.align 16
stack_bottom: .skip 16384
stack_top:

.section .rodata
.align 8
gdt64:
    .quad 0                                              /* null descriptor */
    .quad (1<<43)|(1<<44)|(1<<47)|(1<<53)               /* 64-bit code segment */
gdt64_ptr:
    .word gdt64_ptr - gdt64 - 1
    .quad gdt64

.section .text
.code32
.global _start
_start:
    mov esp, offset stack_top

    /* PML4[0] -> PDPT  (offset = address of the symbol, not its contents) */
    mov eax, offset pdpt
    or eax, 0x3
    mov [pml4], eax
    mov dword ptr [pml4+4], 0
    /* PDPT[0] -> PD */
    mov eax, offset pd
    or eax, 0x3
    mov [pdpt], eax
    mov dword ptr [pdpt+4], 0
    /* PD[0] -> PT */
    mov eax, offset pt
    or eax, 0x3
    mov [pd], eax
    mov dword ptr [pd+4], 0
    /* PT[i] -> identity 4 KiB pages, 512 entries = first 2 MiB (covers kernel) */
    mov ecx, 0
1:
    mov eax, 0x1000
    mul ecx                       /* edx:eax = 4KiB * ecx */
    or eax, 0x3                   /* present | writable */
    mov [pt + ecx*8], eax
    mov dword ptr [pt + ecx*8 + 4], 0
    inc ecx
    cmp ecx, 512
    jb 1b

    /* load CR3 */
    mov eax, offset pml4
    mov cr3, eax
    /* enable PAE (CR4.PAE) */
    mov eax, cr4
    or eax, 1<<5
    mov cr4, eax
    /* set EFER.LME (long mode enable) */
    mov ecx, 0xC0000080
    rdmsr
    or eax, 1<<8
    wrmsr
    /* enable paging (CR0.PG) -> long mode (compatibility) */
    mov eax, cr0
    or eax, 1<<31
    mov cr0, eax

    /* load 64-bit GDT and far-return into the 64-bit code segment */
    lgdt [gdt64_ptr]
    push 0x08                     /* code selector (CS) */
    .byte 0x68                    /* push imm32 opcode -> force 32-bit operand */
    .long long_mode_start         /* return EIP */
    retf

.code64
long_mode_start:
    xor ax, ax
    mov ss, ax
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov rsp, offset stack_top
    call kmain
2:
    hlt
    jmp 2b
"#
);

// --- COM1 serial -------------------------------------------------------------
const COM1: u16 = 0x3F8;

unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    val
}

fn serial_init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1 + 0, 0x03); // divisor low (38400 baud)
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, 1 stop
        outb(COM1 + 2, 0xC7); // enable + clear FIFO
        outb(COM1 + 4, 0x0B); // RTS/DSR set
    }
}

fn putb(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {} // wait for THR empty
        outb(COM1, b);
    }
}

fn print(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            putb(b'\r');
        }
        putb(b);
    }
}

fn print_i64(mut v: i64) {
    if v < 0 {
        putb(b'-');
        v = -v;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for &b in &buf[i..] {
        putb(b);
    }
}

// The x86 implementation of the shared Dezh-core Host: capability checks + the
// actual side effect (serial output). The Dezh-IR engine itself is shared.
struct SerialHost {
    cap: bool,
}
impl dezh_core::ir::Host for SerialHost {
    fn can(&self, cap: u32) -> bool {
        self.cap && cap == dezh_core::ir::CAP_PRINT
    }
    fn print_num(&mut self, v: i64) {
        print("  [ir] => ");
        print_i64(v);
        print("\n");
    }
    fn print_str(&mut self, s: &[u8]) {
        print("  [ir] ");
        for &b in s {
            putb(b);
        }
        putb(b'\n');
    }
    // No block device on x86 yet (M2/M3); Cairn host calls are unavailable.
    fn cairn_put(&mut self, _data: &[u8]) -> bool {
        false
    }
    fn cairn_get(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    use dezh_core::ir;
    serial_init();
    print("\n");
    print("Dezh x86_64 — long mode reached. 64-bit kernel running.\n");

    // Run the SAME Dezh-IR agent program the RISC-V kernel runs — proof that the
    // shared core makes agents portable across ISAs (D003/D016).
    print("Dezh-IR agent (sum 1..=5 with a loop) on x86_64:\n");
    let mut buf = [0u8; 256];
    let prog = ir::demo_sum(&mut buf);
    match ir::verify(prog) {
        Err(_) => print("  verify failed\n"),
        Ok(()) => {
            print("  verified. with PRINT capability:\n");
            let mut h = SerialHost { cap: true };
            let _ = ir::run(prog, &mut h);
            print("  without PRINT capability:\n");
            let mut h = SerialHost { cap: false };
            if ir::run(prog, &mut h) == Err(ir::Trap::MissingCapability) {
                print("  [ir] DENIED: agent holds no PRINT capability\n");
            }
        }
    }

    loop {
        unsafe { asm!("hlt") };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}

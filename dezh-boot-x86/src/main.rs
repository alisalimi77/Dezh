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

/* Multiboot2 header: lets a standard bootloader (GRUB) load this same kernel
   from a bootable ISO — the "install it like a real OS" path (VirtualBox /
   VMware), which the QEMU `-kernel` PVH note above does not provide. arch=0
   (i386) means GRUB hands off in 32-bit protected mode, exactly like PVH, so
   the trampoline below is shared by both boot paths. GRUB uses the ELF entry
   (_start); we read no boot-info, so PVH's and Multiboot2's differing register
   handoff does not matter. */
.section .multiboot_header, "a"
.align 8
mb2_start:
    .long 0xE85250D6                                     /* magic */
    .long 0                                              /* architecture: i386 */
    .long mb2_end - mb2_start                            /* header length */
    .long -(0xE85250D6 + 0 + (mb2_end - mb2_start))      /* checksum */
    /* end tag */
    .short 0
    .short 0
    .long 8
mb2_end:

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

// --- IDT: 32 CPU-exception stubs (M2) ----------------------------------------
// Without an IDT any CPU exception triple-faults and resets the machine. These
// stubs give every exception a uniform (vector, error, rip) frame and route it
// to a Rust handler that reports it and halts — so a fault is diagnosable, not a
// silent reboot. A returnable interrupt path (timer, IRQs) is future work.
global_asm!(
    r#"
.code64
.macro ISR_NOERR n
.global isr\n
isr\n:
    push 0           /* dummy error code so every frame is uniform */
    push \n          /* vector number */
    jmp isr_common
.endm
.macro ISR_ERR n
.global isr\n
isr\n:
    push \n          /* CPU already pushed the real error code */
    jmp isr_common
.endm

ISR_NOERR 0
ISR_NOERR 1
ISR_NOERR 2
ISR_NOERR 3
ISR_NOERR 4
ISR_NOERR 5
ISR_NOERR 6
ISR_NOERR 7
ISR_ERR   8
ISR_NOERR 9
ISR_ERR   10
ISR_ERR   11
ISR_ERR   12
ISR_ERR   13
ISR_ERR   14
ISR_NOERR 15
ISR_NOERR 16
ISR_ERR   17
ISR_NOERR 18
ISR_NOERR 19
ISR_NOERR 20
ISR_ERR   21
ISR_NOERR 22
ISR_NOERR 23
ISR_NOERR 24
ISR_NOERR 25
ISR_NOERR 26
ISR_NOERR 27
ISR_NOERR 28
ISR_NOERR 29
ISR_NOERR 30
ISR_NOERR 31

isr_common:
    mov rdi, [rsp]        /* vector */
    mov rsi, [rsp + 8]    /* error code */
    mov rdx, [rsp + 16]   /* faulting RIP */
    call exception_handler
3:
    hlt
    jmp 3b

.section .rodata
.align 8
.global isr_table
isr_table:
    .quad isr0,  isr1,  isr2,  isr3,  isr4,  isr5,  isr6,  isr7
    .quad isr8,  isr9,  isr10, isr11, isr12, isr13, isr14, isr15
    .quad isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23
    .quad isr24, isr25, isr26, isr27, isr28, isr29, isr30, isr31
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

fn serial_putb(b: u8) {
    unsafe {
        while inb(COM1 + 5) & 0x20 == 0 {} // wait for THR empty
        outb(COM1, b);
    }
}

// --- VGA text mode (0xB8000) -------------------------------------------------
// A bootloader-loaded kernel on real hardware / VirtualBox has no serial console
// on screen; it has the VGA text buffer. We mirror every byte to both, so the
// demo is visible whether the reviewer watches a serial capture (QEMU/CI) or the
// VM window (VirtualBox). 0xB8000 is inside the first 2 MiB the trampoline
// identity-maps, so it is reachable in long mode.
const VGA_BUF: *mut u16 = 0xB8000 as *mut u16;
const VGA_COLS: usize = 80;
const VGA_ROWS: usize = 25;
const VGA_ATTR: u16 = 0x0F00; // white on black
static mut VGA_POS: usize = 0;

fn vga_clear() {
    for i in 0..VGA_COLS * VGA_ROWS {
        unsafe { core::ptr::write_volatile(VGA_BUF.add(i), VGA_ATTR | b' ' as u16) };
    }
    unsafe { VGA_POS = 0 };
}

fn vga_putb(b: u8) {
    unsafe {
        let mut pos = VGA_POS;
        if b == b'\r' {
            return;
        } else if b == b'\n' {
            pos = (pos / VGA_COLS + 1) * VGA_COLS;
        } else {
            core::ptr::write_volatile(VGA_BUF.add(pos), VGA_ATTR | b as u16);
            pos += 1;
        }
        if pos >= VGA_COLS * VGA_ROWS {
            // scroll up one line
            for i in 0..VGA_COLS * (VGA_ROWS - 1) {
                let v = core::ptr::read_volatile(VGA_BUF.add(i + VGA_COLS));
                core::ptr::write_volatile(VGA_BUF.add(i), v);
            }
            for i in VGA_COLS * (VGA_ROWS - 1)..VGA_COLS * VGA_ROWS {
                core::ptr::write_volatile(VGA_BUF.add(i), VGA_ATTR | b' ' as u16);
            }
            pos = VGA_COLS * (VGA_ROWS - 1);
        }
        VGA_POS = pos;
    }
}

fn putb(b: u8) {
    if b == b'\n' {
        serial_putb(b'\r');
    }
    serial_putb(b);
    vga_putb(b);
}

fn print(s: &str) {
    for b in s.bytes() {
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

// --- IDT setup (Rust side) ---------------------------------------------------
extern "C" {
    static isr_table: [u64; 32];
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    off_lo: u16,
    selector: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    zero: u32,
}

#[repr(C, packed)]
struct IdtPtr {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 32] = [IdtEntry {
    off_lo: 0,
    selector: 0,
    ist: 0,
    attr: 0,
    off_mid: 0,
    off_hi: 0,
    zero: 0,
}; 32];

fn idt_init() {
    unsafe {
        for i in 0..32 {
            let addr = isr_table[i];
            IDT[i] = IdtEntry {
                off_lo: addr as u16,
                selector: 0x08, // 64-bit code segment from the boot GDT
                ist: 0,
                attr: 0x8E, // present, DPL0, 64-bit interrupt gate
                off_mid: (addr >> 16) as u16,
                off_hi: (addr >> 32) as u32,
                zero: 0,
            };
        }
        let ptr = IdtPtr {
            limit: (core::mem::size_of::<[IdtEntry; 32]>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };
        asm!("lidt [{}]", in(reg) &ptr, options(nostack));
    }
}

const EXC_NAMES: [&str; 32] = [
    "divide-by-zero", "debug", "NMI", "breakpoint", "overflow", "bound-range",
    "invalid-opcode", "device-not-available", "double-fault", "coprocessor-overrun",
    "invalid-TSS", "segment-not-present", "stack-segment-fault", "general-protection",
    "page-fault", "reserved-15", "x87-fp", "alignment-check", "machine-check",
    "SIMD-fp", "virtualization", "control-protection", "reserved-22", "reserved-23",
    "reserved-24", "reserved-25", "reserved-26", "reserved-27", "hypervisor-injection",
    "VMM-comm", "security", "reserved-31",
];

fn print_hex(mut v: u64) {
    print("0x");
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        let nib = (v & 0xF) as u8;
        buf[i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        v >>= 4;
    }
    for &b in &buf {
        putb(b);
    }
}

#[no_mangle]
extern "C" fn exception_handler(vector: u64, error: u64, rip: u64) -> ! {
    print("\n[trap] CPU exception ");
    print_i64(vector as i64);
    print(" (");
    print(EXC_NAMES.get(vector as usize).copied().unwrap_or("?"));
    print("), error=");
    print_hex(error);
    print(", rip=");
    print_hex(rip);
    print("\n[trap] halting (no ambient recovery).\n");
    loop {
        unsafe { asm!("hlt") };
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    use dezh_core::ir;
    serial_init();
    vga_clear();
    idt_init();
    print("\n");
    print("Dezh x86_64 - long mode reached. 64-bit kernel running.\n");
    print("IDT installed: 32 CPU-exception vectors (faults are reported, not silent).\n");

    // Install and run a real .dzp package (F3, D003/D016): the SAME Dezh-IR bytes
    // the RISC-V kernel runs, wrapped in the SAME architecture-independent .dzp
    // format the SDK builds. We pack it, then parse it back exactly as an install
    // flow would (magic + version + CRC + manifest checks) and run the payload.
    // The bytes are pinned byte-identical by dezh-core's `demo_sum_bytes_are_pinned`
    // test, so what installs on one ISA is exactly what runs on the other.
    use dezh_core::dzp;
    print("Dezh .dzp agent package (sum 1..=5 with a loop) on x86_64:\n");
    let mut prog_buf = [0u8; 256];
    let prog = ir::demo_sum(&mut prog_buf);
    let manifest = "name = \"agent-sum\"\nversion = \"0.1.0\"\ncaps = [\"print\"]\n";
    let mut pkg = [0u8; 512];
    let n = dzp::pack(dzp::KIND_DEZH_IR, manifest, prog, &mut pkg);
    match dzp::parse(&pkg[..n]) {
        Err(e) => {
            print("  .dzp parse failed: ");
            print(e.msg());
            print("\n");
        }
        Ok(p) => {
            print("  .dzp verified: kind=");
            print(dzp::kind_name(p.kind));
            print(", name=");
            print(dzp::manifest_str(p.manifest, "name").unwrap_or("?"));
            print("\n");
            match ir::verify(p.payload) {
                Err(_) => print("  IR verify failed\n"),
                Ok(()) => {
                    print("  with PRINT capability:\n");
                    let mut h = SerialHost { cap: true };
                    let _ = ir::run(p.payload, &mut h);
                    print("  without PRINT capability:\n");
                    let mut h = SerialHost { cap: false };
                    if ir::run(p.payload, &mut h) == Err(ir::Trap::MissingCapability) {
                        print("  [ir] DENIED: agent holds no PRINT capability\n");
                    }
                }
            }
        }
    }

    // Prove the IDT works: deliberately raise a breakpoint (vector 3). Without an
    // IDT this would triple-fault and reset the machine; instead the handler
    // catches it, reports it, and halts cleanly.
    print("\nTrap demo: deliberately raising a breakpoint (int3) to prove the handler catches it...\n");
    unsafe { asm!("int3") };

    // The breakpoint handler halts, so this is unreachable; kept for totality.
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

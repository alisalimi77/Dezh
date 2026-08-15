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

mod console;
mod dev;
mod global;
mod io;

use console::{print, print_hex, print_i64, putb};
use core::arch::{asm, global_asm};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use global::Global;
use io::{inb, io_wait, outb};

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
pml4:    .skip 4096
pdpt:    .skip 4096
pd:      .skip 4096
pt:      .skip 4096
pd_apic: .skip 4096
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

    /* The Local APIC's registers live at 0xFEE00000, which the 2 MiB identity
       map above does not reach, so long mode cannot see the APIC at all without
       this. 0xFEE00000 selects PDPT[3] and, inside it, PD[503]; one 2 MiB page
       there is enough for the whole APIC window. Flags: present | writable |
       page-size | PWT | PCD — MMIO must not be cached. */
    mov eax, offset pd_apic
    or eax, 0x3
    mov [pdpt + 3*8], eax
    mov dword ptr [pdpt + 3*8 + 4], 0
    mov dword ptr [pd_apic + 503*8], 0xFEE0009B
    mov dword ptr [pd_apic + 503*8 + 4], 0

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

// --- IDT: 32 CPU-exception stubs, then 224 returnable interrupt stubs ---------
// Without an IDT any CPU exception triple-faults and resets the machine. The
// first 32 stubs give every exception a uniform (vector, error, rip) frame and
// route it to a Rust handler that reports it and halts — so a fault is
// diagnosable, not a silent reboot.
//
// Vectors 32..255 are a different path and must not end in halt: an interrupt
// interrupts work that was going fine, so the stub has to hand control back to
// the exact instruction it stole the CPU from. Those stubs save every
// general-purpose register, call a dispatcher, restore them, and `iretq`.
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

/* Vectors 32..255. Generated with .rept, so no stub carries a label of its
   own; instead every stub is padded to a fixed 16-byte stride and Rust computes
   stub(v) = isr_ext_stubs + (v - 32) * 16. The padding is what makes that
   arithmetic true: the assembler picks a 2-byte `push imm8` below vector 128
   and a 5-byte `push imm32` at or above it, and relaxes `jmp` between 2 and 5
   bytes, so the bodies are not all the same length. EXT_STUB_STRIDE in the
   Rust side must stay equal to this .balign. */
.balign 16
.global isr_ext_stubs
isr_ext_stubs:
.set vecno, 32
.rept 224
    push 0            /* dummy error code: no vector >= 32 pushes a real one */
    push vecno
    jmp isr_ext_common
    .balign 16
.set vecno, vecno+1
.endr

/* The returnable path. On entry the CPU has pushed ss:rsp, rflags, cs:rip and
   the stub has pushed (dummy error, vector). The CPU aligns rsp to 16 before
   pushing its 5-qword frame, so 5 + 2 + 15 pushes leave rsp 16-byte aligned at
   the `call`, which is what the SysV ABI requires of us. */
isr_ext_common:
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, [rsp + 15*8]   /* vector, below the 15 saved registers */
    call irq_dispatch
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    add rsp, 16             /* drop the vector and the dummy error code */
    iretq

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
    /// First byte of the vector-32..255 stub table; the stubs have no labels of
    /// their own, only a fixed stride (see the .rept block above).
    static isr_ext_stubs: u8;
}

/// Bytes between two consecutive entries of `isr_ext_stubs`. Must equal the
/// `.balign` inside the .rept block.
const EXT_STUB_STRIDE: usize = 16;
const IDT_LEN: usize = 256;

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

/// The IDT. Written once by `idt_init` on the boot CPU before interrupts are
/// enabled, and read by the CPU only thereafter; nothing else touches it.
static IDT: Global<[IdtEntry; IDT_LEN]> = Global::new(
    [IdtEntry {
        off_lo: 0,
        selector: 0,
        ist: 0,
        attr: 0,
        off_mid: 0,
        off_hi: 0,
        zero: 0,
    }; IDT_LEN],
);

fn idt_gate(addr: u64) -> IdtEntry {
    IdtEntry {
        off_lo: addr as u16,
        selector: 0x08, // 64-bit code segment from the boot GDT
        ist: 0,
        attr: 0x8E, // present, DPL0, 64-bit interrupt gate
        off_mid: (addr >> 16) as u16,
        off_hi: (addr >> 32) as u32,
        zero: 0,
    }
}

fn idt_init() {
    unsafe {
        let base = IDT.get() as *mut IdtEntry;
        for (i, &addr) in isr_table.iter().enumerate() {
            core::ptr::write(base.add(i), idt_gate(addr));
        }
        let ext = core::ptr::addr_of!(isr_ext_stubs) as u64;
        for i in 32..IDT_LEN {
            let addr = ext + ((i - 32) * EXT_STUB_STRIDE) as u64;
            core::ptr::write(base.add(i), idt_gate(addr));
        }
        let ptr = IdtPtr {
            limit: (core::mem::size_of::<[IdtEntry; IDT_LEN]>() - 1) as u16,
            base: base as u64,
        };
        asm!("lidt [{}]", in(reg) &ptr, options(nostack));
    }
}

// --- Legacy 8259 PICs: remapped out of the way, then fully masked ------------
// At reset the two 8259s deliver IRQ0..15 on vectors 0x08..0x0F and 0x70..0x77 —
// on top of the CPU exception vectors. A single stray legacy IRQ would then be
// reported as a double fault or a coprocessor overrun. We move them to
// 0x20..0x2F and mask every line: this kernel takes its timer from the Local
// APIC, so nothing should arrive here, and if a line ever unmasks itself the
// vector it lands on says plainly which one it was.
fn pic_remap_and_mask() {
    unsafe {
        outb(0x20, 0x11); // ICW1: init, expect ICW4
        outb(0xA0, 0x11);
        io_wait();
        outb(0x21, 0x20); // ICW2: master vector base
        outb(0xA1, 0x28); // ICW2: slave vector base
        io_wait();
        outb(0x21, 0x04); // ICW3: slave on master IRQ2
        outb(0xA1, 0x02); // ICW3: slave cascade identity
        io_wait();
        outb(0x21, 0x01); // ICW4: 8086 mode
        outb(0xA1, 0x01);
        io_wait();
        outb(0x21, 0xFF); // OCW1: mask every line
        outb(0xA1, 0xFF);
    }
}

// --- Local APIC timer --------------------------------------------------------
// The choice between the Local APIC timer and the legacy 8259 + PIT was decided
// by where this is going: a scheduler needs a timer per CPU, and the LAPIC is
// per CPU while the PIT is one global counter shared by all of them. The RISC-V
// side already has a per-hart timer, so this keeps the two kernels the same
// shape. Every 64-bit CPU has a LAPIC; the PIT and the 8259 are legacy parts
// that modern platforms are removing.
//
// The two costs of that choice are paid here. The APIC register window is at
// 0xFEE00000, outside the 2 MiB the trampoline identity-maps, so the trampoline
// now maps it. And unlike the PIT the LAPIC timer runs at a frequency nobody
// tells you, so it has to be measured — against the PIT, which is still the one
// clock on the machine with a frequency fixed by hardware. Every rate this
// kernel prints is that measurement, not a datasheet number.
const LAPIC_BASE: usize = 0xFEE0_0000;
const LAPIC_ID: usize = 0x020;
const LAPIC_TPR: usize = 0x080;
const LAPIC_EOI: usize = 0x0B0;
const LAPIC_SVR: usize = 0x0F0;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_TIMER_INIT: usize = 0x380;
const LAPIC_TIMER_CUR: usize = 0x390;
const LAPIC_TIMER_DIV: usize = 0x3E0;

const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const SVR_ENABLE: u32 = 1 << 8;
const LAPIC_DIV_16: u32 = 0x3; // divide configuration register encoding
const LAPIC_DIVISOR: u64 = 16;

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11;

/// Timer vector, chosen above the 0x20..0x2F block the 8259s were remapped into
/// so the two can never be confused for one another.
const VEC_TIMER: u8 = 0x30;
/// The APIC's own "never mind" vector, given a number of its own so a spurious
/// delivery is countable instead of landing on some unrelated handler.
const VEC_SPURIOUS: u8 = 0xFF;

const TIMER_HZ: u64 = 100;
const PIT_HZ: u64 = 1_193_182;
const CALIBRATE_MS: u64 = 10;

/// Incremented by the timer handler, read by ordinary kernel code. Relaxed is
/// enough: nothing is published alongside it, and the handler runs on the same
/// CPU as the reader, so the only thing being protected is the increment itself.
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Spurious APIC deliveries. Expected to stay zero; counted rather than ignored
/// so that "zero" is an observation instead of an assumption.
static SPURIOUS: AtomicU64 = AtomicU64::new(0);

fn lapic_read(reg: usize) -> u32 {
    unsafe { core::ptr::read_volatile((LAPIC_BASE + reg) as *const u32) }
}
fn lapic_write(reg: usize, val: u32) {
    unsafe { core::ptr::write_volatile((LAPIC_BASE + reg) as *mut u32, val) };
}

unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
    ((hi as u64) << 32) | lo as u64
}
unsafe fn wrmsr(msr: u32, val: u64) {
    asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") val as u32,
        in("edx") (val >> 32) as u32,
        options(nomem, nostack),
    );
}

fn sti() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}
fn cli() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Turns the Local APIC on. Returns false if firmware relocated the register
/// window away from the address the trampoline mapped, in which case none of the
/// register accesses below would mean anything and the caller must not proceed.
fn lapic_enable() -> bool {
    let base = unsafe { rdmsr(IA32_APIC_BASE) };
    if (base & 0xFFFF_F000) as usize != LAPIC_BASE {
        return false;
    }
    unsafe { wrmsr(IA32_APIC_BASE, base | APIC_BASE_GLOBAL_ENABLE) };
    lapic_write(LAPIC_TPR, 0); // accept every priority; firmware may have raised it
    lapic_write(LAPIC_SVR, SVR_ENABLE | VEC_SPURIOUS as u32);
    true
}

/// Counts how far the LAPIC timer gets while the PIT measures `CALIBRATE_MS`.
///
/// PIT channel 2 is used because it is the one channel wired to no interrupt
/// line and whose output is readable from port 0x61 bit 5 — it can be timed
/// against without an IRQ path existing yet. Returns `None` rather than
/// spinning forever on a machine that has no PIT.
fn lapic_calibrate() -> Option<u32> {
    let reload = (PIT_HZ * CALIBRATE_MS / 1000) as u16;
    unsafe {
        let p61 = inb(0x61);
        outb(0x61, (p61 & 0xFD) | 0x01); // speaker off, gate on
        outb(0x43, 0xB2); // channel 2, lo/hi byte, mode 1 one-shot, binary
        outb(0x42, reload as u8);
        outb(0x80, 0); // settle between the two halves of the reload value
        outb(0x42, (reload >> 8) as u8);

        // Dropping the gate and raising it retriggers the one-shot: t = 0 is
        // here, and channel 2's output stays low until the count runs out.
        let gate = inb(0x61) & 0xFE;
        outb(0x61, gate);
        outb(0x61, gate | 1);

        lapic_write(LAPIC_TIMER_DIV, LAPIC_DIV_16);
        lapic_write(LAPIC_LVT_TIMER, LVT_MASKED); // count, but deliver nothing
        lapic_write(LAPIC_TIMER_INIT, u32::MAX);

        let mut spins: u64 = 0;
        while inb(0x61) & 0x20 == 0 {
            spins += 1;
            if spins > 200_000_000 {
                lapic_write(LAPIC_TIMER_INIT, 0);
                return None;
            }
        }
        let remaining = lapic_read(LAPIC_TIMER_CUR);
        lapic_write(LAPIC_TIMER_INIT, 0);
        match u32::MAX - remaining {
            0 => None,
            counted => Some(counted),
        }
    }
}

fn lapic_arm_periodic(initial: u32) {
    lapic_write(LAPIC_TIMER_DIV, LAPIC_DIV_16);
    lapic_write(LAPIC_LVT_TIMER, LVT_PERIODIC | VEC_TIMER as u32);
    lapic_write(LAPIC_TIMER_INIT, initial);
}

fn lapic_mask_timer() {
    lapic_write(LAPIC_LVT_TIMER, LVT_MASKED);
    lapic_write(LAPIC_TIMER_INIT, 0);
}

/// A deterministic unit of work, run under the armed timer.
///
/// Its running total and loop counter are live in registers across whatever
/// interrupt lands in the middle of it, so an entry path that lost a register
/// shows up here as a wrong sum rather than as a plausible-looking number. The
/// `black_box` is what keeps the sum from being folded away at compile time in
/// the release profile, where there would otherwise be no loop left to interrupt.
fn work_chunk() -> u64 {
    let mut sum: u64 = 0;
    let mut i: u64 = 1;
    while i <= 1000 {
        sum = sum.wrapping_add(core::hint::black_box(i));
        i += 1;
    }
    sum
}
const WORK_CHUNK_SUM: u64 = 500_500;

const EXC_NAMES: [&str; 32] = [
    "divide-by-zero", "debug", "NMI", "breakpoint", "overflow", "bound-range",
    "invalid-opcode", "device-not-available", "double-fault", "coprocessor-overrun",
    "invalid-TSS", "segment-not-present", "stack-segment-fault", "general-protection",
    "page-fault", "reserved-15", "x87-fp", "alignment-check", "machine-check",
    "SIMD-fp", "virtualization", "control-protection", "reserved-22", "reserved-23",
    "reserved-24", "reserved-25", "reserved-26", "reserved-27", "hypervisor-injection",
    "VMM-comm", "security", "reserved-31",
];

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

/// Called from `isr_ext_common` for vectors 32..255, with interrupts off.
///
/// Everything this returns from resumes the interrupted instruction stream, so
/// it must stay short and must not print on the normal path: `print` walks the
/// VGA cursor, and a handler that printed could interleave with a print the
/// interrupted code was halfway through. The one path that does print is the
/// one that never returns.
#[no_mangle]
extern "C" fn irq_dispatch(vector: u64) {
    if vector == VEC_TIMER as u64 {
        TICKS.fetch_add(1, Ordering::Relaxed);
        lapic_write(LAPIC_EOI, 0);
        return;
    }
    if vector == VEC_SPURIOUS as u64 {
        // A spurious vector is the APIC withdrawing an interrupt it had already
        // started to deliver. It takes no EOI — sending one would acknowledge an
        // interrupt that is still in service.
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    print("\n[trap] interrupt vector ");
    print_i64(vector as i64);
    print(" arrived with no handler installed.\n");
    print("[trap] halting (no ambient recovery).\n");
    loop {
        unsafe { asm!("hlt") };
    }
}

/// Arms the timer and then refuses to stop working while it fires.
///
/// The exception path already in this kernel ends every trap in `hlt`, which is
/// only ever an admission that the machine cannot go on. This is the other kind
/// of trap: the CPU is taken away from a loop that was in the middle of a sum
/// and has to be handed back to it. The output below is what distinguishes the
/// two — a tick count that grew, a work loop that kept its arithmetic intact
/// across every one of those ticks, and, after the timer is masked, a tick count
/// that stops while the same loop keeps running.
fn timer_demo() {
    print("\nTimer: arming the Local APIC timer and staying at work through it.\n");
    if !lapic_enable() {
        print("  [timer] Local APIC is not at the mapped window; timer not armed.\n");
        return;
    }
    print("  [timer] Local APIC enabled, id=");
    print_i64((lapic_read(LAPIC_ID) >> 24) as i64);
    print("\n");

    let counted = match lapic_calibrate() {
        None => {
            print("  [timer] PIT calibration returned no counts; timer not armed.\n");
            return;
        }
        Some(c) => c,
    };
    let per_second = counted as u64 * (1000 / CALIBRATE_MS);
    print("  [timer] measured ");
    print_i64(counted as i64);
    print(" LAPIC counts in ");
    print_i64(CALIBRATE_MS as i64);
    print(" ms at divide-16 (APIC bus ");
    print_i64((per_second * LAPIC_DIVISOR) as i64);
    print(" Hz)\n");

    let per_tick = per_second / TIMER_HZ;
    if per_tick == 0 || per_tick > u32::MAX as u64 {
        print("  [timer] measured rate will not fit the requested tick; timer not armed.\n");
        return;
    }

    print("  [timer] armed: vector 0x30, periodic, ");
    print_i64(TIMER_HZ as i64);
    print(" Hz\n");
    lapic_arm_periodic(per_tick as u32);
    sti();

    const TARGET_TICKS: u64 = 10;
    let mut rounds: u64 = 0;
    let mut wrong: u64 = 0;
    while TICKS.load(Ordering::Relaxed) < TARGET_TICKS {
        if work_chunk() != WORK_CHUNK_SUM {
            wrong += 1;
        }
        rounds += 1;
    }
    let ticks = TICKS.load(Ordering::Relaxed);

    // Mask the timer and keep the same loop running with interrupts still on.
    // If the ticks had been coming from anything other than the source just
    // armed, the count would carry on climbing here.
    lapic_mask_timer();
    for _ in 0..64 {
        // Drain anything the APIC had already begun delivering when the mask
        // landed, so the frozen reading below is taken after the last of them.
        let _ = work_chunk();
    }
    let frozen = TICKS.load(Ordering::Relaxed);
    for _ in 0..2000 {
        if work_chunk() != WORK_CHUNK_SUM {
            wrong += 1;
        }
        rounds += 1;
    }
    let after = TICKS.load(Ordering::Relaxed);
    cli();

    print("  [timer] took ");
    print_i64(ticks as i64);
    print(" ticks; the work loop completed ");
    print_i64(rounds as i64);
    print(" rounds\n");
    if wrong == 0 && ticks >= TARGET_TICKS && rounds > 0 {
        print("  [timer] interrupts returned: work resumed after every tick, checksum OK\n");
    } else {
        print("  [timer] FAILED: ");
        print_i64(wrong as i64);
        print(" corrupted rounds\n");
    }
    if after == frozen {
        print("  [timer] masked: tick count frozen at ");
        print_i64(after as i64);
        print(" while the work loop kept running\n");
    } else {
        print("  [timer] FAILED: ticks kept arriving after the timer was masked\n");
    }
    let spurious = SPURIOUS.load(Ordering::Relaxed);
    if spurious != 0 {
        print("  [timer] spurious APIC deliveries: ");
        print_i64(spurious as i64);
        print("\n");
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    use dezh_core::ir;
    console::init();
    idt_init();
    pic_remap_and_mask();
    print("\n");
    print("Dezh x86_64 - long mode reached. 64-bit kernel running.\n");
    print("IDT installed: 32 CPU-exception vectors (faults are reported, not silent)\n");
    print("  plus 224 interrupt vectors on a path that saves state and returns.\n");
    print("Legacy 8259 PICs remapped to 0x20..0x2F and fully masked.\n");

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

    timer_demo();

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

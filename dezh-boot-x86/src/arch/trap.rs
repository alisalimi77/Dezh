//! Traps: three kinds, one entry path.
//!
//! Without an IDT any CPU exception triple-faults and resets the machine. Every
//! stub here — exception, interrupt, syscall — saves all fifteen general-purpose
//! registers, calls a dispatcher with the frame, restores whatever frame the
//! dispatcher returned, and `iretq`s.
//!
//! What differs is what the dispatcher decides. An interrupt resumes the work it
//! interrupted, or hands the CPU to another task. A syscall answers and resumes
//! the caller. A fault depends on who faulted: a kernel fault still ends in a
//! reported halt, because there is nothing left to trust, while a fault at CPL3
//! kills that task alone and returns some other task's frame.

use crate::arch::gdt;
use crate::arch::paging;
use crate::arch::timer;
use crate::console::{print, print_hex, print_i64};
use crate::sched;
use crate::global::Global;
use core::arch::{asm, global_asm};

global_asm!(
    r#"
.code64
.macro ISR_NOERR n
.global isr\n
isr\n:
    push 0           /* dummy error code so every frame is uniform */
    push \n          /* vector number */
    jmp isr_ext_common
.endm
.macro ISR_ERR n
.global isr\n
isr\n:
    push \n          /* CPU already pushed the real error code */
    jmp isr_ext_common
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

/* Vectors 32..255, and since step 4 the exception stubs above as well. A fault
   used to end in `hlt` on the spot, which is the only honest answer when the
   kernel itself faulted and the wrong one when a user task did: that task has to
   die while the machine carries on. Both kinds therefore go through the one path
   that can hand a different context back. Generated with .rept, so no stub carries a label of its
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
   the `call`, which is what the SysV ABI requires of us.

   Those 22 qwords are the whole of an interrupted context, laid out
   contiguously on the interrupted stack — which is why the dispatcher is handed
   `rsp` and its return value is loaded back into `rsp`. Returning a different
   frame's address resumes a different task; returning the one it was given
   resumes the task that was interrupted. That is the entire context switch. */
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
    mov rsi, rsp            /* the interrupted context, all 22 qwords of it */
    call irq_dispatch
    mov rsp, rax            /* whichever context the dispatcher chose to resume */
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

/// The IDT. Written once by `init` on the boot CPU before interrupts are
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

/// The vector a user task uses to ask the kernel for something. It is the only
/// gate in the table a CPL3 task may take: every other one is DPL0, so `int` on
/// any of them from ring 3 is a general-protection fault rather than a way in.
pub(crate) const VEC_SYSCALL: usize = 0x80;

fn gate(addr: u64, dpl: u8) -> IdtEntry {
    IdtEntry {
        off_lo: addr as u16,
        selector: gdt::KERNEL_CS,
        ist: 0,
        // present, 64-bit interrupt gate, callable from `dpl` and below
        attr: 0x8E | (dpl << 5),
        off_mid: (addr >> 16) as u16,
        off_hi: (addr >> 32) as u32,
        zero: 0,
    }
}

pub(crate) fn init() {
    unsafe {
        let base = IDT.get() as *mut IdtEntry;
        for (i, &addr) in isr_table.iter().enumerate() {
            core::ptr::write(base.add(i), gate(addr, 0));
        }
        let ext = core::ptr::addr_of!(isr_ext_stubs) as u64;
        for i in 32..IDT_LEN {
            let addr = ext + ((i - 32) * EXT_STUB_STRIDE) as u64;
            let dpl = if i == VEC_SYSCALL { 3 } else { 0 };
            core::ptr::write(base.add(i), gate(addr, dpl));
        }
        let ptr = IdtPtr {
            limit: (core::mem::size_of::<[IdtEntry; IDT_LEN]>() - 1) as u16,
            base: base as u64,
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

/// A CPU exception. Who faulted decides what it costs.
fn exception(vector: u64, frame: u64) -> u64 {
    let (error, rip, cs) = unsafe {
        (
            sched::frame_get(frame, sched::FRAME_ERR),
            sched::frame_get(frame, sched::FRAME_RIP),
            sched::frame_get(frame, sched::FRAME_CS),
        )
    };
    let name = EXC_NAMES.get(vector as usize).copied().unwrap_or("?");

    // A fault at CPL3 is the task's own doing, and costs the task rather than
    // the machine. Printing here is safe for the same reason the syscall path
    // is: the only code that prints runs on the boot task, and the boot task is
    // not the one that faulted.
    if cs & 3 == 3 {
        print("\n[trap] task ");
        print_i64(sched::current() as i64);
        print(" faulted at CPL3: ");
        print(name);
        print(" touching ");
        print_hex(paging::fault_address());
        print(", rip=");
        print_hex(rip);
        print(", error=");
        print_hex(error);
        print("\n[trap] killing the task; the machine keeps running.\n");
        return sched::kill_current(frame);
    }

    print("\n[trap] CPU exception ");
    print_i64(vector as i64);
    print(" (");
    print(name);
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
/// `frame` is the interrupted context's `rsp`; the returned value is loaded back
/// into `rsp` before the stub restores registers, so returning `frame` resumes
/// the interrupted task and returning another task's saved frame resumes that
/// one instead.
///
/// Everything this returns from resumes some instruction stream, so it must stay
/// short and must not print on the normal path: printing walks the VGA cursor,
/// and a handler that printed could interleave with a print the interrupted code
/// was halfway through. The one path that does print is the one that never
/// returns.
#[no_mangle]
extern "C" fn irq_dispatch(vector: u64, frame: u64) -> u64 {
    if vector < 32 {
        return exception(vector, frame);
    }
    if vector == timer::VEC_TIMER as u64 {
        timer::on_tick();
        return sched::on_tick(frame);
    }
    if vector == timer::VEC_SPURIOUS as u64 {
        timer::on_spurious();
        return frame;
    }
    if vector == VEC_SYSCALL as u64 {
        return sched::on_syscall(frame);
    }
    print("\n[trap] interrupt vector ");
    print_i64(vector as i64);
    print(" arrived with no handler installed.\n");
    print("[trap] halting (no ambient recovery).\n");
    loop {
        unsafe { asm!("hlt") };
    }
}

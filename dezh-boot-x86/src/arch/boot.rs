//! The boot trampoline: 32-bit protected mode -> identity paging -> long mode.
//!
//! Both x86 boot paths land in `_start` below in 32-bit protected mode — QEMU's
//! `-kernel` through the PVH note, and GRUB through the Multiboot2 header — so
//! everything here is shared by them. It identity-maps the first 2 MiB plus the
//! Local APIC window, switches on long mode, and calls `kmain`.

use core::arch::global_asm;

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

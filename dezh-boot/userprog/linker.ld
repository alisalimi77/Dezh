/* The user program is linked at its own virtual base (0x4000_0000). The kernel's
 * loader maps this VA range to freshly allocated frames in the process's private
 * address space; the kernel itself is mapped elsewhere (U=0) for trap handling. */
OUTPUT_ARCH("riscv")
ENTRY(_start)

SECTIONS
{
    . = 0x40000000;
    .text : {
        *(.text._start)
        *(.text .text.*)
    }
    . = ALIGN(8);
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) }
    . = ALIGN(8);
    .data : { *(.data .data.*) *(.sdata .sdata.*) }
    . = ALIGN(8);
    .bss : { *(.bss .bss.* .sbss .sbss.* COMMON) }
    . = ALIGN(8);
    __prog_end = .;
}

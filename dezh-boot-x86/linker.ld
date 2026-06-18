ENTRY(_start)

PHDRS {
    text PT_LOAD FLAGS(7);            /* rwx : the whole kernel image */
    note PT_NOTE FLAGS(4);            /* r-- : PVH boot note QEMU reads */
}

SECTIONS {
    . = 1M;                           /* loaded at 1 MiB */

    /* note lives inside the loaded image AND is exposed as a PT_NOTE */
    .note.Xen : { KEEP(*(.note.Xen)) } :text :note

    .boot   : { KEEP(*(.multiboot_header)) } :text
    .text   : { *(.text .text.*) } :text
    .rodata : { *(.rodata .rodata.*) } :text
    .data   : { *(.data .data.*) } :text
    .bss    : { *(.bss .bss.*) *(COMMON) } :text
}

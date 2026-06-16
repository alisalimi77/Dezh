/* RISC-V QEMU `virt` kernel layout.
 * OpenSBI (loaded by `-bios default`) hands control to S-mode at 0x8020_0000,
 * so the kernel image must start there. */
OUTPUT_ARCH("riscv")
ENTRY(_start)

BASE_ADDRESS = 0x80200000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        *(.text.entry)
        *(.text .text.*)
    }

    . = ALIGN(8);
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(8);
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    . = ALIGN(8);
    .bss : {
        __bss_start = .;
        *(.sbss .sbss.*)
        *(.bss .bss.* COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    . = ALIGN(16);
    . = . + 0x40000;            /* 256 KiB boot stack */
    __stack_top = .;
}

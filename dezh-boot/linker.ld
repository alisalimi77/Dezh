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

    /* User region: a 2 MiB-aligned span mapped U=1 (user-accessible) by the
     * page tables. Holds U-mode task code; the user stack grows down from the
     * top of the region. Everything else is supervisor-only. */
    . = ALIGN(0x200000);
    __user_start = .;
    .user : {
        *(.user.text .user.text.*)
        *(.user.rodata .user.rodata.*)
    }
    . = ALIGN(0x200000);
    __user_end = .;
}

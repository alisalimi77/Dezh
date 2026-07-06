/* Link the guest at 0x4000_0000. Dezh keeps a low identity gigapage (0..1 GiB,
 * U=0) for kernel MMIO, so a user image must live above it; 0x4000_0000 (VPN2=1)
 * is the private user region the loader maps per process — the same base the
 * native `userprog` uses. A static Linux ET_EXEC is free to be linked here and
 * still loads and runs on real riscv64 Linux. */
OUTPUT_ARCH("riscv")
ENTRY(_start)

SECTIONS
{
    . = 0x40000000;
    .text : {
        *(.text._start)
        *(.text .text.*)
    }
    /* Page-align each PT_LOAD so no two segments with different permissions
     * share a page. A real Linux ELF loader mmaps each segment independently;
     * when .text (R+E) and .rodata (R) overlap one page, the shared page ends
     * up R-only and the entry point faults with SEGV_ACCERR. Page alignment
     * keeps the code page executable on both real riscv64 Linux and Dezh's
     * per-page (W^X) loader. */
    . = ALIGN(0x1000);
    .rodata : { *(.rodata .rodata.*) *(.srodata .srodata.*) }
    . = ALIGN(0x1000);
    .data : { *(.data .data.*) *(.sdata .sdata.*) }
    . = ALIGN(8);
    .bss : { *(.bss .bss.* .sbss .sbss.* COMMON) }
    . = ALIGN(8);
    __prog_end = .;
}

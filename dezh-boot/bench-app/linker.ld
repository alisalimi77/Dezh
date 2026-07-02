ENTRY(_start)

SECTIONS
{
  . = 0x40000000;
  .text : {
    *(.text._start)
    *(.text .text.*)
  }
  .rodata : { *(.rodata .rodata.*) }
  .data : { *(.data .data.*) }
  .bss : {
    *(.bss .bss.*)
    *(COMMON)
  }
}

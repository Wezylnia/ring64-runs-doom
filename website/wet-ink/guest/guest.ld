ENTRY(step)
SECTIONS {
  . = 0x1000;
  .guest : { *(.text.step) *(.text .text.*) *(.rodata .rodata.*) }
  /DISCARD/ : { *(.eh_frame .comment .note*) }
}

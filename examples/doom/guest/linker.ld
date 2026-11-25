/* Guest memory map (linear, long mode):
 *   0x00400000  code + rodata    (read/execute)
 *   0x01000000  data, bss, heap, stack (read/write, RAM_SIZE bytes)
 *   0x10000000  doom1.wad        (read only, mapped by the host)
 */
ENTRY(_start)

RAM_BASE = 0x01000000;
RAM_SIZE = 0x04000000;   /* 64 MiB */
STACK_SIZE = 0x00100000; /* 1 MiB */

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = 0x00400000;
    .text : {
        KEEP(*(.text.hyper))
        KEEP(*(.text.start))
        *(.text .text.*)
    } :text
    .rodata : { *(.rodata .rodata.*) } :text
    /DISCARD/ : { *(.eh_frame*) *(.note*) *(.comment) }

    . = RAM_BASE;
    .data : { *(.data .data.*) } :data
    .bss : {
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        __bss_end = .;
    } :data
    . = ALIGN(4096);
    __heap_start = .;
    __stack_top = RAM_BASE + RAM_SIZE;
    __heap_end = __stack_top - STACK_SIZE;
}

ASSERT(ring64_hypercall == 0x400000, "hypercall must be the first code byte")

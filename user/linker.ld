/*
 * User program layout. Separate program headers keep code read+execute,
 * constants read-only and data read+write; the kernel refuses to load any
 * segment that is both writable and executable.
 */
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);   /* r-x */
    rodata PT_LOAD FLAGS(4); /* r-- */
    data PT_LOAD FLAGS(6);   /* rw- */
}

SECTIONS
{
    . = 0x400000;
    .text : { *(.text.entry) *(.text .text.*) } :text
    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata
    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .bss : { *(COMMON) *(.bss .bss.*) } :data
    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}

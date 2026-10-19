/*
 * Kernel link layout: linked in the top 2 GiB of the address space
 * (-mcmodel=kernel). The loader places it anywhere in physical memory and
 * maps it here. Each section is page-aligned
 * so vmm.c can give it its own permissions (text r-x, rodata r--, data rw-).
 */
ENTRY(_start)

KERNEL_VMA = 0xFFFFFFFF80000000;

SECTIONS
{
    . = KERNEL_VMA + 0x100000;

    __text_start = .;
    .text : { *(.text.entry) *(.text .text.*) }
    . = ALIGN(4096);
    __text_end = .;

    __rodata_start = .;
    .rodata : { *(.rodata .rodata.*) }
    . = ALIGN(4096);
    __rodata_end = .;

    __data_start = .;
    .data : { *(.data .data.*) }
    .bss : { *(COMMON) *(.bss .bss.*) }
    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}

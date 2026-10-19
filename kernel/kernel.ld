/* SPDX-License-Identifier: Apache-2.0 */
/*
 * The loader loads the kernel at its linked physical address. Keep the image
 * below 4 GiB so the loader can reach it with 32-bit firmware structures and
 * the kernel can use small code-model relocations.
 */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

SECTIONS
{
    . = 0x04000000;
    __kernel_start = .;

    .text : ALIGN(4096) {
        __text_start = .;
        KEEP(*(.text.entry))
        *(.text .text.*)
        __text_end = .;
    }

    .rodata : ALIGN(4096) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    }

    .data : ALIGN(4096) {
        __data_start = .;
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        *(.bss .bss.*)
        *(COMMON)
    }

    __kernel_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}

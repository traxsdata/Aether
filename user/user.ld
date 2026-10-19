/* SPDX-License-Identifier: Apache-2.0 */
/*
 * User programs are linked at the base of the user window. The kernel copies
 * PT_LOAD segments into frames mapped at these virtual addresses and marks the
 * pages user-accessible; the kernel identity map remains supervisor-only.
 */
OUTPUT_FORMAT(elf64-x86-64)
ENTRY(_start)

SECTIONS
{
    . = 0x0000020000000000;
    __user_start = .;

    .text : ALIGN(4096) {
        __user_text_start = .;
        KEEP(*(.text.entry))
        *(.text .text.*)
        __user_text_end = .;
    }

    .rodata : ALIGN(4096) {
        . = ALIGN(4096);
        __user_rodata_start = .;
        *(.rodata .rodata.*)
        __user_rodata_end = .;
    }

    .data : ALIGN(4096) {
        . = ALIGN(4096);
        __user_data_start = .;
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        . = ALIGN(4096);
        __user_bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __user_bss_end = .;
    }

    __user_end = .;

    .got : ALIGN(8) {
        *(.got)
        *(.got.plt)
    }

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}

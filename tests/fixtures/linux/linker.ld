/* SPDX-License-Identifier: Apache-2.0 */
/* Link a freestanding Linux test program inside Aether's user window. */
ENTRY(_start)
PHDRS
{
    all PT_LOAD FLAGS(5);
}
SECTIONS
{
    . = 0x20000010000;
    .text : { *(.text*) } :all
    .rodata : { *(.rodata*) *(.eh_frame*) } :all
    .data : { *(.data*) *(.got*) } :all
    .bss : { *(.bss*) *(COMMON) } :all
}

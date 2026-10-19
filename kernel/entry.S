/*
 * entry.S - first kernel instructions, running at the higher-half address
 * on the loader's page tables. RDI = physical address of struct bootinfo.
 * Switch to our own stack and call kmain(bootinfo_phys).
 */
    .section .text.entry
    .global _start
_start:
    cli
    lea boot_stack_top(%rip), %rsp
    xor %rbp, %rbp
    call kmain
1:  cli
    hlt
    jmp 1b

    .section .bss
    .balign 16
boot_stack:
    .skip 64 * 1024
boot_stack_top:

/* apps.S - embed the native user programs (built first by the Makefile) in the kernel image. */
    .section .rodata
    .balign 16
.macro APP name, path
    .global app_\name\()_start
    .global app_\name\()_end
app_\name\()_start:
    .incbin "\path"
app_\name\()_end:
    .balign 16
.endm
    APP terminal, "build/user/terminal.elf"
    APP calculator, "build/user/calculator.elf"
    APP notes, "build/user/notes.elf"
    APP probe, "build/user/probe.elf"
    APP explorer, "build/user/explorer.elf"

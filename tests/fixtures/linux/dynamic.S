/* SPDX-License-Identifier: Apache-2.0 */
/* Minimal dynamically linked Linux program for the loader boot test.
 *
 * The loader enters the interpreter first. The interpreter validates the
 * auxv, prints its marker, then jumps to AT_ENTRY, which runs this code.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea message(%rip), %rsi
    mov $message_len, %rdx
    syscall

    mov $60, %rax               /* exit */
    xor %rdi, %rdi
    syscall
1:
    jmp 1b

    .section .rodata
message:
    .ascii "AETHER_LINUX_DYN_MAIN_OK\n"
    .set message_len, . - message

    .section .note.GNU-stack,"",@progbits

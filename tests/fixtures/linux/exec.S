/* SPDX-License-Identifier: Apache-2.0 */
/* Static Linux image used to verify execve replaces the current process. */
    .section .text
    .global _start
    .type _start, @function
_start:
    mov $1, %rax
    mov $1, %rdi
    lea message(%rip), %rsi
    mov $message_len, %rdx
    syscall

    mov $60, %rax
    xor %rdi, %rdi
    syscall
1:
    jmp 1b

    .section .rodata
message:
    .ascii "AETHER_LINUX_EXECVE_OK image=ok\n"
    .set message_len, . - message

    .section .note.GNU-stack,"",@progbits

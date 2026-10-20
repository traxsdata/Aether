/* SPDX-License-Identifier: Apache-2.0 */
/* Minimal static Linux x86-64 program used by the Linux ABI boot test.
 *
 * It issues the two syscalls a freestanding program needs to prove that the
 * kernel's Linux entry path works: write(1, ...) and exit(0). It is built with
 * GNU as/ld, not the Rust Linux target, so CI does not need another toolchain.
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
    .ascii "AETHER_LINUX_EXEC_OK source=static-linux-elf\n"
    .set message_len, . - message

    .section .note.GNU-stack,"",@progbits

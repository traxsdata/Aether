/* SPDX-License-Identifier: Apache-2.0 */
/* Minimal Linux ELF interpreter for the dynamic-loader boot test.
 *
 * It walks the initial stack, requires AT_PHDR, AT_PHENT, AT_PHNUM, AT_ENTRY,
 * and AT_BASE, reports success, and jumps to the main image's entry point.
 * Relocations and shared-library loading are a later interpreter milestone.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    mov (%rsp), %rax            /* argc */
    lea 8(%rsp), %rbx           /* argv */
    lea 16(%rsp,%rax,8), %rcx   /* envp = argv + (argc + 1) * 8 */
1:
    cmpq $0, (%rcx)
    je 2f
    add $8, %rcx
    jmp 1b
2:
    add $8, %rcx                /* auxv */
    xor %r12, %r12              /* AT_PHDR */
    xor %r13, %r13              /* AT_PHENT */
    xor %r14, %r14              /* AT_PHNUM */
    xor %r15, %r15              /* AT_ENTRY */
    xor %rbx, %rbx              /* AT_BASE */
3:
    mov (%rcx), %rax
    test %rax, %rax
    jz 4f
    mov 8(%rcx), %rdx
    cmp $3, %rax
    jne 5f
    mov %rdx, %r12
5:
    cmp $4, %rax
    jne 6f
    mov %rdx, %r13
6:
    cmp $5, %rax
    jne 7f
    mov %rdx, %r14
7:
    cmp $7, %rax
    jne 8f
    mov %rdx, %rbx
8:
    cmp $9, %rax
    jne 9f
    mov %rdx, %r15
9:
    add $16, %rcx
    jmp 3b
4:
    test %r12, %r12
    jz fail
    test %r13, %r13
    jz fail
    test %r14, %r14
    jz fail
    test %r15, %r15
    jz fail
    test %rbx, %rbx
    jz fail

    mov $1, %rax                /* write */
    mov $1, %rdi
    lea message(%rip), %rsi
    mov $message_len, %rdx
    syscall

    jmp *%r15                   /* enter the main image */

fail:
    mov $1, %rax
    mov $1, %rdi
    lea failure(%rip), %rsi
    mov $failure_len, %rdx
    syscall
    mov $60, %rax
    mov $1, %rdi
    syscall
1:
    jmp 1b

    .section .rodata
message:
    .ascii "AETHER_LINUX_DYN_INTERP_OK\n"
    .set message_len, . - message
failure:
    .ascii "AETHER_LINUX_DYN_INTERP_FAIL\n"
    .set failure_len, . - failure

    .section .note.GNU-stack,"",@progbits

/* SPDX-License-Identifier: Apache-2.0 */
/* ET_DYN Linux image validates bounded execve argv/envp at its initial stack. */
    .section .text
    .global _start
    .type _start, @function
_start:
    /* Exactly two argv pointers, followed by the NULL terminator. */
    cmpq $2, (%rsp)
    jne fail
    cmpq $0, 8(%rsp)
    je fail
    mov 16(%rsp), %r13
    movabs $0x00656e6f2d677261, %rax /* "arg-one\0" */
    cmp %rax, (%r13)
    jne fail
    cmpq $0, 24(%rsp)
    jne fail

    /* One environment entry, with a fixed value and terminating NULL. */
    mov 32(%rsp), %r13
    movabs $0x455f524548544541, %rax /* "AETHER_E" */
    cmp %rax, (%r13)
    jne fail
    movabs $0x3d564e455f434558, %rax /* "XEC_ENV=" */
    cmp %rax, 8(%r13)
    jne fail
    movabs $0x00746e6573657270, %rax /* "present\0" */
    cmp %rax, 16(%r13)
    jne fail
    cmpq $0, 40(%rsp)
    jne fail

    mov $1, %rax
    mov $1, %rdi
    lea success(%rip), %rsi
    mov $success_len, %rdx
    syscall
    mov $60, %rax
    xor %rdi, %rdi
    syscall
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
success:
    .ascii "AETHER_LINUX_EXECVE_DYN_OK argv=ok envp=ok\n"
    .set success_len, . - success
failure:
    .ascii "AETHER_LINUX_EXECVE_DYN_FAIL argv-envp\n"
    .set failure_len, . - failure
    .section .note.GNU-stack,"",@progbits

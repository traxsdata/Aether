/* SPDX-License-Identifier: Apache-2.0 */
/* Static Linux image used to verify execve replaces the current process. */
    .section .text
    .global _start
    .type _start, @function
_start:
    /* execve must carry argv and envp into the new process image. */
    cmpq $2, (%rsp)
    jne fail
    mov 16(%rsp), %r12
    movabs $0x00656e6f2d677261, %rax /* "arg-one\0" */
    cmp %rax, (%r12)
    jne fail
    cmpq $0, 24(%rsp)               /* argv terminator */
    jne fail
    mov 32(%rsp), %r13               /* envp[0] */
    movabs $0x455f524548544541, %rax /* "AETHER_E" */
    cmp %rax, (%r13)
    jne fail
    movabs $0x3d564e455f434558, %rax /* "XEC_ENV=" */
    cmp %rax, 8(%r13)
    jne fail
    movabs $0x00746e6573657270, %rax /* "present\0" */
    cmp %rax, 16(%r13)
    jne fail
    cmpq $0, 40(%rsp)               /* envp terminator */
    jne fail

    /* Record the validated static exec, then chain into the ET_DYN image. */
    mov $1, %rax
    mov $1, %rdi
    lea message(%rip), %rsi
    mov $message_len, %rdx
    syscall

    mov $59, %rax
    lea dynamic_path(%rip), %rdi
    lea dynamic_argv(%rip), %rsi
    lea dynamic_envp(%rip), %rdx
    syscall
    jmp fail

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
message:
    .ascii "AETHER_LINUX_EXECVE_OK image=static argv=ok envp=ok\n"
    .set message_len, . - message
dynamic_path:
    .asciz "/usr/bin/dynamic-exec"
dynamic_arg0:
    .asciz "/usr/bin/dynamic-exec"
dynamic_arg1:
    .asciz "arg-one"
dynamic_env:
    .asciz "AETHER_EXEC_ENV=present"
    .align 8
dynamic_argv:
    .quad dynamic_arg0, dynamic_arg1, 0
dynamic_envp:
    .quad dynamic_env, 0
failure:
    .ascii "AETHER_LINUX_EXECVE_FAIL argv-envp\n"
    .set failure_len, . - failure

    .section .note.GNU-stack,"",@progbits

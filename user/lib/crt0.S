/* crt0.S - program entry: seed the stack canary, call main(arg), exit with its result. */
    .section .text.entry
    .global _start
_start:
    and $-16, %rsp
    push %rdi
    rdtsc
    shl $32, %rdx
    or %rdx, %rax
    movabs $0x9E3779B97F4A7C15, %rcx
    imul %rcx, %rax
    mov %rax, __stack_chk_guard(%rip)
    pop %rdi
    call main               /* rsp is 16-byte aligned here, as the ABI requires */
    mov %eax, %edi
    xor %eax, %eax        /* SYS_EXIT */
    syscall
1:  jmp 1b

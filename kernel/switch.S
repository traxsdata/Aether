/*
 * switch.S - kernel stack switching and the syscall entry point.
 */
    .text

/* void switch_to(uint64_t *save_rsp, uint64_t new_rsp)
 * Saves callee-saved registers on the current kernel stack, switches stacks,
 * and restores the other task's registers. Returns into whatever the other
 * task was doing (or into isr_return for a brand-new process). */
    .global switch_to
switch_to:
    pushq %rbp
    pushq %rbx
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    mov %rsp, (%rdi)
    mov %rsi, %rsp
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %rbx
    popq %rbp
    ret

/*
 * syscall_entry: user code executes `syscall`. The CPU put the user RIP in
 * RCX and RFLAGS in R11, and cleared IF, DF, TF and AC (SFMASK), but did
 * NOT switch stacks. Move to this process's kernel stack before touching
 * anything, then build the same struct isr_frame interrupts use, so
 * scheduling and process creation have a single frame format. Return with
 * iretq rather than sysretq, which sidesteps sysret's non-canonical-RIP pitfall.
 */
    .global syscall_entry
syscall_entry:
    mov %rsp, syscall_user_rsp(%rip)
    mov kernel_stack_current(%rip), %rsp
    pushq $0x1B                       /* ss: user data */
    pushq syscall_user_rsp(%rip)      /* rsp */
    pushq %r11                        /* rflags */
    pushq $0x23                       /* cs: user code */
    pushq %rcx                        /* rip */
    pushq $0                          /* error code */
    pushq $0x80                       /* vector: syscall marker */
    pushq %rax
    pushq %rbx
    pushq %rcx
    pushq %rdx
    pushq %rsi
    pushq %rdi
    pushq %rbp
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    pushq %r12
    pushq %r13
    pushq %r14
    pushq %r15
    mov %rsp, %rdi
    call syscall_dispatch
    jmp isr_return

    .bss
    .balign 8
    .global kernel_stack_current
kernel_stack_current:
    .quad 0
syscall_user_rsp:
    .quad 0

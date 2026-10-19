/*
 * isr.S - interrupt entry stubs.
 *
 * Every vector gets a stub that pushes an error code (a dummy 0 if the CPU
 * doesn't push one) and the vector number, then jumps to isr_common, which
 * saves all general registers into a struct isr_frame and calls
 * isr_dispatch(frame). isr_return is also where newly created processes
 * "return" to, so they enter user mode through the same iretq.
 */
    .text

/* Vectors where the CPU pushes an error code itself: 8, 10-14, 17, 21, 29, 30.
 * Every other vector gets a dummy 0 so the frame layout is always the same. */
.macro ISR_NOERR n
    .balign 16
isr_stub_\n:
    pushq $0
    pushq $\n
    jmp isr_common
.endm

.macro ISR_ERR n
    .balign 16
isr_stub_\n:
    pushq $\n
    jmp isr_common
.endm

.irp n,0,1,2,3,4,5,6,7,9,15,16,18,19,20,22,23,24,25,26,27,28,31,32,33,34,35,36,37,38,39,40,41
    ISR_NOERR \n
.endr
.irp n,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73
    ISR_NOERR \n
.endr
.irp n,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105
    ISR_NOERR \n
.endr
.irp n,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127,128,129,130,131,132,133,134,135,136,137
    ISR_NOERR \n
.endr
.irp n,138,139,140,141,142,143,144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159,160,161,162,163,164,165,166,167,168,169
    ISR_NOERR \n
.endr
.irp n,170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,190,191,192,193,194,195,196,197,198,199,200,201
    ISR_NOERR \n
.endr
.irp n,202,203,204,205,206,207,208,209,210,211,212,213,214,215,216,217,218,219,220,221,222,223,224,225,226,227,228,229,230,231,232,233
    ISR_NOERR \n
.endr
.irp n,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255
    ISR_NOERR \n
.endr
.irp n,8,10,11,12,13,14,17,21,29,30
    ISR_ERR \n
.endr

    .section .rodata
    .global isr_stub_table
isr_stub_table:
.irp n,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31
    .quad isr_stub_\n
.endr
.irp n,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59,60,61,62,63
    .quad isr_stub_\n
.endr
.irp n,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95
    .quad isr_stub_\n
.endr
.irp n,96,97,98,99,100,101,102,103,104,105,106,107,108,109,110,111,112,113,114,115,116,117,118,119,120,121,122,123,124,125,126,127
    .quad isr_stub_\n
.endr
.irp n,128,129,130,131,132,133,134,135,136,137,138,139,140,141,142,143,144,145,146,147,148,149,150,151,152,153,154,155,156,157,158,159
    .quad isr_stub_\n
.endr
.irp n,160,161,162,163,164,165,166,167,168,169,170,171,172,173,174,175,176,177,178,179,180,181,182,183,184,185,186,187,188,189,190,191
    .quad isr_stub_\n
.endr
.irp n,192,193,194,195,196,197,198,199,200,201,202,203,204,205,206,207,208,209,210,211,212,213,214,215,216,217,218,219,220,221,222,223
    .quad isr_stub_\n
.endr
.irp n,224,225,226,227,228,229,230,231,232,233,234,235,236,237,238,239,240,241,242,243,244,245,246,247,248,249,250,251,252,253,254,255
    .quad isr_stub_\n
.endr

    .text
    .global isr_common
isr_common:
    cld
    /* The user can set AC (SMAP's override) with POPF. Clear it before any
     * C code runs; STAC is only allowed inside copy_in/copy_out. */
    pushfq
    andq $-262145, (%rsp) /* ~X86_EFLAGS_AC */
    popfq
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
    call isr_dispatch

    .global isr_return
isr_return:
    popq %r15
    popq %r14
    popq %r13
    popq %r12
    popq %r11
    popq %r10
    popq %r9
    popq %r8
    popq %rbp
    popq %rdi
    popq %rsi
    popq %rdx
    popq %rcx
    popq %rbx
    popq %rax
    add $16, %rsp /* vector and error code */
    iretq

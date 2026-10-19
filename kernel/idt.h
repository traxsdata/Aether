/* idt.h - interrupt descriptor table and the register frame every entry pushes. */
#pragma once
#include <stdint.h>

/* Layout matches isr.S: pushed GPRs, then vector/error, then the CPU's iret frame. */
struct isr_frame {
    uint64_t r15, r14, r13, r12, r11, r10, r9, r8;
    uint64_t rbp, rdi, rsi, rdx, rcx, rbx, rax;
    uint64_t vector, error;
    uint64_t rip, cs, rflags, rsp, ss;
};

#define VEC_IRQ0 32
#define VEC_SYSCALL 0x80 /* marker only: syscalls arrive via the syscall instruction */

void idt_init(void);
void isr_dispatch(struct isr_frame *f);

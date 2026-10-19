/* linux.h - the Linux system call layer, for apps built as static Linux programs. */
#pragma once
#include <stdint.h>

struct proc;
struct program;
struct elf_info;
struct isr_frame;

/* Builds the initial stack (argv, environment, auxiliary vector) and the
 * heap for a freshly loaded Linux program in p's address space. */
int linux_setup(struct proc *p, const struct program *prog, const struct elf_info *ei, uint64_t arg, uint64_t *rsp);
/* Handles one Linux system call for the current thread: the result goes in f->rax. */
void linux_syscall(struct isr_frame *f);

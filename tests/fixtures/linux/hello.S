/* SPDX-License-Identifier: Apache-2.0 */
/* Minimal static Linux x86-64 program used by the Linux ABI boot test.
 *
 * It exercises the syscall entry, anonymous mmap, FS-based TLS setup, and the
 * two calls a freestanding program needs to report and terminate. Any failed
 * step writes a failure marker and exits nonzero, which fails the boot gate.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    /* mmap(NULL, 4096, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS,
     *      -1, 0) */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_mmap
    mov %rax, %r12

    /* The arena is writable anonymous memory. */
    movabs $0x1122334455667788, %rax
    mov %rax, (%r12)
    mov (%r12), %rbx
    movabs $0x1122334455667788, %rax
    cmp %rax, %rbx
    jne fail_mmap

    /* arch_prctl(ARCH_SET_FS, mapping); then address it through %fs. */
    mov $158, %rax
    mov $0x1002, %rdi
    mov %r12, %rsi
    syscall
    test %rax, %rax
    js fail_fs
    movq $0x99, %fs:8
    cmpq $0x99, %fs:8
    jne fail_fs

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

fail_mmap:
    lea mmap_failure(%rip), %rsi
    mov $mmap_failure_len, %rdx
    jmp report_failure
fail_fs:
    lea fs_failure(%rip), %rsi
    mov $fs_failure_len, %rdx
report_failure:
    mov $1, %rax
    mov $1, %rdi
    syscall
    mov $60, %rax
    mov $1, %rdi
    syscall
2:
    jmp 2b

    .section .rodata
message:
    .ascii "AETHER_LINUX_EXEC_OK source=static-linux-elf mmap=ok fs=ok\n"
    .set message_len, . - message
mmap_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=mmap\n"
    .set mmap_failure_len, . - mmap_failure
fs_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=fs\n"
    .set fs_failure_len, . - fs_failure

    .section .note.GNU-stack,"",@progbits

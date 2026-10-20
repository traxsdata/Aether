/* SPDX-License-Identifier: Apache-2.0 */
/* Static Linux fixture for the Gate 4 file-portal slice.
 *
 * It opens the exact-file portal namespace, reads the workbook, saves new
 * bytes through the same grant, asks the test-only kernel path to revoke the
 * grant, and then proves that a fresh read and a fresh write both fail closed.
 * The kernel, not this program, emits the Gate 4 evidence markers after it
 * validates the read bytes, the saved bytes in the kernel filesystem, and the
 * two post-revocation denials.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    sub $4096, %rsp

    /* openat(AT_FDCWD, portal_path, O_RDONLY, 0) */
    mov $257, %rax
    mov $-100, %rdi
    lea portal_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    /* read(fd, stack_buffer, 4096) */
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $4096, %rdx
    syscall
    cmp $1, %rax
    jl fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* openat(AT_FDCWD, portal_path, O_WRONLY|O_TRUNC, 0) */
    mov $257, %rax
    mov $-100, %rdi
    lea portal_path(%rip), %rsi
    mov $0x201, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    /* write(fd, saved_payload, saved_len) */
    mov $1, %rax
    mov %r12, %rdi
    lea saved_payload(%rip), %rsi
    mov $saved_len, %rdx
    syscall
    cmp $saved_len, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* openat(AT_FDCWD, revoke_path, O_RDONLY, 0): test-only revoke action */
    mov $257, %rax
    mov $-100, %rdi
    lea revoke_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* A post-revocation read must fail with EACCES or ENOENT. */
    mov $257, %rax
    mov $-100, %rdi
    lea portal_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    cmp $-13, %rax
    je 1f
    cmp $-2, %rax
    jne fail
1:
    /* A post-revocation write/save must fail the same way. */
    mov $257, %rax
    mov $-100, %rdi
    lea portal_path(%rip), %rsi
    mov $0x201, %rdx
    xor %r10, %r10
    syscall
    cmp $-13, %rax
    je 2f
    cmp $-2, %rax
    jne fail
2:
    mov $60, %rax
    xor %rdi, %rdi
    syscall

fail:
    mov $60, %rax
    mov $1, %rdi
    syscall
1:
    jmp 1b

    .section .rodata
portal_path:
    .asciz "/run/aether/portal/dd8f879ba78d207c/gate4.xlsx"
revoke_path:
    .asciz "/run/aether/gate4/revoke"
saved_payload:
    .incbin "tests/fixtures/gate4/saved.csv"
    .set saved_len, . - saved_payload

    .section .note.GNU-stack,"",@progbits

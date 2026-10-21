/* SPDX-License-Identifier: Apache-2.0 */
/* Static Linux fixture for the Gate 4 file-portal slice.
 *
 * It reads the granted workbook, performs the same bounded staging sequence a
 * real graphical save uses (`<file>.sctmp` write, close, rename to `<file>`),
 * reads the final bytes back, revokes the grant through the test-only kernel
 * path, and proves that fresh reads and writes fail closed. The kernel, not
 * this program, emits the Gate 4 evidence markers after it validates the read
 * bytes, the staging/rename transaction, the saved bytes in the kernel
 * filesystem, and the two post-revocation denials.
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

    /* openat(AT_FDCWD, temp_path, O_WRONLY|O_CREAT|O_TRUNC, 0) */
    mov $257, %rax
    mov $-100, %rdi
    lea temp_path(%rip), %rsi
    mov $0x241, %rdx
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

    /* rename(temp_path, portal_path): commit the staged bytes */
    mov $82, %rax
    lea temp_path(%rip), %rdi
    lea portal_path(%rip), %rsi
    syscall
    test %rax, %rax
    js fail

    /* Re-open the final file and prove the committed byte count. */
    mov $257, %rax
    mov $-100, %rdi
    lea portal_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $4096, %rdx
    syscall
    cmp $saved_len, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* Stage a second, different save that must never be committed. */
    mov $257, %rax
    mov $-100, %rdi
    lea temp_path(%rip), %rsi
    mov $0x241, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $1, %rax
    mov %r12, %rdi
    lea doomed_payload(%rip), %rsi
    mov $doomed_len, %rdx
    syscall
    cmp $doomed_len, %rax
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

    /* A rename after revocation must fail without committing the staging. */
    mov $82, %rax
    lea temp_path(%rip), %rdi
    lea portal_path(%rip), %rsi
    syscall
    cmp $-13, %rax
    je 3f
    cmp $-2, %rax
    jne fail
3:

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
temp_path:
    .asciz "/run/aether/portal/dd8f879ba78d207c/gate4.xlsx.sctmp"
revoke_path:
    .asciz "/run/aether/gate4/revoke"
saved_payload:
    .incbin "tests/fixtures/gate4/saved.csv"
    .set saved_len, . - saved_payload
doomed_payload:
    .ascii "revoked-commit-must-not-land\n"
    .set doomed_len, . - doomed_payload

    .section .note.GNU-stack,"",@progbits

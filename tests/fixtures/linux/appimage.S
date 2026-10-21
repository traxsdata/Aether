/* SPDX-License-Identifier: Apache-2.0 */
/* Static program packaged in the user-supplied AppImage fixture.
 *
 * It writes one settings file and one log file, verifies that the third
 * write exceeds the per-application overlay quota with ENOSPC, and reports
 * success. The kernel additionally checks after this task exits that the
 * overlay entries still exist and then exercises the erasure path.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    /* openat(AT_FDCWD, "/settings/config", O_WRONLY|O_CREAT|O_TRUNC, 0600) */
    mov $257, %rax
    mov $-100, %rdi
    lea settings_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    /* write(fd, payload400, 400) */
    mov $1, %rax
    mov %r12, %rdi
    lea payload400(%rip), %rsi
    mov $400, %rdx
    syscall
    cmp $400, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* openat(AT_FDCWD, "/logs/run.log", ...) */
    mov $257, %rax
    mov $-100, %rdi
    lea logs_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $1, %rax
    mov %r12, %rdi
    lea payload400(%rip), %rsi
    mov $400, %rdx
    syscall
    cmp $400, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* Grow one file until the 4 KiB per-file quota denies a write. */
    mov $257, %rax
    mov $-100, %rdi
    lea large_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $12, %r13
1:
    mov $1, %rax
    mov %r12, %rdi
    lea payload400(%rip), %rsi
    mov $400, %rdx
    syscall
    cmp $-28, %rax                 /* -ENOSPC from the quota */
    je 2f
    cmp $400, %rax
    jne fail
    dec %r13
    jnz 1b
    jmp fail
2:
    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* Re-open the settings path read-only to prove the overlay mapping. */
    mov $257, %rax
    mov $-100, %rdi
    lea settings_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    sub $32, %rsp
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    cmp $16, %rax
    jne fail
    movabs $0x3434343434343434, %rax /* "44444444" */
    cmp %rax, (%rsp)
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

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
settings_path:
    .asciz "/settings/config"
logs_path:
    .asciz "/logs/run.log"
large_path:
    .asciz "/settings/large"
success:
    .ascii "AETHER_LINUX_APPIMAGE_EXEC_OK settings=ok logs=ok quota=ok\n"
    .set success_len, . - success
failure:
    .ascii "AETHER_LINUX_APPIMAGE_EXEC_FAIL\n"
    .set failure_len, . - failure
payload400:
    .rept 400
    .byte '4'
    .endr

    .section .note.GNU-stack,"",@progbits

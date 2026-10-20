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

    /* openat(AT_FDCWD, "/etc/motd", O_RDONLY, 0) */
    mov $257, %rax
    mov $-100, %rdi
    lea motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_file
    mov %rax, %r13

    /* read(fd, arena, 128) */
    mov $0, %rax
    mov %r13, %rdi
    mov %r12, %rsi
    mov $128, %rdx
    syscall
    test %rax, %rax
    jle fail_file
    mov %rax, %r14

    /* write(1, arena, bytes_read) */
    mov $1, %rax
    mov $1, %rdi
    mov %r12, %rsi
    mov %r14, %rdx
    syscall

    /* close(fd) */
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* openat(AT_FDCWD, "/tmp/out", O_WRONLY|O_CREAT|O_TRUNC, 0600) */
    mov $257, %rax
    mov $-100, %rdi
    lea overlay_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js fail_overlay
    mov %rax, %r13

    /* write the overlay content */
    mov $1, %rax
    mov %r13, %rdi
    lea overlay_content(%rip), %rsi
    mov $overlay_content_len, %rdx
    syscall
    test %rax, %rax
    js fail_overlay
    cmp $overlay_content_len, %rax
    jne fail_overlay

    /* close and reopen read-only */
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea overlay_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_overlay
    mov %rax, %r13

    /* read it back through the overlay */
    mov $0, %rax
    mov %r13, %rdi
    mov %r12, %rsi
    mov $128, %rdx
    syscall
    test %rax, %rax
    jle fail_overlay
    mov %rax, %r14
    mov $1, %rax
    mov $1, %rdi
    mov %r12, %rsi
    mov %r14, %rdx
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* File-backed mmap: map /etc/motd and check the first eight bytes. */
    mov $257, %rax
    mov $-100, %rdi
    lea motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_vma
    mov %rax, %r13

    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $1, %rdx                /* PROT_READ */
    mov $2, %r10                /* MAP_PRIVATE */
    mov %r13, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_vma
    mov %rax, %r15
    movabs $0x4c5f524548544541, %rax  /* "AETHER_L" */
    cmp %rax, (%r15)
    jne fail_vma

    /* munmap the file mapping, then MAP_FIXED an anonymous page over it. */
    mov $11, %rax
    mov %r15, %rdi
    mov $4096, %rsi
    syscall
    test %rax, %rax
    js fail_vma

    mov $9, %rax
    mov %r15, %rdi
    mov $4096, %rsi
    mov $3, %rdx                /* PROT_READ|PROT_WRITE */
    mov $0x32, %r10             /* PRIVATE|ANONYMOUS|FIXED */
    mov $-1, %r8
    xor %r9, %r9
    syscall
    cmp %r15, %rax
    jne fail_vma
    movq $0x5a, (%rax)
    cmpq $0x5a, (%rax)
    jne fail_vma
    mov %rax, %rdi
    mov $4096, %rsi
    mov $11, %rax
    syscall
    test %rax, %rax
    js fail_vma
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* Writable+executable anonymous memory must be rejected. */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $7, %rdx                /* PROT_READ|PROT_WRITE|PROT_EXEC */
    mov $0x22, %r10             /* PRIVATE|ANONYMOUS */
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    jns fail_vma

    /* A file mapping may be executable, but mprotect cannot add write. */
    mov $257, %rax
    mov $-100, %rdi
    lea motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_vma
    mov %rax, %r13
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $5, %rdx                /* PROT_READ|PROT_EXEC */
    mov $2, %r10
    mov %r13, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_vma
    mov %rax, %r15
    mov $10, %rax
    mov %r15, %rdi
    mov $4096, %rsi
    mov $6, %rdx                /* PROT_WRITE|PROT_EXEC */
    syscall
    test %rax, %rax
    jns fail_vma
    mov $11, %rax
    mov %r15, %rdi
    mov $4096, %rsi
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* The break grows inside the heap window and the new page is writable. */
    mov $12, %rax
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    jle fail_vma
    mov %rax, %r14
    lea 8192(%r14), %r15
    mov $12, %rax
    mov %r15, %rdi
    syscall
    cmp %r15, %rax
    jne fail_vma
    movb $0x5a, 8191(%r14)
    cmpb $0x5a, 8191(%r14)
    jne fail_vma

    sub $256, %rsp
    /* newfstatat(AT_FDCWD, "/etc/motd", &stat, 0) */
    mov $262, %rax
    mov $-100, %rdi
    lea motd_path(%rip), %rsi
    mov %rsp, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_meta
    cmpq $0, 48(%rsp)           /* st_size */
    jle fail_meta

    /* open, fstat, fcntl(F_GETFD), dup */
    mov $257, %rax
    mov $-100, %rdi
    lea motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_meta
    mov %rax, %r13
    mov $5, %rax                /* fstat */
    mov %r13, %rdi
    mov %rsp, %rsi
    syscall
    test %rax, %rax
    jne fail_meta
    cmpq $0, 48(%rsp)
    jle fail_meta
    mov $72, %rax               /* fcntl */
    mov %r13, %rdi
    mov $1, %rsi                /* F_GETFD */
    xor %rdx, %rdx
    syscall
    test %rax, %rax
    jne fail_meta
    mov $32, %rax               /* dup */
    mov %r13, %rdi
    syscall
    test %rax, %rax
    js fail_meta
    mov %rax, %r14
    mov $3, %rax
    mov %r14, %rdi
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* readlink("/proc/self/exe", stack, 64) */
    mov $89, %rax
    lea proc_exe(%rip), %rdi
    mov %rsp, %rsi
    mov $64, %rdx
    syscall
    test %rax, %rax
    jle fail_meta
    add $256, %rsp

    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea meta_message(%rip), %rsi
    mov $meta_message_len, %rdx
    syscall

    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea vma_message(%rip), %rsi
    mov $vma_message_len, %rdx
    syscall

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
    jmp report_failure
fail_file:
    lea file_failure(%rip), %rsi
    mov $file_failure_len, %rdx
    jmp report_failure
fail_overlay:
    lea overlay_failure(%rip), %rsi
    mov $overlay_failure_len, %rdx
    jmp report_failure
fail_vma:
    lea vma_failure(%rip), %rsi
    mov $vma_failure_len, %rdx
    jmp report_failure
fail_meta:
    lea meta_failure(%rip), %rsi
    mov $meta_failure_len, %rdx
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
    .ascii "AETHER_LINUX_EXEC_OK source=static-linux-elf mmap=ok fs=ok overlay=ok\n"
    .set message_len, . - message
vma_message:
    .ascii "AETHER_LINUX_VMA_OK file=ok fixed=ok wx=deny brk=ok\n"
    .set vma_message_len, . - vma_message
vma_failure:
    .ascii "AETHER_LINUX_VMA_FAIL\n"
    .set vma_failure_len, . - vma_failure
meta_message:
    .ascii "AETHER_LINUX_FILEMETA_OK stat=ok fd=ok dup=ok link=ok\n"
    .set meta_message_len, . - meta_message
meta_failure:
    .ascii "AETHER_LINUX_FILEMETA_FAIL\n"
    .set meta_failure_len, . - meta_failure
proc_exe:
    .asciz "/proc/self/exe"
mmap_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=mmap\n"
    .set mmap_failure_len, . - mmap_failure
fs_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=fs\n"
    .set fs_failure_len, . - fs_failure
file_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=file\n"
    .set file_failure_len, . - file_failure
motd_path:
    .asciz "/etc/motd"
overlay_path:
    .asciz "/tmp/out"
overlay_failure:
    .ascii "AETHER_LINUX_EXEC_FAIL stage=overlay\n"
    .set overlay_failure_len, . - overlay_failure
overlay_content:
    .ascii "AETHER_LINUX_OVERLAY_OK source=/tmp/out\n"
    .set overlay_content_len, . - overlay_content

    .section .note.GNU-stack,"",@progbits

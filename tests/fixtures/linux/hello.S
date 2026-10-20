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

    sub $512, %rsp
    /* openat(AT_FDCWD, "/", O_RDONLY|O_DIRECTORY) */
    mov $257, %rax
    mov $-100, %rdi
    lea root_path(%rip), %rsi
    mov $0x10000, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_dir
    mov %rax, %r13
    mov $217, %rax              /* getdents64 */
    mov %r13, %rdi
    mov %rsp, %rsi
    mov $512, %rdx
    syscall
    test %rax, %rax
    jle fail_dir
    mov $3, %rax
    mov %r13, %rdi
    syscall

    /* openat(AT_FDCWD, "/usr/bin", O_RDONLY|O_DIRECTORY) */
    mov $257, %rax
    mov $-100, %rdi
    lea usrbin_path(%rip), %rsi
    mov $0x10000, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_dir
    mov %rax, %r13
    mov $217, %rax
    mov %r13, %rdi
    mov %rsp, %rsi
    mov $512, %rdx
    syscall
    test %rax, %rax
    jle fail_dir
    mov $3, %rax
    mov %r13, %rdi
    syscall
    add $512, %rsp

    sub $64, %rsp
    /* pipe2(fds, 0) */
    mov $293, %rax
    mov %rsp, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_pipe
    mov (%rsp), %r13d           /* read fd */
    mov 4(%rsp), %r14d          /* write fd */
    mov $1, %rax                /* write "ping" */
    mov %r14, %rdi
    lea pipe_data(%rip), %rsi
    mov $4, %rdx
    syscall
    cmp $4, %rax
    jne fail_pipe
    mov %r13d, 16(%rsp)         /* pollfd.fd */
    movw $1, 20(%rsp)           /* POLLIN */
    movw $0, 22(%rsp)
    mov $7, %rax                /* poll */
    lea 16(%rsp), %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    cmp $1, %rax
    jne fail_pipe
    cmpw $1, 22(%rsp)
    jne fail_pipe
    mov $0, %rax                /* read back */
    mov %r13, %rdi
    lea 24(%rsp), %rsi
    mov $4, %rdx
    syscall
    cmp $4, %rax
    jne fail_pipe
    cmpl $0x676e6970, 24(%rsp)  /* "ping" */
    jne fail_pipe
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $3, %rax
    mov %r14, %rdi
    syscall
    add $64, %rsp

    sub $64, %rsp
    movq $1, 0(%rsp)            /* eventfd initial value */
    mov $290, %rax              /* eventfd2 */
    xor %rdi, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    js fail_event
    mov %rax, %r13
    mov $1, %rax                /* write value 1 */
    mov %r13, %rdi
    mov %rsp, %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_event
    mov %r13d, 16(%rsp)
    movw $1, 20(%rsp)
    movw $0, 22(%rsp)
    mov $7, %rax                /* poll POLLIN */
    lea 16(%rsp), %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    cmp $1, %rax
    jne fail_event

    mov $291, %rax              /* epoll_create1 */
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    js fail_event
    mov %rax, %r14
    movl $1, 32(%rsp)           /* epoll_event.events = POLLIN */
    movq $0x1234, 36(%rsp)      /* epoll_event.data */
    mov $233, %rax              /* epoll_ctl ADD */
    mov %r14, %rdi
    mov $1, %rsi
    mov %r13, %rdx
    lea 32(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_event
    mov $232, %rax              /* epoll_wait */
    mov %r14, %rdi
    lea 48(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_event
    cmpl $1, 48(%rsp)
    jne fail_event
    cmpq $0x1234, 52(%rsp)
    jne fail_event

    mov $0, %rax                /* read eventfd */
    mov %r13, %rdi
    mov %rsp, %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_event
    cmpq $1, 0(%rsp)
    jne fail_event
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $3, %rax
    mov %r14, %rdi
    syscall
    add $64, %rsp

    /* mremap: grow a mapping and preserve its contents. */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_mremap
    mov %rax, %r12
    movb $0x5a, (%r12)
    mov $25, %rax               /* mremap */
    mov %r12, %rdi
    mov $4096, %rsi
    mov $8192, %rdx
    mov $1, %r10                /* MREMAP_MAYMOVE */
    xor %r8, %r8
    syscall
    test %rax, %rax
    js fail_mremap
    mov %rax, %r13
    cmpb $0x5a, (%r13)
    jne fail_mremap
    movb $0x5b, 4096(%r13)
    cmpb $0x5b, 4096(%r13)
    jne fail_mremap
    mov $11, %rax
    mov %r13, %rdi
    mov $8192, %rsi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea mremap_message(%rip), %rsi
    mov $mremap_message_len, %rdx
    syscall

    /* Signal: install SIGUSR1, raise it, and verify the handler ran. */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_signal
    mov %rax, %r12
    movl $0, (%r12)
    sub $64, %rsp
    lea signal_handler(%rip), %rax
    mov %rax, 0(%rsp)
    movq $0x04000000, 8(%rsp)   /* SA_RESTORER */
    lea signal_restorer(%rip), %rax
    mov %rax, 16(%rsp)
    movq $0, 24(%rsp)
    mov $13, %rax               /* rt_sigaction */
    mov $10, %rdi               /* SIGUSR1 */
    mov %rsp, %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_signal
    mov $39, %rax               /* getpid */
    syscall
    mov %rax, %rdi
    mov $10, %rsi
    mov $62, %rax               /* kill */
    syscall
    test %rax, %rax
    jne fail_signal
    cmpl $1, (%r12)
    jne fail_signal
    add $64, %rsp
    mov %r12, %rdi
    mov $4096, %rsi
    mov $11, %rax
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea signal_message(%rip), %rsi
    mov $signal_message_len, %rdx
    syscall

    /* Thread + futex: map a shared page, clone, wait for the child. */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_thread
    mov %rax, %r12
    movl $0, (%r12)
    movl $0, 8(%r12)
    mov $56, %rax               /* clone */
    mov $0x1290100, %rdi        /* VM|THREAD|SETTLS|CLEARTID|CHILD_SETTID */
    lea 0xf00(%r12), %rsi
    xor %rdx, %rdx
    lea 8(%r12), %r10
    lea 0x200(%r12), %r8
    syscall
    test %rax, %rax
    jz child_thread
wait_flag:
    cmpl $0, (%r12)
    jne wait_child
    mov $202, %rax              /* futex wait on flag */
    mov %r12, %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    syscall
    jmp wait_flag
wait_child:
    mov 8(%r12), %edx
    test %edx, %edx
    jz thread_done
    mov $202, %rax              /* futex wait on child_tid */
    lea 8(%r12), %rdi
    xor %rsi, %rsi
    syscall
    jmp wait_child
thread_done:
    mov %r12, %rdi
    mov $4096, %rsi
    mov $11, %rax
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea thread_message(%rip), %rsi
    mov $thread_message_len, %rdx
    syscall
    jmp after_thread

child_thread:
    movl $1, (%r12)
    mov $202, %rax              /* futex wake flag */
    mov %r12, %rdi
    mov $1, %rsi
    mov $1, %rdx
    syscall
    mov $60, %rax               /* thread exit */
    xor %rdi, %rdi
    syscall
    jmp child_thread

after_thread:
    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea event_message(%rip), %rsi
    mov $event_message_len, %rdx
    syscall

    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea pipe_message(%rip), %rsi
    mov $pipe_message_len, %rdx
    syscall

    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea dir_message(%rip), %rsi
    mov $dir_message_len, %rdx
    syscall

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
    jmp report_failure
fail_dir:
    lea dir_failure(%rip), %rsi
    mov $dir_failure_len, %rdx
    jmp report_failure
fail_pipe:
    lea pipe_failure(%rip), %rsi
    mov $pipe_failure_len, %rdx
    jmp report_failure
fail_event:
    lea event_failure(%rip), %rsi
    mov $event_failure_len, %rdx
    jmp report_failure
fail_thread:
    lea thread_failure(%rip), %rsi
    mov $thread_failure_len, %rdx
    jmp report_failure
fail_mremap:
    lea mremap_failure(%rip), %rsi
    mov $mremap_failure_len, %rdx
    jmp report_failure
fail_signal:
    lea signal_failure(%rip), %rsi
    mov $signal_failure_len, %rdx
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
dir_message:
    .ascii "AETHER_LINUX_DIR_OK root=ok usrbin=ok\n"
    .set dir_message_len, . - dir_message
dir_failure:
    .ascii "AETHER_LINUX_DIR_FAIL\n"
    .set dir_failure_len, . - dir_failure
pipe_message:
    .ascii "AETHER_LINUX_PIPE_OK write=ok poll=ok read=ok\n"
    .set pipe_message_len, . - pipe_message
pipe_failure:
    .ascii "AETHER_LINUX_PIPE_FAIL\n"
    .set pipe_failure_len, . - pipe_failure
pipe_data:
    .ascii "ping"
event_message:
    .ascii "AETHER_LINUX_EVENT_OK eventfd=ok epoll=ok\n"
    .set event_message_len, . - event_message
event_failure:
    .ascii "AETHER_LINUX_EVENT_FAIL\n"
    .set event_failure_len, . - event_failure
thread_message:
    .ascii "AETHER_LINUX_THREAD_OK clone=ok futex=ok join=ok\n"
    .set thread_message_len, . - thread_message
thread_failure:
    .ascii "AETHER_LINUX_THREAD_FAIL\n"
    .set thread_failure_len, . - thread_failure
mremap_message:
    .ascii "AETHER_LINUX_MREMAP_OK grow=ok data=ok\n"
    .set mremap_message_len, . - mremap_message
mremap_failure:
    .ascii "AETHER_LINUX_MREMAP_FAIL\n"
    .set mremap_failure_len, . - mremap_failure
signal_message:
    .ascii "AETHER_LINUX_SIGNAL_OK handler=ok return=ok\n"
    .set signal_message_len, . - signal_message
signal_failure:
    .ascii "AETHER_LINUX_SIGNAL_FAIL\n"
    .set signal_failure_len, . - signal_failure

signal_handler:
    movl $1, (%r12)
    ret
signal_restorer:
    mov $15, %rax
    syscall
    jmp signal_restorer
root_path:
    .asciz "/"
usrbin_path:
    .asciz "/usr/bin"
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

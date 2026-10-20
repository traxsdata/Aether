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
    mov $8192, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_mmap
    mov %rax, %r12

    /* The syscall wrapper must page in a valid untouched buffer before it
     * checks the descriptor. The invalid fd then returns EBADF without
     * exposing the zero byte on the serial output. */
    mov $0, %rax
    mov $999, %rdi
    lea 4096(%r12), %rsi
    mov $1, %rdx
    syscall
    cmp $-9, %rax                /* -EBADF */
    jne fail_mmap
    /* The page was untouched until read's validated buffer access. */
    cmpq $0, 4096(%r12)
    jne fail_mmap
    movabs $0x706167656661756c, %rax
    mov %rax, 4096(%r12)
    cmp %rax, 4096(%r12)
    jne fail_mmap
    mov $1, %rax
    mov $1, %rdi
    lea demand_message(%rip), %rsi
    mov $demand_message_len, %rdx
    syscall
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

    /* init1(flags=0) requests blocking mode and must be rejected. */
    mov $294, %rax
    xor %rdi, %rdi
    syscall
    cmp $-22, %rax
    jne fail_inotify_flags

    /* Only IN_NONBLOCK is supported; watch the overlay file for IN_MODIFY. */
    mov $294, %rax
    mov $0x800, %rdi              /* IN_NONBLOCK */
    syscall
    test %rax, %rax
    js fail_inotify_init
    mov %rax, %r15
    mov $254, %rax
    mov %r15, %rdi
    lea overlay_path(%rip), %rsi
    mov $2, %rdx                 /* IN_MODIFY */
    syscall
    cmp $1, %rax
    jne fail_inotify_add

    /* No event is pending yet, so a nonblocking read must return EAGAIN. */
    mov $0, %rax
    mov %r15, %rdi
    mov %r12, %rsi
    mov $16, %rdx
    syscall
    cmp $-11, %rax
    jne fail_inotify_empty

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

    /* read one fixed-size struct inotify_event; wd=1, mask=IN_MODIFY. */
    mov $0, %rax
    mov %r15, %rdi
    mov %r12, %rsi
    mov $16, %rdx
    syscall
    cmp $16, %rax
    jne fail_inotify_event_length
    cmpl $1, 0(%r12)
    jne fail_inotify_event_watch
    cmpl $2, 4(%r12)
    jne fail_inotify_event_mask
    mov $3, %rax
    mov %r15, %rdi
    syscall

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

    /* Cross-process SIGUSR1: child installs a handler and waits; parent sends. */
    push %r14
    push %r15
    mov $57, %rax               /* fork */
    syscall
    test %rax, %rax
    js fail_signal
    jz cross_signal_child
    mov %rax, %r14              /* child PID */
    /* Give the child a scheduling interval to install its handler. */
    mov $10000000, %r15d
cross_signal_delay:
    dec %r15d
    jne cross_signal_delay
    mov %r14, %rdi
    mov $10, %rsi               /* SIGUSR1 */
    mov $62, %rax               /* kill(child_pid, SIGUSR1) */
    syscall
    test %rax, %rax
    jne fail_signal
    sub $16, %rsp
    mov $61, %rax               /* wait4(child_pid, &status, 0, NULL) */
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_signal
    cmpl $0x2b00, (%rsp)        /* handler-observed child exits 43 */
    jne fail_signal
    add $16, %rsp
    mov $1, %rax
    mov $1, %rdi
    lea cross_signal_message(%rip), %rsi
    mov $cross_signal_message_len, %rdx
    syscall
    pop %r15
    pop %r14
    jmp cross_signal_done

cross_signal_child:
    sub $16, %rsp
    movl $0, (%rsp)
    mov %rsp, %r12              /* handler writes this child's private flag */
    sub $64, %rsp
    lea signal_handler(%rip), %rax
    mov %rax, 0(%rsp)
    movq $0x04000000, 8(%rsp)   /* SA_RESTORER */
    lea signal_restorer(%rip), %rax
    mov %rax, 16(%rsp)
    movq $0, 24(%rsp)
    mov $13, %rax               /* rt_sigaction(SIGUSR1, ...) */
    mov $10, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_signal
    add $64, %rsp
cross_signal_wait:
    cmpl $1, (%r12)
    jne cross_signal_wait
    mov $60, %rax
    mov $43, %rdi
    syscall
    jmp cross_signal_wait

cross_signal_done:
    /* Fork + wait4: the child exits 42 and the parent reaps the status. */
    /* kill(0, 0) is an existence check on the caller's process group. */
    xor %rdi, %rdi
    xor %rsi, %rsi
    mov $62, %rax
    syscall
    test %rax, %rax
    jne fail_fork
    mov $39, %rax               /* Preserve the inherited group's leader PID. */
    syscall
    mov %rax, %r13
    neg %r13
    mov %r13, %rdi
    xor %rsi, %rsi
    mov $62, %rax               /* kill(-pgid, 0): group existence check */
    syscall
    test %rax, %rax
    jne fail_fork
    neg %r13
    sub $16, %rsp
    mov $57, %rax               /* fork */
    syscall
    test %rax, %rax
    js fail_fork
    jz child_process
    mov %rax, %r12
    /* A non-child PID must fail immediately, despite a live child. */
    mov $61, %rax
    lea 100(%r12), %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    syscall
    cmp $-10, %rax              /* -ECHILD */
    jne fail_fork
    /* Unsupported process-group selectors must be rejected. */
    mov $61, %rax
    mov $-2, %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    syscall
    cmp $-22, %rax              /* -EINVAL */
    jne fail_fork
parent_wait:
    mov $61, %rax               /* blocking wait4(child, &status, 0) */
    mov %r12, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js fail_fork
    cmpl $0x2a00, (%rsp)        /* exit 42 << 8 */
    jne fail_fork
    add $16, %rsp
    mov $1, %rax
    mov $1, %rdi
    lea fork_message(%rip), %rsi
    mov $fork_message_len, %rdx
    syscall
    jmp after_fork

child_process:
    /* Join the parent's group, then setsid is legal because we are not leader. */
    mov $109, %rax               /* setpgid(0, parent_pgid) */
    xor %rdi, %rdi
    mov %r13, %rsi
    syscall
    test %rax, %rax
    jne fail_fork
    mov $121, %rax               /* getpgid(0) */
    xor %rdi, %rdi
    syscall
    cmp %r13, %rax
    jne fail_fork
    mov $112, %rax               /* setsid */
    syscall
    mov %rax, %r12                /* setsid returns the new SID (child PID). */
    mov $39, %rax
    syscall
    cmp %r12, %rax
    jne fail_fork
    mov $121, %rax               /* setsid also makes the caller its PGID. */
    xor %rdi, %rdi
    syscall
    cmp %r12, %rax
    jne fail_fork
    mov $60, %rax
    mov $42, %rdi
    syscall
    jmp child_process

after_fork:
    /* Thread + futex: map a shared page, clone, wait for the child. */
    sub $16, %rsp
    movq $0, 0(%rsp)
    mov $290, %rax              /* eventfd2 */
    xor %rdi, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    js fail_thread
    mov %rax, %r14
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x21, %r10
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
    mov %r14, %rdi
    mov %rsp, %rsi
    mov $8, %rdx
    xor %rax, %rax
    syscall
    cmp $8, %rax
    jne fail_thread
    cmpq $1, 0(%rsp)
    jne fail_thread
    cmpl $1, 16(%r12)
    jne fail_thread
    mov $57, %rax               /* fork is explicitly unsupported here */
    syscall
    cmp $-38, %rax
    jne fail_thread
    add $16, %rsp
    mov %r12, %rdi
    mov $4096, %rsi
    mov $11, %rax
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea thread_message(%rip), %rsi
    mov $thread_message_len, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea shared_mmap_message(%rip), %rsi
    mov $shared_mmap_message_len, %rdx
    syscall
    jmp after_thread

child_thread:
    movq $1, 16(%r12)
    mov %r14, %rdi
    lea 16(%r12), %rsi
    mov $8, %rdx
    mov $1, %rax
    syscall
    cmp $8, %rax
    jne fail_thread
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
    /* A 30 ms relative FUTEX_WAIT timeout rounds up to three 100 Hz ticks. */
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
    sub $16, %rsp
    movq $0, 0(%rsp)
    movq $30000000, 8(%rsp)
    mov $202, %rax
    mov %r12, %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    mov %rsp, %r10
    syscall
    add $16, %rsp
    cmp $-110, %rax
    jne fail_thread
    mov $11, %rax
    mov %r12, %rdi
    mov $4096, %rsi
    syscall
    test %rax, %rax
    js fail_thread
    mov $1, %rax
    mov $1, %rdi
    lea futex_timeout_message(%rip), %rsi
    mov $futex_timeout_message_len, %rdx
    syscall

    /* ================= blocking multiplexing and time ================= */
    sub $1024, %rsp
    /* Scratch page: signal flag at 0, thread tid at 8, thread data at 16. */
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_sleep
    mov %rax, %r12
    movl $0, (%r12)
    movl $0, 8(%r12)

    /* --- nanosleep: a 60 ms relative sleep advances the monotonic clock. */
    mov $228, %rax              /* clock_gettime(CLOCK_MONOTONIC) */
    mov $1, %rdi
    lea 32(%rsp), %rsi
    syscall
    test %rax, %rax
    jne fail_sleep_rel
    movq $0, 0(%rsp)
    movq $60000000, 8(%rsp)
    mov $35, %rax               /* nanosleep */
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_sleep_rel
    mov $228, %rax
    mov $1, %rdi
    lea 0(%rsp), %rsi
    syscall
    test %rax, %rax
    jne fail_sleep_rel
    mov 0(%rsp), %rbx
    mov 32(%rsp), %rax
    sub %rax, %rbx
    imul $1000000000, %rbx, %rbx
    add 8(%rsp), %rbx
    sub 40(%rsp), %rbx
    cmp $50000000, %rbx         /* at least 50 ms of the 60 ms elapsed */
    jl fail_sleep_rel

    /* --- clock_nanosleep(TIMER_ABSTIME): deadline 40 ms ahead. */
    mov $228, %rax
    mov $1, %rdi
    lea 0(%rsp), %rsi
    syscall
    test %rax, %rax
    jne fail_sleep_abs
    mov 0(%rsp), %rax
    mov 8(%rsp), %rdx
    add $40000000, %rdx
    cmp $1000000000, %rdx
    jb .Lnanosleep_nowrap
    sub $1000000000, %rdx
    inc %rax
.Lnanosleep_nowrap:
    mov %rax, 0(%rsp)
    mov %rdx, 8(%rsp)
    mov $230, %rax              /* clock_nanosleep */
    mov $1, %rdi                /* CLOCK_MONOTONIC */
    mov $1, %rsi                /* TIMER_ABSTIME */
    lea 0(%rsp), %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_sleep_abs

    /* --- A deliverable signal interrupts nanosleep with EINTR. --- */
    mov $57, %rax               /* fork */
    syscall
    test %rax, %rax
    jns .Lintr_forked
    cmp $-11, %rax
    je fail_sleep_intr_fork_eagain
    cmp $-38, %rax
    je fail_sleep_intr_fork_enosys
    jmp fail_sleep_intr
.Lintr_forked:
    jz .Lintr_child
    mov %rax, %r15
    movq $2, 0(%rsp)
    movq $0, 8(%rsp)
    mov $35, %rax
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    cmp $-4, %rax               /* -EINTR */
    jne fail_sleep_intr_ret
    cmpl $1, (%r12)             /* the SIGUSR1 handler ran */
    jne fail_sleep_intr_handler
    sub $16, %rsp
    mov $61, %rax
    mov %r15, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    add $16, %rsp
    test %rax, %rax
    jns .Lintr_reaped
    cmp $-10, %rax
    je fail_sleep_intr_echild
    cmp $-4, %rax
    je fail_sleep_intr_eintr
    cmp $-3, %rax
    je fail_sleep_intr_esrch
    cmp $-22, %rax
    je fail_sleep_intr_einval
    jmp fail_sleep_intr
.Lintr_reaped:
    mov $1, %rax
    mov $1, %rdi
    lea nanosleep_message(%rip), %rsi
    mov $nanosleep_message_len, %rdx
    syscall
    jmp .Lpipe_block_start

.Lintr_child:
    mov $5000000, %ecx
.Lintr_delay:
    dec %ecx
    jnz .Lintr_delay
    mov $110, %rax              /* getppid */
    syscall
    mov %rax, %rdi
    mov $10, %rsi               /* SIGUSR1 */
    mov $62, %rax
    syscall
    mov $60, %rax
    xor %rdi, %rdi
    syscall

    /* --- Cross-process blocking pipe write and read. --- */
.Lpipe_block_start:
    mov $293, %rax              /* pipe2 */
    lea 96(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_pipe_block
    mov 96(%rsp), %r13d
    mov 100(%rsp), %r14d
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_pipe_block
    jz .Lpipe_block_child
    mov %rax, %r15
    movq $0, 0(%rsp)
    movq $60000000, 8(%rsp)
    mov $35, %rax               /* let the child fill and block */
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_pipe_block
    mov $0, %rax                /* drain the full pipe */
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $512, %rdx
    syscall
    cmp $512, %rax
    jne fail_pipe_block
    mov $0, %rax                /* blocking read wakes the blocked writer */
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_pipe_block
    cmpb $0x58, 192(%rsp)
    jne fail_pipe_block
    mov $3, %rax
    mov %r14, %rdi
    syscall
    sub $16, %rsp
    mov $61, %rax
    mov %r15, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    add $16, %rsp
    test %rax, %rax
    js fail_pipe_block
    mov $0, %rax                /* empty pipe, no writers: EOF */
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    test %rax, %rax
    jne fail_pipe_block
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea pipe_block_message(%rip), %rsi
    mov $pipe_block_message_len, %rdx
    syscall
    jmp .Lpoll_block_start

.Lpipe_block_child:
    lea 192(%rsp), %rdi
    mov $512, %rcx
    mov $0x41, %al
.Lpipe_fill:
    mov %al, (%rdi)
    inc %rdi
    dec %rcx
    jnz .Lpipe_fill
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $512, %rdx
    syscall
    cmp $512, %rax
    jne fail_pipe_block
    movb $0x58, 192(%rsp)
    mov $1, %rax                /* blocks until the parent drains */
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_pipe_block
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $3, %rax
    mov %r14, %rdi
    syscall
    mov $60, %rax
    xor %rdi, %rdi
    syscall

    /* --- poll: finite timeout, infinite timeout, pipe and eventfd. --- */
.Lpoll_block_start:
    mov $293, %rax
    lea 96(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_poll_block
    mov 96(%rsp), %r13d
    mov 100(%rsp), %r14d
    movl %r13d, 32(%rsp)
    movw $1, 36(%rsp)           /* POLLIN */
    movw $0, 38(%rsp)
    mov $228, %rax
    mov $1, %rdi
    lea 704(%rsp), %rsi
    syscall
    mov $7, %rax                /* 30 ms finite poll on an empty pipe */
    lea 32(%rsp), %rdi
    mov $1, %rsi
    mov $30, %rdx
    syscall
    test %rax, %rax
    jne fail_poll_block
    mov $228, %rax
    mov $1, %rdi
    lea 720(%rsp), %rsi
    syscall
    mov 720(%rsp), %rbx
    mov 704(%rsp), %rax
    sub %rax, %rbx
    imul $1000000000, %rbx, %rbx
    add 728(%rsp), %rbx
    sub 712(%rsp), %rbx
    cmp $20000000, %rbx
    jl fail_poll_block
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_poll_block
    jz .Lpoll_block_child
    mov %rax, %r15
    movl %r13d, 32(%rsp)
    movw $1, 36(%rsp)
    movw $0, 38(%rsp)
    mov $7, %rax                /* infinite poll wakes on the child write */
    lea 32(%rsp), %rdi
    mov $1, %rsi
    mov $-1, %rdx
    syscall
    cmp $1, %rax
    jne fail_poll_block
    movzwl 38(%rsp), %eax
    test $1, %eax
    jz fail_poll_block
    mov $0, %rax
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_poll_block
    cmpb $0x50, 192(%rsp)
    jne fail_poll_block
    sub $16, %rsp
    mov $61, %rax
    mov %r15, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    add $16, %rsp
    test %rax, %rax
    js fail_poll_block

    /* eventfd readiness: a CLONE_VM thread writes while poll blocks. */
    movq $0, 0(%rsp)
    mov $290, %rax
    xor %rdi, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    js fail_poll_block
    mov %rax, 104(%rsp)
    mov %rax, (%r12)
    mov 104(%rsp), %eax
    mov %eax, 32(%rsp)
    movw $1, 36(%rsp)
    movw $0, 38(%rsp)
    mov $7, %rax                /* finite poll on a zero counter */
    lea 32(%rsp), %rdi
    mov $1, %rsi
    mov $30, %rdx
    syscall
    test %rax, %rax
    jne fail_poll_block
    mov $56, %rax               /* clone thread */
    mov $0x1290100, %rdi
    lea 0xf00(%r12), %rsi
    xor %rdx, %rdx
    lea 8(%r12), %r10
    lea 0xf00(%r12), %r8
    syscall
    test %rax, %rax
    js fail_poll_block
    jz .Lpoll_event_thread
    mov 104(%rsp), %eax
    mov %eax, 32(%rsp)
    movw $1, 36(%rsp)
    movw $0, 38(%rsp)
    mov $7, %rax                /* infinite poll wakes on the eventfd */
    lea 32(%rsp), %rdi
    mov $1, %rsi
    mov $-1, %rdx
    syscall
    cmp $1, %rax
    jne fail_poll_block
    mov $0, %rax
    mov 104(%rsp), %rdi
    lea 192(%rsp), %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_poll_block
    cmpq $1, 192(%rsp)
    jne fail_poll_block
    mov $3, %rax
    mov 104(%rsp), %rdi
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $3, %rax
    mov %r14, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea poll_block_message(%rip), %rsi
    mov $poll_block_message_len, %rdx
    syscall
    jmp .Lepoll_block_start

.Lpoll_block_child:
    movq $0, 0(%rsp)
    movq $60000000, 8(%rsp)
    mov $35, %rax
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_poll_block
    movb $0x50, 192(%rsp)
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_poll_block
    mov $60, %rax
    xor %rdi, %rdi
    syscall

.Lpoll_event_thread:
    mov $20000000, %ecx
.Lpoll_event_delay:
    dec %ecx
    jnz .Lpoll_event_delay
    mov (%r12), %rdi
    movq $1, 16(%r12)
    mov $1, %rax
    lea 16(%r12), %rsi
    mov $8, %rdx
    syscall
    mov $60, %rax
    xor %rdi, %rdi
    syscall

    /* --- epoll: blocking wait, level, edge, and one-shot. --- */
.Lepoll_block_start:
    mov $293, %rax
    lea 96(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_epoll_s1
    mov 96(%rsp), %r13d
    mov 100(%rsp), %r14d
    mov $291, %rax
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    js fail_epoll_s2
    mov %rax, 108(%rsp)
    movl $1, 160(%rsp)
    movq $0x1234, 164(%rsp)
    mov $233, %rax              /* ADD read end, level-triggered */
    mov 108(%rsp), %rdi
    mov $1, %rsi
    mov %r13, %rdx
    lea 160(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s3
    mov $232, %rax              /* 30 ms finite wait on an empty pipe */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    mov $30, %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s4
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_epoll_s5
    jz .Lepoll_block_child
    mov %rax, %r15
    mov $232, %rax              /* infinite wait wakes on readiness */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    mov $-1, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s6
    cmpl $1, 176(%rsp)
    jne fail_epoll_s7
    cmpq $0x1234, 180(%rsp)
    jne fail_epoll_s8
    mov $232, %rax              /* level-triggered re-report */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s9
    mov $0, %rax
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s10
    sub $16, %rsp
    mov $61, %rax
    mov %r15, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    add $16, %rsp
    test %rax, %rax
    js fail_epoll_s11

    /* Edge-triggered slice: one report per new readiness transition. */
    movl $0x80000001, 160(%rsp)
    movq $0x1234, 164(%rsp)
    mov $233, %rax
    mov 108(%rsp), %rdi
    mov $3, %rsi                /* MOD */
    mov %r13, %rdx
    lea 160(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s12
    movb $0x45, 192(%rsp)
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s13
    mov $232, %rax
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s14
    mov $232, %rax              /* no new edge: zero */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s15
    mov $0, %rax
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s16
    movb $0x45, 192(%rsp)
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s17
    mov $232, %rax              /* a second write is a new edge */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s18
    mov $0, %rax
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s19

    /* One-shot: disarmed after a report, re-armed by MOD. */
    movl $0x40000001, 160(%rsp)
    movq $0x1234, 164(%rsp)
    mov $233, %rax
    mov 108(%rsp), %rdi
    mov $3, %rsi
    mov %r13, %rdx
    lea 160(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s20
    movb $0x4f, 192(%rsp)
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s21
    mov $232, %rax
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s22
    mov $232, %rax              /* disarmed: no report */
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s23
    mov $233, %rax              /* MOD re-arms */
    mov 108(%rsp), %rdi
    mov $3, %rsi
    mov %r13, %rdx
    lea 160(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_epoll_s24
    mov $232, %rax
    mov 108(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    cmp $1, %rax
    jne fail_epoll_s25
    mov $0, %rax
    mov %r13, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s26
    mov $3, %rax
    mov 108(%rsp), %rdi
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $3, %rax
    mov %r14, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea epoll_block_message(%rip), %rsi
    mov $epoll_block_message_len, %rdx
    syscall
    jmp .Ltimerfd_start

.Lepoll_block_child:
    movq $0, 0(%rsp)
    movq $60000000, 8(%rsp)
    mov $35, %rax
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_epoll_s1
    movb $0x45, 192(%rsp)
    mov $1, %rax
    mov %r14, %rdi
    lea 192(%rsp), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne fail_epoll_s1
    mov $60, %rax
    xor %rdi, %rdi
    syscall

    /* --- timerfd: one-shot, periodic, poll, and epoll. --- */
.Ltimerfd_start:
    mov $283, %rax              /* timerfd_create(CLOCK_MONOTONIC, 0) */
    mov $1, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    js fail_timerfd_s1
    mov %rax, 112(%rsp)
    movq $0, 768(%rsp)          /* interval 0 */
    movq $0, 776(%rsp)
    movq $0, 784(%rsp)          /* value 40 ms */
    movq $40000000, 792(%rsp)
    mov $286, %rax              /* timerfd_settime relative */
    mov 112(%rsp), %rdi
    xor %rsi, %rsi
    lea 768(%rsp), %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_timerfd_s2
    mov 112(%rsp), %eax
    mov %eax, 32(%rsp)
    movw $1, 36(%rsp)
    movw $0, 38(%rsp)
    mov $7, %rax                /* poll blocks until the timer expires */
    lea 32(%rsp), %rdi
    mov $1, %rsi
    mov $300, %rdx
    syscall
    cmp $1, %rax
    jne fail_timerfd_s3
    mov $0, %rax
    mov 112(%rsp), %rdi
    lea 192(%rsp), %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_timerfd_s4
    cmpq $1, 192(%rsp)
    jne fail_timerfd_s5
    mov $287, %rax              /* one-shot read leaves it disarmed */
    mov 112(%rsp), %rdi
    lea 800(%rsp), %rsi
    syscall
    test %rax, %rax
    jne fail_timerfd_s6
    cmpq $0, 800(%rsp)
    jne fail_timerfd_s7
    cmpq $0, 816(%rsp)
    jne fail_timerfd_s8

    movq $0, 768(%rsp)          /* periodic: 20 ms interval and value */
    movq $20000000, 776(%rsp)
    movq $0, 784(%rsp)
    movq $20000000, 792(%rsp)
    mov $286, %rax
    mov 112(%rsp), %rdi
    xor %rsi, %rsi
    lea 768(%rsp), %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_timerfd_s9
    movq $0, 0(%rsp)
    movq $60000000, 8(%rsp)
    mov $35, %rax
    lea 0(%rsp), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_timerfd_s10
    mov $0, %rax
    mov 112(%rsp), %rdi
    lea 192(%rsp), %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_timerfd_s11
    cmpq $2, 192(%rsp)
    jb fail_timerfd_s12
    mov $287, %rax
    mov 112(%rsp), %rdi
    lea 800(%rsp), %rsi
    syscall
    test %rax, %rax
    jne fail_timerfd_s13
    cmpq $20000000, 808(%rsp)
    jne fail_timerfd_s14

    /* timerfd readiness through epoll. */
    mov $291, %rax
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    js fail_timerfd_s15
    mov %rax, 120(%rsp)
    movl $1, 160(%rsp)
    movq $0x55, 164(%rsp)
    mov $233, %rax
    mov 120(%rsp), %rdi
    mov $1, %rsi
    mov 112(%rsp), %rdx
    lea 160(%rsp), %r10
    syscall
    test %rax, %rax
    jne fail_timerfd_s16
    movq $0, 768(%rsp)
    movq $0, 776(%rsp)
    movq $0, 784(%rsp)
    movq $30000000, 792(%rsp)
    mov $286, %rax
    mov 112(%rsp), %rdi
    xor %rsi, %rsi
    lea 768(%rsp), %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_timerfd_s17
    mov $232, %rax
    mov 120(%rsp), %rdi
    lea 176(%rsp), %rsi
    mov $1, %rdx
    mov $-1, %r10
    syscall
    cmp $1, %rax
    jne fail_timerfd_s18
    cmpq $0x55, 180(%rsp)
    jne fail_timerfd_s19
    mov $0, %rax
    mov 112(%rsp), %rdi
    lea 192(%rsp), %rsi
    mov $8, %rdx
    syscall
    cmp $8, %rax
    jne fail_timerfd_s20

    movq $0, 768(%rsp)          /* disarm and verify poll reports zero */
    movq $0, 776(%rsp)
    movq $0, 784(%rsp)
    movq $0, 792(%rsp)
    mov $286, %rax
    mov 112(%rsp), %rdi
    xor %rsi, %rsi
    lea 768(%rsp), %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_timerfd_s21
    mov 112(%rsp), %eax
    mov %eax, 32(%rsp)
    movw $1, 36(%rsp)
    movw $0, 38(%rsp)
    mov $7, %rax
    lea 32(%rsp), %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    test %rax, %rax
    jne fail_timerfd_s22
    mov $3, %rax
    mov 120(%rsp), %rdi
    syscall
    mov $3, %rax
    mov 112(%rsp), %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea timerfd_message(%rip), %rsi
    mov $timerfd_message_len, %rdx
    syscall

    add $1024, %rsp
    jmp .Lafter_multiplex

.Lafter_multiplex:
    mov $1, %rax                /* write */
    mov $1, %rdi                /* stdout */
    lea event_message(%rip), %rsi
    mov $event_message_len, %rdx
    syscall

    mov $1, %rax
    mov $1, %rdi
    lea inotify_message(%rip), %rsi
    mov $inotify_message_len, %rdx
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

    /* execve replaces this process; success never returns here. */
    mov $59, %rax
    lea exec_path(%rip), %rdi
    lea exec_argv(%rip), %rsi
    lea exec_envp(%rip), %rdx
    syscall
    lea exec_failure(%rip), %rsi
    mov $exec_failure_len, %rdx
    jmp report_failure

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
fail_inotify:
    lea inotify_failure(%rip), %rsi
    mov $inotify_failure_len, %rdx
    jmp report_failure
fail_inotify_flags:
    lea inotify_flags_failure(%rip), %rsi
    mov $inotify_flags_failure_len, %rdx
    jmp report_failure
fail_inotify_init:
    lea inotify_init_failure(%rip), %rsi
    mov $inotify_init_failure_len, %rdx
    jmp report_failure
fail_inotify_add:
    lea inotify_add_failure(%rip), %rsi
    mov $inotify_add_failure_len, %rdx
    jmp report_failure
fail_inotify_empty:
    lea inotify_empty_failure(%rip), %rsi
    mov $inotify_empty_failure_len, %rdx
    jmp report_failure
fail_inotify_event_length:
    lea inotify_event_length_failure(%rip), %rsi
    mov $inotify_event_length_failure_len, %rdx
    jmp report_failure
fail_inotify_event_watch:
    lea inotify_event_watch_failure(%rip), %rsi
    mov $inotify_event_watch_failure_len, %rdx
    jmp report_failure
fail_inotify_event_mask:
    lea inotify_event_mask_failure(%rip), %rsi
    mov $inotify_event_mask_failure_len, %rdx
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
    jmp report_failure
fail_fork:
    lea fork_failure(%rip), %rsi
    mov $fork_failure_len, %rdx
    jmp report_failure
fail_sleep:
    lea sleep_failure(%rip), %rsi
    mov $sleep_failure_len, %rdx
    jmp report_failure
fail_sleep_rel:
    lea sleep_rel_failure(%rip), %rsi
    mov $sleep_rel_failure_len, %rdx
    jmp report_failure
fail_sleep_abs:
    lea sleep_abs_failure(%rip), %rsi
    mov $sleep_abs_failure_len, %rdx
    jmp report_failure
fail_sleep_intr:
    lea sleep_intr_failure(%rip), %rsi
    mov $sleep_intr_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_ret:
    lea sleep_intr_ret_failure(%rip), %rsi
    mov $sleep_intr_ret_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_handler:
    lea sleep_intr_handler_failure(%rip), %rsi
    mov $sleep_intr_handler_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_fork_eagain:
    lea sleep_intr_fork_eagain_failure(%rip), %rsi
    mov $sleep_intr_fork_eagain_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_fork_enosys:
    lea sleep_intr_fork_enosys_failure(%rip), %rsi
    mov $sleep_intr_fork_enosys_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_echild:
    lea sleep_intr_echild_failure(%rip), %rsi
    mov $sleep_intr_echild_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_eintr:
    lea sleep_intr_eintr_failure(%rip), %rsi
    mov $sleep_intr_eintr_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_esrch:
    lea sleep_intr_esrch_failure(%rip), %rsi
    mov $sleep_intr_esrch_failure_len, %rdx
    jmp report_failure
fail_sleep_intr_einval:
    lea sleep_intr_einval_failure(%rip), %rsi
    mov $sleep_intr_einval_failure_len, %rdx
    jmp report_failure
fail_pipe_block:
    lea pipe_block_failure(%rip), %rsi
    mov $pipe_block_failure_len, %rdx
    jmp report_failure
fail_poll_block:
    lea poll_block_failure(%rip), %rsi
    mov $poll_block_failure_len, %rdx
    jmp report_failure
fail_epoll_s1:
    lea epoll_s1_failure(%rip), %rsi
    mov $epoll_s1_failure_len, %rdx
    jmp report_failure
fail_epoll_s2:
    lea epoll_s2_failure(%rip), %rsi
    mov $epoll_s2_failure_len, %rdx
    jmp report_failure
fail_epoll_s3:
    lea epoll_s3_failure(%rip), %rsi
    mov $epoll_s3_failure_len, %rdx
    jmp report_failure
fail_epoll_s4:
    lea epoll_s4_failure(%rip), %rsi
    mov $epoll_s4_failure_len, %rdx
    jmp report_failure
fail_epoll_s5:
    lea epoll_s5_failure(%rip), %rsi
    mov $epoll_s5_failure_len, %rdx
    jmp report_failure
fail_epoll_s6:
    lea epoll_s6_failure(%rip), %rsi
    mov $epoll_s6_failure_len, %rdx
    jmp report_failure
fail_epoll_s7:
    lea epoll_s7_failure(%rip), %rsi
    mov $epoll_s7_failure_len, %rdx
    jmp report_failure
fail_epoll_s8:
    lea epoll_s8_failure(%rip), %rsi
    mov $epoll_s8_failure_len, %rdx
    jmp report_failure
fail_epoll_s9:
    lea epoll_s9_failure(%rip), %rsi
    mov $epoll_s9_failure_len, %rdx
    jmp report_failure
fail_epoll_s10:
    lea epoll_s10_failure(%rip), %rsi
    mov $epoll_s10_failure_len, %rdx
    jmp report_failure
fail_epoll_s11:
    lea epoll_s11_failure(%rip), %rsi
    mov $epoll_s11_failure_len, %rdx
    jmp report_failure
fail_epoll_s12:
    lea epoll_s12_failure(%rip), %rsi
    mov $epoll_s12_failure_len, %rdx
    jmp report_failure
fail_epoll_s13:
    lea epoll_s13_failure(%rip), %rsi
    mov $epoll_s13_failure_len, %rdx
    jmp report_failure
fail_epoll_s14:
    lea epoll_s14_failure(%rip), %rsi
    mov $epoll_s14_failure_len, %rdx
    jmp report_failure
fail_epoll_s15:
    lea epoll_s15_failure(%rip), %rsi
    mov $epoll_s15_failure_len, %rdx
    jmp report_failure
fail_epoll_s16:
    lea epoll_s16_failure(%rip), %rsi
    mov $epoll_s16_failure_len, %rdx
    jmp report_failure
fail_epoll_s17:
    lea epoll_s17_failure(%rip), %rsi
    mov $epoll_s17_failure_len, %rdx
    jmp report_failure
fail_epoll_s18:
    lea epoll_s18_failure(%rip), %rsi
    mov $epoll_s18_failure_len, %rdx
    jmp report_failure
fail_epoll_s19:
    lea epoll_s19_failure(%rip), %rsi
    mov $epoll_s19_failure_len, %rdx
    jmp report_failure
fail_epoll_s20:
    lea epoll_s20_failure(%rip), %rsi
    mov $epoll_s20_failure_len, %rdx
    jmp report_failure
fail_epoll_s21:
    lea epoll_s21_failure(%rip), %rsi
    mov $epoll_s21_failure_len, %rdx
    jmp report_failure
fail_epoll_s22:
    lea epoll_s22_failure(%rip), %rsi
    mov $epoll_s22_failure_len, %rdx
    jmp report_failure
fail_epoll_s23:
    lea epoll_s23_failure(%rip), %rsi
    mov $epoll_s23_failure_len, %rdx
    jmp report_failure
fail_epoll_s24:
    lea epoll_s24_failure(%rip), %rsi
    mov $epoll_s24_failure_len, %rdx
    jmp report_failure
fail_epoll_s25:
    lea epoll_s25_failure(%rip), %rsi
    mov $epoll_s25_failure_len, %rdx
    jmp report_failure
fail_epoll_s26:
    lea epoll_s26_failure(%rip), %rsi
    mov $epoll_s26_failure_len, %rdx
    jmp report_failure
fail_timerfd_s1:
    lea timerfd_s1_failure(%rip), %rsi
    mov $timerfd_s1_failure_len, %rdx
    jmp report_failure
fail_timerfd_s2:
    lea timerfd_s2_failure(%rip), %rsi
    mov $timerfd_s2_failure_len, %rdx
    jmp report_failure
fail_timerfd_s3:
    lea timerfd_s3_failure(%rip), %rsi
    mov $timerfd_s3_failure_len, %rdx
    jmp report_failure
fail_timerfd_s4:
    lea timerfd_s4_failure(%rip), %rsi
    mov $timerfd_s4_failure_len, %rdx
    jmp report_failure
fail_timerfd_s5:
    lea timerfd_s5_failure(%rip), %rsi
    mov $timerfd_s5_failure_len, %rdx
    jmp report_failure
fail_timerfd_s6:
    lea timerfd_s6_failure(%rip), %rsi
    mov $timerfd_s6_failure_len, %rdx
    jmp report_failure
fail_timerfd_s7:
    lea timerfd_s7_failure(%rip), %rsi
    mov $timerfd_s7_failure_len, %rdx
    jmp report_failure
fail_timerfd_s8:
    lea timerfd_s8_failure(%rip), %rsi
    mov $timerfd_s8_failure_len, %rdx
    jmp report_failure
fail_timerfd_s9:
    lea timerfd_s9_failure(%rip), %rsi
    mov $timerfd_s9_failure_len, %rdx
    jmp report_failure
fail_timerfd_s10:
    lea timerfd_s10_failure(%rip), %rsi
    mov $timerfd_s10_failure_len, %rdx
    jmp report_failure
fail_timerfd_s11:
    lea timerfd_s11_failure(%rip), %rsi
    mov $timerfd_s11_failure_len, %rdx
    jmp report_failure
fail_timerfd_s12:
    lea timerfd_s12_failure(%rip), %rsi
    mov $timerfd_s12_failure_len, %rdx
    jmp report_failure
fail_timerfd_s13:
    lea timerfd_s13_failure(%rip), %rsi
    mov $timerfd_s13_failure_len, %rdx
    jmp report_failure
fail_timerfd_s14:
    lea timerfd_s14_failure(%rip), %rsi
    mov $timerfd_s14_failure_len, %rdx
    jmp report_failure
fail_timerfd_s15:
    lea timerfd_s15_failure(%rip), %rsi
    mov $timerfd_s15_failure_len, %rdx
    jmp report_failure
fail_timerfd_s16:
    lea timerfd_s16_failure(%rip), %rsi
    mov $timerfd_s16_failure_len, %rdx
    jmp report_failure
fail_timerfd_s17:
    lea timerfd_s17_failure(%rip), %rsi
    mov $timerfd_s17_failure_len, %rdx
    jmp report_failure
fail_timerfd_s18:
    lea timerfd_s18_failure(%rip), %rsi
    mov $timerfd_s18_failure_len, %rdx
    jmp report_failure
fail_timerfd_s19:
    lea timerfd_s19_failure(%rip), %rsi
    mov $timerfd_s19_failure_len, %rdx
    jmp report_failure
fail_timerfd_s20:
    lea timerfd_s20_failure(%rip), %rsi
    mov $timerfd_s20_failure_len, %rdx
    jmp report_failure
fail_timerfd_s21:
    lea timerfd_s21_failure(%rip), %rsi
    mov $timerfd_s21_failure_len, %rdx
    jmp report_failure
fail_timerfd_s22:
    lea timerfd_s22_failure(%rip), %rsi
    mov $timerfd_s22_failure_len, %rdx
    jmp report_failure

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
demand_message:
    .ascii "AETHER_LINUX_DEMAND_PAGING_OK zero=ok write=ok\n"
    .set demand_message_len, . - demand_message
exec_path:
    .asciz "/usr/bin/exec"
exec_arg0:
    .asciz "/usr/bin/exec"
exec_arg1:
    .asciz "arg-one"
exec_env:
    .asciz "AETHER_EXEC_ENV=present"
    .align 8
exec_argv:
    .quad exec_arg0, exec_arg1, 0
exec_envp:
    .quad exec_env, 0
exec_failure:
    .ascii "AETHER_LINUX_EXECVE_FAIL\n"
    .set exec_failure_len, . - exec_failure
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
inotify_message:
    .ascii "AETHER_LINUX_INOTIFY_OK modify=ok bounded=ok\n"
    .set inotify_message_len, . - inotify_message
inotify_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL\n"
    .set inotify_failure_len, . - inotify_failure
inotify_flags_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=flags\n"
    .set inotify_flags_failure_len, . - inotify_flags_failure
inotify_init_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=init_nonblock\n"
    .set inotify_init_failure_len, . - inotify_init_failure
inotify_add_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=add_watch\n"
    .set inotify_add_failure_len, . - inotify_add_failure
inotify_empty_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=empty_read\n"
    .set inotify_empty_failure_len, . - inotify_empty_failure
inotify_event_length_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=event_length\n"
    .set inotify_event_length_failure_len, . - inotify_event_length_failure
inotify_event_watch_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=event_watch\n"
    .set inotify_event_watch_failure_len, . - inotify_event_watch_failure
inotify_event_mask_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=event_mask\n"
    .set inotify_event_mask_failure_len, . - inotify_event_mask_failure
thread_message:
    .ascii "AETHER_LINUX_THREAD_OK clone=ok futex=ok join=ok\n"
    .set thread_message_len, . - thread_message
shared_mmap_message:
    .ascii "AETHER_LINUX_SHARED_MMAP_OK thread=visible fork=blocked\n"
    .set shared_mmap_message_len, . - shared_mmap_message
thread_failure:
    .ascii "AETHER_LINUX_THREAD_FAIL\n"
    .set thread_failure_len, . - thread_failure
futex_timeout_message:
    .ascii "AETHER_LINUX_FUTEX_TIMEOUT_OK etimedout=ok\n"
    .set futex_timeout_message_len, . - futex_timeout_message
nanosleep_message:
    .ascii "AETHER_LINUX_NANOSLEEP_OK relative=ok absolute=ok eintr=ok\n"
    .set nanosleep_message_len, . - nanosleep_message
sleep_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=setup\n"
    .set sleep_failure_len, . - sleep_failure
sleep_rel_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=relative\n"
    .set sleep_rel_failure_len, . - sleep_rel_failure
sleep_abs_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=absolute\n"
    .set sleep_abs_failure_len, . - sleep_abs_failure
sleep_intr_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr\n"
    .set sleep_intr_failure_len, . - sleep_intr_failure
sleep_intr_ret_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_ret\n"
    .set sleep_intr_ret_failure_len, . - sleep_intr_ret_failure
sleep_intr_handler_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_handler\n"
    .set sleep_intr_handler_failure_len, . - sleep_intr_handler_failure
sleep_intr_echild_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_echild\n"
    .set sleep_intr_echild_failure_len, . - sleep_intr_echild_failure
sleep_intr_fork_eagain_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_fork_eagain\n"
    .set sleep_intr_fork_eagain_failure_len, . - sleep_intr_fork_eagain_failure
sleep_intr_fork_enosys_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_fork_enosys\n"
    .set sleep_intr_fork_enosys_failure_len, . - sleep_intr_fork_enosys_failure
sleep_intr_eintr_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_wait4_eintr\n"
    .set sleep_intr_eintr_failure_len, . - sleep_intr_eintr_failure
sleep_intr_esrch_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_wait4_esrch\n"
    .set sleep_intr_esrch_failure_len, . - sleep_intr_esrch_failure
sleep_intr_einval_failure:
    .ascii "AETHER_LINUX_NANOSLEEP_FAIL stage=eintr_wait4_einval\n"
    .set sleep_intr_einval_failure_len, . - sleep_intr_einval_failure
pipe_block_message:
    .ascii "AETHER_LINUX_PIPE_BLOCK_OK write=block read=block eof=ok cross_process=ok\n"
    .set pipe_block_message_len, . - pipe_block_message
pipe_block_failure:
    .ascii "AETHER_LINUX_PIPE_BLOCK_FAIL\n"
    .set pipe_block_failure_len, . - pipe_block_failure
poll_block_message:
    .ascii "AETHER_LINUX_POLL_BLOCK_OK finite=ok infinite=ok pipe=ok eventfd=ok\n"
    .set poll_block_message_len, . - poll_block_message
poll_block_failure:
    .ascii "AETHER_LINUX_POLL_BLOCK_FAIL\n"
    .set poll_block_failure_len, . - poll_block_failure
epoll_block_message:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_OK finite=ok infinite=ok level=ok edge=ok oneshot=ok\n"
    .set epoll_block_message_len, . - epoll_block_message
epoll_s1_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=1\n"
    .set epoll_s1_failure_len, . - epoll_s1_failure
epoll_s2_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=2\n"
    .set epoll_s2_failure_len, . - epoll_s2_failure
epoll_s3_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=3\n"
    .set epoll_s3_failure_len, . - epoll_s3_failure
epoll_s4_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=4\n"
    .set epoll_s4_failure_len, . - epoll_s4_failure
epoll_s5_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=5\n"
    .set epoll_s5_failure_len, . - epoll_s5_failure
epoll_s6_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=6\n"
    .set epoll_s6_failure_len, . - epoll_s6_failure
epoll_s7_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=7\n"
    .set epoll_s7_failure_len, . - epoll_s7_failure
epoll_s8_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=8\n"
    .set epoll_s8_failure_len, . - epoll_s8_failure
epoll_s9_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=9\n"
    .set epoll_s9_failure_len, . - epoll_s9_failure
epoll_s10_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=10\n"
    .set epoll_s10_failure_len, . - epoll_s10_failure
epoll_s11_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=11\n"
    .set epoll_s11_failure_len, . - epoll_s11_failure
epoll_s12_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=12\n"
    .set epoll_s12_failure_len, . - epoll_s12_failure
epoll_s13_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=13\n"
    .set epoll_s13_failure_len, . - epoll_s13_failure
epoll_s14_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=14\n"
    .set epoll_s14_failure_len, . - epoll_s14_failure
epoll_s15_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=15\n"
    .set epoll_s15_failure_len, . - epoll_s15_failure
epoll_s16_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=16\n"
    .set epoll_s16_failure_len, . - epoll_s16_failure
epoll_s17_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=17\n"
    .set epoll_s17_failure_len, . - epoll_s17_failure
epoll_s18_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=18\n"
    .set epoll_s18_failure_len, . - epoll_s18_failure
epoll_s19_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=19\n"
    .set epoll_s19_failure_len, . - epoll_s19_failure
epoll_s20_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=20\n"
    .set epoll_s20_failure_len, . - epoll_s20_failure
epoll_s21_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=21\n"
    .set epoll_s21_failure_len, . - epoll_s21_failure
epoll_s22_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=22\n"
    .set epoll_s22_failure_len, . - epoll_s22_failure
epoll_s23_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=23\n"
    .set epoll_s23_failure_len, . - epoll_s23_failure
epoll_s24_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=24\n"
    .set epoll_s24_failure_len, . - epoll_s24_failure
epoll_s25_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=25\n"
    .set epoll_s25_failure_len, . - epoll_s25_failure
epoll_s26_failure:
    .ascii "AETHER_LINUX_EPOLL_BLOCK_FAIL stage=26\n"
    .set epoll_s26_failure_len, . - epoll_s26_failure
timerfd_message:
    .ascii "AETHER_LINUX_TIMERFD_OK oneshot=ok periodic=ok poll=ok epoll=ok\n"
    .set timerfd_message_len, . - timerfd_message
timerfd_s1_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=1\n"
    .set timerfd_s1_failure_len, . - timerfd_s1_failure
timerfd_s2_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=2\n"
    .set timerfd_s2_failure_len, . - timerfd_s2_failure
timerfd_s3_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=3\n"
    .set timerfd_s3_failure_len, . - timerfd_s3_failure
timerfd_s4_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=4\n"
    .set timerfd_s4_failure_len, . - timerfd_s4_failure
timerfd_s5_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=5\n"
    .set timerfd_s5_failure_len, . - timerfd_s5_failure
timerfd_s6_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=6\n"
    .set timerfd_s6_failure_len, . - timerfd_s6_failure
timerfd_s7_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=7\n"
    .set timerfd_s7_failure_len, . - timerfd_s7_failure
timerfd_s8_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=8\n"
    .set timerfd_s8_failure_len, . - timerfd_s8_failure
timerfd_s9_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=9\n"
    .set timerfd_s9_failure_len, . - timerfd_s9_failure
timerfd_s10_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=10\n"
    .set timerfd_s10_failure_len, . - timerfd_s10_failure
timerfd_s11_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=11\n"
    .set timerfd_s11_failure_len, . - timerfd_s11_failure
timerfd_s12_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=12\n"
    .set timerfd_s12_failure_len, . - timerfd_s12_failure
timerfd_s13_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=13\n"
    .set timerfd_s13_failure_len, . - timerfd_s13_failure
timerfd_s14_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=14\n"
    .set timerfd_s14_failure_len, . - timerfd_s14_failure
timerfd_s15_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=15\n"
    .set timerfd_s15_failure_len, . - timerfd_s15_failure
timerfd_s16_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=16\n"
    .set timerfd_s16_failure_len, . - timerfd_s16_failure
timerfd_s17_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=17\n"
    .set timerfd_s17_failure_len, . - timerfd_s17_failure
timerfd_s18_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=18\n"
    .set timerfd_s18_failure_len, . - timerfd_s18_failure
timerfd_s19_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=19\n"
    .set timerfd_s19_failure_len, . - timerfd_s19_failure
timerfd_s20_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=20\n"
    .set timerfd_s20_failure_len, . - timerfd_s20_failure
timerfd_s21_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=21\n"
    .set timerfd_s21_failure_len, . - timerfd_s21_failure
timerfd_s22_failure:
    .ascii "AETHER_LINUX_TIMERFD_FAIL stage=22\n"
    .set timerfd_s22_failure_len, . - timerfd_s22_failure
mremap_message:
    .ascii "AETHER_LINUX_MREMAP_OK grow=ok data=ok\n"
    .set mremap_message_len, . - mremap_message
mremap_failure:
    .ascii "AETHER_LINUX_MREMAP_FAIL\n"
    .set mremap_failure_len, . - mremap_failure
signal_message:
    .ascii "AETHER_LINUX_SIGNAL_OK handler=ok return=ok\n"
    .set signal_message_len, . - signal_message
cross_signal_message:
    .ascii "AETHER_LINUX_CROSS_PROCESS_SIGNAL_OK fork=ok kill=ok handler=received\n"
    .set cross_signal_message_len, . - cross_signal_message
signal_failure:
    .ascii "AETHER_LINUX_SIGNAL_FAIL\n"
    .set signal_failure_len, . - signal_failure
fork_message:
    .ascii "AETHER_LINUX_FORK_OK fork=ok wait4=ok status=ok\nAETHER_LINUX_PROCESS_GROUP_OK setpgid=ok getpgid=ok setsid=ok kill_group=ok\n"
    .set fork_message_len, . - fork_message
fork_failure:
    .ascii "AETHER_LINUX_FORK_FAIL\n"
    .set fork_failure_len, . - fork_failure

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

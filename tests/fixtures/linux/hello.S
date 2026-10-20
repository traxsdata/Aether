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

    /* ---- siginfo/ucontext, alternate stack, register restore ---- */
    mov $9, %rax                /* mmap data page */
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_siginfo
    mov %rax, %r12
    mov $9, %rax                /* mmap alternate stack */
    xor %rdi, %rdi
    mov $8192, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js fail_siginfo
    mov %rax, %r13

    /* sigaltstack{ss_sp = r13+4096, flags=0, ss_size=4096} */
    lea 4096(%r13), %rax
    mov %rax, 0x80(%r12)
    movl $0, 0x88(%r12)
    movl $0, 0x8c(%r12)
    movq $4096, 0x90(%r12)
    mov $131, %rax
    lea 0x80(%r12), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_siginfo

    /* SIGUSR1 with SA_SIGINFO|SA_ONSTACK|SA_RESTORER. */
    lea siginfo_handler(%rip), %rax
    mov %rax, 0xa0(%r12)
    movq $0x0c000004, 0xa8(%r12)
    lea signal_restorer(%rip), %rax
    mov %rax, 0xb0(%r12)
    movq $0, 0xb8(%r12)
    mov $13, %rax
    mov $10, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_siginfo

    /* Block SIGUSR2 (bit 12, 0x800) to check mask preservation. */
    movq $0x800, 0xc0(%r12)
    mov $14, %rax
    mov $2, %rdi
    lea 0xc0(%r12), %rsi
    lea 0xc8(%r12), %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_siginfo

    mov $39, %rax               /* getpid */
    syscall
    mov %rax, 0xd0(%r12)
    movabs $0x1122334455667788, %rbx
    movabs $0x8877665544332211, %r15
    mov %r13, %r14

    movl $1, 0xe0(%r12)
    /* kill(getpid, SIGUSR1) -> si_code == SI_USER (0). */
    mov %rax, 0x00(%r12)        /* placeholder, replaced below */
    lea 11f(%rip), %rax
    mov %rax, 0x00(%r12)
    mov 0xd0(%r12), %rdi
    mov $10, %rsi
    mov $62, %rax
    syscall
11:
    movl $2, 0xe0(%r12)
    test %rax, %rax
    jne fail_siginfo
    movl $3, 0xe0(%r12)
    cmpl $1, 0x48(%r12)
    jne fail_siginfo
    cmpq $10, 0x20(%r12)
    jne fail_siginfo
    cmpq $0, 0x28(%r12)
    jne fail_siginfo
    mov 0xd0(%r12), %rax
    cmpq %rax, 0x30(%r12)
    jne fail_siginfo
    cmpq $0, 0x10(%r12)
    je fail_siginfo
    cmpq $0, 0x18(%r12)
    je fail_siginfo
    mov 0x00(%r12), %rax
    cmpq %rax, 0x38(%r12)
    jne fail_siginfo
    movl $4, 0xe0(%r12)
    cmpq $0x800, 0x40(%r12)
    jne fail_siginfo
    movl $5, 0xe0(%r12)
    mov 0x08(%r12), %rax
    lea 4096(%r13), %rcx
    cmp %rcx, %rax
    jb fail_siginfo
    lea 8192(%r13), %rcx
    cmp %rcx, %rax
    jae fail_siginfo
    lea 4096(%r13), %rcx
    cmpq %rcx, 0x58(%r12)
    jne fail_siginfo
    cmpl $1, 0x60(%r12)
    jne fail_siginfo
    movabs $0x1122334455667788, %rax
    cmp %rax, %rbx
    jne fail_siginfo
    movabs $0x8877665544332211, %rax
    cmp %rax, %r15
    jne fail_siginfo
    cmp %r13, %r14
    jne fail_siginfo

    movl $7, 0xe0(%r12)
    /* tgkill(getpid, getpid, SIGUSR1) -> si_code == SI_TKILL (-6). */
    lea 12f(%rip), %rax
    mov %rax, 0x00(%r12)
    mov 0xd0(%r12), %rdi
    mov %rdi, %rsi
    mov $10, %rdx
    mov $234, %rax
    syscall
12:
    movl $8, 0xe0(%r12)
    test %rax, %rax
    jne fail_siginfo
    cmpl $2, 0x48(%r12)
    jne fail_siginfo
    cmpq $-6, 0x28(%r12)
    jne fail_siginfo
    mov 0x00(%r12), %rax
    cmpq %rax, 0x38(%r12)
    jne fail_siginfo

    /* Disable the alternate stack: delivery must fall back to the normal
     * stack and report SS_DISABLE in ucontext.uc_stack. */
    movq $0, 0x80(%r12)
    movl $2, 0x88(%r12)
    movl $0, 0x8c(%r12)
    movq $0, 0x90(%r12)
    mov $131, %rax
    lea 0x80(%r12), %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_siginfo
    lea 13f(%rip), %rax
    mov %rax, 0x00(%r12)
    mov 0xd0(%r12), %rdi
    mov $10, %rsi
    mov $62, %rax
    syscall
13:
    movl $9, 0xe0(%r12)
    test %rax, %rax
    jne fail_siginfo
    cmpl $3, 0x48(%r12)
    jne fail_siginfo
    mov 0x08(%r12), %rax
    lea 4096(%r13), %rcx
    cmp %rcx, %rax
    jb 33f
    lea 8192(%r13), %rcx
    cmp %rcx, %rax
    jb fail_siginfo
33:
    cmpl $2, 0x60(%r12)
    jne fail_siginfo

    /* The blocked mask must survive handler delivery and return. */
    mov $14, %rax
    xor %rdi, %rdi
    xor %rsi, %rsi
    lea 0xd8(%r12), %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_siginfo
    cmpq $0x800, 0xd8(%r12)
    jne fail_siginfo
    movq $0, 0xc0(%r12)         /* restore an empty mask */
    mov $14, %rax
    mov $2, %rdi
    lea 0xc0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_siginfo

    mov $1, %rax
    mov $1, %rdi
    lea siginfo_message(%rip), %rsi
    mov $siginfo_message_len, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea altstack_message(%rip), %rsi
    mov $altstack_message_len, %rdx
    syscall

    /* ---- SA_RESTART wait4 vs EINTR ---- */
    lea restart_handler(%rip), %rax
    mov %rax, 0xa0(%r12)
    movq $0x14000000, 0xa8(%r12) /* SA_RESTORER|SA_RESTART */
    lea signal_restorer(%rip), %rax
    mov %rax, 0xb0(%r12)
    movq $0, 0xb8(%r12)
    mov $13, %rax
    mov $10, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_restart
    movl $0, 0x500(%r12)

    sub $16, %rsp
    mov $57, %rax               /* fork restart child */
    syscall
    test %rax, %rax
    js fail_restart
    jz restart_child
    mov %rax, %r14
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax               /* blocking wait4 with SA_RESTART */
    syscall
    cmp %r14, %rax
    jne fail_restart
    cmpl $0x0700, (%rsp)
    jne fail_restart
    add $16, %rsp
    cmpl $1, 0x500(%r12)
    jne fail_restart

    /* Now a handler without SA_RESTART must return EINTR (-4). */
    movq $0x04000000, 0xa8(%r12)
    mov $13, %rax
    mov $10, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_restart
    sub $16, %rsp
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_restart
    jz interrupt_child
    mov %rax, %r14
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax
    syscall
    cmp $-4, %rax
    jne fail_restart
    cmpl $2, 0x500(%r12)
    jne fail_restart
    mov %r14, %rdi              /* reap the child after EINTR */
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax
    syscall
    cmp %r14, %rax
    jne fail_restart
    cmpl $0x0800, (%rsp)
    jne fail_restart
    add $16, %rsp
    mov $1, %rax
    mov $1, %rdi
    lea restart_message(%rip), %rsi
    mov $restart_message_len, %rdx
    syscall

    jmp after_signals

restart_child:
    mov $110, %rax              /* getppid */
    syscall
    mov %rax, %r13
    mov $2000000, %ecx
14:
    dec %ecx
    jne 14b
    mov %r13, %rdi
    mov $10, %rsi
    mov $62, %rax
    syscall
    mov $2000000, %ecx
15:
    dec %ecx
    jne 15b
    mov $60, %rax
    mov $7, %rdi
    syscall
    jmp 15b

interrupt_child:
    mov $110, %rax
    syscall
    mov %rax, %r13
    mov $2000000, %ecx
16:
    dec %ecx
    jne 16b
    mov %r13, %rdi
    mov $10, %rsi
    mov $62, %rax
    syscall
    mov $2000000, %ecx
17:
    dec %ecx
    jne 17b
    mov $60, %rax
    mov $8, %rdi
    syscall
    jmp 17b

after_signals:
    /* ---- SIGTERM handler and SIGKILL default action ---- */
    sub $16, %rsp
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_default
    jz term_child
    mov %rax, %r14
    mov $10000000, %ecx
18:
    dec %ecx
    jne 18b
    mov %r14, %rdi
    mov $15, %rsi               /* SIGTERM */
    mov $62, %rax
    syscall
    test %rax, %rax
    jne fail_default
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax
    syscall
    cmp %r14, %rax
    jne fail_default
    cmpl $0x2e00, (%rsp)
    jne fail_default
    add $16, %rsp

    sub $16, %rsp
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_default
    jz kill_child
    mov %rax, %r14
    mov $10000000, %ecx
19:
    dec %ecx
    jne 19b
    mov %r14, %rdi
    mov $9, %rsi                /* SIGKILL default action */
    mov $62, %rax
    syscall
    test %rax, %rax
    jne fail_default
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax
    syscall
    cmp %r14, %rax
    jne fail_default
    cmpl $9, (%rsp)             /* WIFSIGNALED: status == SIGKILL */
    jne fail_default
    add $16, %rsp
    mov $1, %rax
    mov $1, %rdi
    lea default_message(%rip), %rsi
    mov $default_message_len, %rdx
    syscall
    jmp after_default

term_child:
    lea term_handler(%rip), %rax
    mov %rax, 0xa0(%r12)
    movq $0x04000000, 0xa8(%r12)
    lea signal_restorer(%rip), %rax
    mov %rax, 0xb0(%r12)
    movq $0, 0xb8(%r12)
    mov $13, %rax
    mov $15, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_default
    movl $0, 0x530(%r12)
    mov $20000000, %ecx
20:
    dec %ecx
    jne 20b
    mov $60, %rax
    cmpl $1, 0x530(%r12)
    jne 21f
    mov $46, %rdi
    syscall
21:
    mov $1, %rdi
    syscall
    jmp 21b

kill_child:
    mov $20000000, %ecx
22:
    dec %ecx
    jne 22b
    mov $60, %rax
    mov $1, %rdi
    syscall
    jmp 22b

after_default:
    /* ---- process-group scoped kill and setpgid/setsid validation ---- */
    /* Ignore SIGUSR1 in the parent; the group signal must only reach the
     * child that moved itself into a new group. */
    movq $1, 0xa0(%r12)         /* SIG_IGN */
    movq $0, 0xa8(%r12)
    movq $0, 0xb0(%r12)
    movq $0, 0xb8(%r12)
    mov $13, %rax
    mov $10, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_group
    mov $121, %rax              /* getpgid(0) == parent group */
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    js fail_group
    mov %rax, 0x520(%r12)

    sub $16, %rsp
    mov $57, %rax
    syscall
    test %rax, %rax
    js fail_group
    jz group_child
    mov %rax, %r14              /* child becomes its own group leader */
    mov $10000000, %ecx
23:
    dec %ecx
    jne 23b
    mov %r14, %rdi
    neg %rdi                    /* kill(-child_pgid, SIGUSR1) */
    mov $10, %rsi
    mov $62, %rax
    syscall
    test %rax, %rax
    jne fail_group
    mov $5000000, %ecx
24:
    dec %ecx
    jne 24b
    mov 0x520(%r12), %rdi
    neg %rdi                    /* parent group: ignored locally */
    mov $10, %rsi
    mov $62, %rax
    syscall
    test %rax, %rax
    jne fail_group
    mov %r14, %rdi
    mov %rsp, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    mov $61, %rax
    syscall
    cmp %r14, %rax
    jne fail_group
    cmpl $0x0100, (%rsp)        /* handler ran exactly once */
    jne fail_group
    add $16, %rsp
    mov $1, %rax
    mov $1, %rdi
    lea group_extra_message(%rip), %rsi
    mov $group_extra_message_len, %rdx
    syscall
    jmp after_group

group_child:
    mov $109, %rax              /* setpgid(0, 0): own group */
    xor %rdi, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne fail_group
    mov $39, %rax               /* getpid == getpgid after setpgid(0,0) */
    syscall
    mov %rax, 0x528(%r12)
    mov $121, %rax
    xor %rdi, %rdi
    syscall
    cmp 0x528(%r12), %rax
    jne fail_group
    /* A session leader cannot join another group. */
    mov $109, %rax
    xor %rdi, %rdi
    mov 0x520(%r12), %rsi
    syscall
    cmp $-1, %rax               /* -EPERM */
    jne fail_group
    /* Negative/huge pgid is EINVAL; unknown pid is ESRCH. */
    mov $109, %rax
    xor %rdi, %rdi
    mov $0x7fffffff, %rsi
    syscall
    cmp $-22, %rax
    jne fail_group
    mov $109, %rax
    mov $0x7fffffff, %rdi
    xor %rsi, %rsi
    syscall
    cmp $-3, %rax
    jne fail_group
    mov $121, %rax
    mov $0x7fffffff, %rdi
    syscall
    cmp $-3, %rax
    jne fail_group
    lea group_handler(%rip), %rax
    mov %rax, 0xa0(%r12)
    movq $0x04000000, 0xa8(%r12)
    lea signal_restorer(%rip), %rax
    mov %rax, 0xb0(%r12)
    movq $0, 0xb8(%r12)
    mov $13, %rax
    mov $10, %rdi
    lea 0xa0(%r12), %rsi
    xor %rdx, %rdx
    mov $8, %r10
    syscall
    test %rax, %rax
    jne fail_group
    movl $0, 0x510(%r12)
    mov $40000000, %ecx
25:
    dec %ecx
    jne 25b
    mov $60, %rax
    mov 0x510(%r12), %rdi
    syscall
    jmp 25b

after_group:
    /* ---- bounded rseq registration ---- */
    movq $0, 0x3c0(%r12)
    movq $0, 0x3c8(%r12)
    movq $0, 0x3d0(%r12)
    movq $0, 0x3d8(%r12)
    mov $334, %rax
    lea 0x3c0(%r12), %rdi
    mov $32, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_rseq
    mov $334, %rax
    lea 0x3c0(%r12), %rdi
    mov $16, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    cmp $-22, %rax
    jne fail_rseq
    mov $334, %rax
    xor %rdi, %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    jne fail_rseq
    mov $1, %rax
    mov $1, %rdi
    lea rseq_message(%rip), %rsi
    mov $rseq_message_len, %rdx
    syscall

    /* ---- clone3 thread ---- */
    movl $0, 0x208(%r12)
    movl $0, 0x218(%r12)
    movq $0x1290100, 0x300(%r12)
    movq $0, 0x308(%r12)
    lea 0x218(%r12), %rax
    mov %rax, 0x310(%r12)
    movq $0, 0x318(%r12)
    movq $0, 0x320(%r12)
    lea 0x800(%r12), %rax
    mov %rax, 0x328(%r12)
    movq $0x400, 0x330(%r12)
    lea 0x200(%r12), %rax
    mov %rax, 0x338(%r12)
    movq $0, 0x340(%r12)
    movq $0, 0x348(%r12)
    movq $0, 0x350(%r12)
    movq $0, 0x358(%r12)
    movq $0, 0x360(%r12)
    movq $0, 0x368(%r12)
    movq $0, 0x370(%r12)
    movq $0, 0x378(%r12)
    movl $1, 0xe4(%r12)
    /* A short layout is EINVAL; a namespace flag is ENOSYS. */
    mov $435, %rax
    lea 0x300(%r12), %rdi
    mov $32, %rsi
    syscall
    cmp $-22, %rax
    jne fail_clone3
    movl $2, 0xe4(%r12)
    movq $0x1290100, %rdx
    orq $0x20000, %rdx          /* CLONE_NEWNS */
    mov %rdx, 0x300(%r12)
    mov $435, %rax
    lea 0x300(%r12), %rdi
    mov $88, %rsi
    syscall
    cmp $-38, %rax
    jne fail_clone3
    movl $3, 0xe4(%r12)
    movq $0x1290100, 0x300(%r12)
    mov $435, %rax
    lea 0x300(%r12), %rdi
    mov $88, %rsi
    syscall
    test %rax, %rax
    js fail_clone3
    jz clone3_thread
    mov %rax, %r14
    movl $4, 0xe4(%r12)
26:
    movl 0x218(%r12), %eax
    test %eax, %eax
    jz 34f
    mov $202, %rax
    lea 0x218(%r12), %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    jmp 26b
34:
    movl $5, 0xe4(%r12)
    cmpl $1, 0x208(%r12)
    jne fail_clone3
    jmp clone3_done
clone3_done:
    mov $1, %rax
    mov $1, %rdi
    lea clone3_message(%rip), %rsi
    mov $clone3_message_len, %rdx
    syscall

    /* ---- robust futex list: owner exits, waiter sees OWNER_DIED ---- */
    /* Main thread registration round trip. */
    movl $2, 0xe8(%r12)
    mov $273, %rax
    lea 0x480(%r12), %rdi
    mov $24, %rsi
    syscall
    test %rax, %rax
    je 60f
    cmp $-14, %rax
    je 61f
    cmp $-22, %rax
    je 62f
    jmp robust_fail
61:
    movl $6, 0xe8(%r12)
    jmp robust_fail
62:
    movl $7, 0xe8(%r12)
    jmp robust_fail
60:
    movl $5, 0xe8(%r12)
    mov $274, %rax
    xor %rdi, %rdi
    lea 0x488(%r12), %rsi
    lea 0x490(%r12), %rdx
    syscall
    test %rax, %rax
    jne robust_fail
    movl $6, 0xe8(%r12)
    cmpq $0, 0x488(%r12)
    jne 63f
    movl $7, 0xe8(%r12)
    jmp robust_fail
63:
    lea 0x480(%r12), %rax
    cmpq %rax, 0x488(%r12)
    je 64f
    movq 0x488(%r12), %rax
    sub $32, %rsp
    mov %rsp, %rdi
    mov $16, %ecx
65:
    rol $4, %rax
    mov %al, %dl
    and $0xf, %dl
    cmp $10, %dl
    jb 66f
    add $39, %dl
66:
    add $48, %dl
    mov %dl, (%rdi)
    inc %rdi
    dec %ecx
    jne 65b
    mov $1, %rax
    mov $1, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea failure_newline(%rip), %rsi
    mov $1, %rdx
    syscall
    mov $60, %rax
    mov $1, %rdi
    syscall
    jmp .
64:
    cmpq $24, 0x490(%r12)
    je 67f
    jmp robust_fail
67:
    mov $273, %rax              /* unregister */
    xor %rdi, %rdi
    xor %rsi, %rsi
    syscall
    test %rax, %rax
    jne robust_fail
    movl $0, 0x440(%r12)
    movl $0, 0x448(%r12)
    movl $0, 0x44c(%r12)
    movl $0, 0x450(%r12)
    movl $0, 0x454(%r12)
    movl $0, 0x458(%r12)
    movl $0, 0x46c(%r12)
    lea 0x420(%r12), %rax
    mov %rax, 0x400(%r12)       /* head.next = entry */
    movq $0x20, 0x408(%r12)     /* futex_offset = word - entry */
    movq $0, 0x410(%r12)
    lea 0x400(%r12), %rax
    mov %rax, 0x420(%r12)       /* entry.next = head */
    movl $0, 0x218(%r12)
    mov $56, %rax               /* clone owner thread */
    mov $0x1290100, %rdi
    lea 0xc00(%r12), %rsi
    xor %rdx, %rdx
    lea 0x218(%r12), %r10
    xor %r8, %r8
    syscall
    test %rax, %rax
    js fail_robust
    jz robust_owner
28:
    cmpl $1, 0x448(%r12)
    jne 28b
    movl $0, 0x218(%r12)
    mov $56, %rax               /* clone waiter thread */
    mov $0x1290100, %rdi
    lea 0xf00(%r12), %rsi
    xor %rdx, %rdx
    lea 0x21c(%r12), %r10
    xor %r8, %r8
    syscall
    test %rax, %rax
    js fail_robust
    jz robust_waiter
29:
    cmpl $1, 0x450(%r12)
    jne 29b
    mov $1000000, %ecx
30:
    dec %ecx
    jne 30b
    movl $1, 0x44c(%r12)
    mov $202, %rax
    lea 0x44c(%r12), %rdi
    mov $1, %rsi
    mov $1, %rdx
    syscall
    sub $16, %rsp
    movq $1, 0(%rsp)
    movq $0, 8(%rsp)
    cmpl $0, 0x458(%r12)
    jne 31f
    mov $202, %rax
    lea 0x458(%r12), %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    mov %rsp, %r10
    syscall
31:
    add $16, %rsp
    movl $5, 0xe8(%r12)
    cmpl $1, 0x458(%r12)
    jne fail_robust
    movl $7, 0xe8(%r12)
    cmpl $1, 0x454(%r12)
    jne fail_robust
    mov $1, %rax
    mov $1, %rdi
    lea robust_message(%rip), %rsi
    mov $robust_message_len, %rdx
    syscall

    jmp after_new_signals

clone3_thread:
    movl $1, 0x208(%r12)
    mov $60, %rax
    xor %rdi, %rdi
    syscall
    jmp clone3_thread

robust_owner:
    movl $1, 0xe8(%r12)
    mov $186, %rax              /* gettid */
    syscall
    mov %eax, 0x440(%r12)       /* futex word names this thread */
    mov $273, %rax              /* set_robust_list(head, 24) */
    lea 0x400(%r12), %rdi
    mov $24, %rsi
    syscall
    test %rax, %rax
    jne robust_fail
    movl $1, 0x448(%r12)
    mov $202, %rax
    lea 0x448(%r12), %rdi
    mov $1, %rsi
    mov $1, %rdx
    syscall
32:
    cmpl $1, 0x44c(%r12)
    jne 32b
    mov $60, %rax
    xor %rdi, %rdi
    syscall
    jmp 32b

robust_waiter:
    movl $3, 0xe8(%r12)
33:
    cmpl $1, 0x448(%r12)
    jne 33b
    movl $1, 0x450(%r12)
    mov 0x440(%r12), %edx
    lea 0x440(%r12), %rdi
    xor %rsi, %rsi
    xor %r10, %r10
    mov $202, %rax
    syscall
    movl $4, 0xe8(%r12)
    mov 0x440(%r12), %eax
    mov %eax, %ecx
    and $0x40000000, %ecx
    jz robust_waiter_done
    and $0x3fffffff, %eax
    jnz robust_waiter_done
    movl $1, 0x454(%r12)
robust_waiter_done:
    movl $1, 0x458(%r12)
    mov $202, %rax
    lea 0x458(%r12), %rdi
    mov $1, %rsi
    mov $1, %rdx
    syscall
    mov $60, %rax
    xor %rdi, %rdi
    syscall
    jmp robust_waiter_done

robust_fail:
    jmp fail_robust

after_new_signals:
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
fail_siginfo:
    movl 0xe0(%r12), %eax
    add $48, %al
    sub $16, %rsp
    mov %al, (%rsp)
    mov $1, %rax
    mov $1, %rdi
    lea siginfo_failure(%rip), %rsi
    mov $siginfo_failure_len, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    mov %rsp, %rsi
    mov $1, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea failure_newline(%rip), %rsi
    mov $1, %rdx
    syscall
    mov $60, %rax
    mov $1, %rdi
    syscall
    jmp .
fail_restart:
    lea restart_failure(%rip), %rsi
    mov $restart_failure_len, %rdx
    jmp report_failure
fail_default:
    lea default_failure(%rip), %rsi
    mov $default_failure_len, %rdx
    jmp report_failure
fail_group:
    lea group_failure(%rip), %rsi
    mov $group_failure_len, %rdx
    jmp report_failure
fail_rseq:
    lea rseq_failure(%rip), %rsi
    mov $rseq_failure_len, %rdx
    jmp report_failure
fail_clone3:
    movl 0xe4(%r12), %eax
    add $48, %al
    sub $16, %rsp
    mov %al, (%rsp)
    mov $1, %rax
    mov $1, %rdi
    lea clone3_failure(%rip), %rsi
    mov $clone3_failure_len, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    mov %rsp, %rsi
    mov $1, %rdx
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea failure_newline(%rip), %rsi
    mov $1, %rdx
    syscall
    mov $60, %rax
    mov $1, %rdi
    syscall
    jmp .
fail_robust:
    movl 0xe8(%r12), %eax
    cmp $0, %eax
    je 40f
    cmp $1, %eax
    je 41f
    cmp $2, %eax
    je 42f
    cmp $3, %eax
    je 43f
    cmp $4, %eax
    je 44f
    cmp $5, %eax
    je 45f
    cmp $6, %eax
    je 46f
    cmp $7, %eax
    je 47f
    lea robust_failure(%rip), %rsi
    mov $robust_failure_len, %rdx
    jmp report_failure
40:
    lea robust_f0(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
41:
    lea robust_f1(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
42:
    lea robust_f2(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
43:
    lea robust_f3(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
44:
    lea robust_f4(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
45:
    lea robust_f5(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
46:
    lea robust_f6(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
47:
    lea robust_f7(%rip), %rsi
    mov $robust_f_len, %rdx
    jmp report_failure
fail_fork:
    lea fork_failure(%rip), %rsi
    mov $fork_failure_len, %rdx
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
siginfo_message:
    .ascii "AETHER_LINUX_SIGINFO_OK siginfo=ok ucontext=ok regs=ok\n"
    .set siginfo_message_len, . - siginfo_message
altstack_message:
    .ascii "AETHER_LINUX_ALTSTACK_OK onstack=ok fallback=ok\n"
    .set altstack_message_len, . - altstack_message
restart_message:
    .ascii "AETHER_LINUX_RESTART_OK sa_restart=ok eintr=ok\n"
    .set restart_message_len, . - restart_message
default_message:
    .ascii "AETHER_LINUX_SIGNAL_DEFAULT_OK sigterm=handler sigkill=default\n"
    .set default_message_len, . - default_message
group_extra_message:
    .ascii "AETHER_LINUX_PROCESS_GROUP_KILL_OK group=scoped eperm=ok esrch=ok einval=ok\n"
    .set group_extra_message_len, . - group_extra_message
rseq_message:
    .ascii "AETHER_LINUX_RSEQ_OK register=ok validation=ok\n"
    .set rseq_message_len, . - rseq_message
rseq_failure:
    .ascii "AETHER_LINUX_RSEQ_FAIL\n"
    .set rseq_failure_len, . - rseq_failure
clone3_message:
    .ascii "AETHER_LINUX_CLONE3_OK clone3=ok join=ok enosys=ok\n"
    .set clone3_message_len, . - clone3_message
robust_message:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_OK register=ok owner_died=ok wake=ok\n"
    .set robust_message_len, . - robust_message
siginfo_failure:
    .ascii "AETHER_LINUX_SIGINFO_FAIL stage="
    .set siginfo_failure_len, . - siginfo_failure
failure_newline:
    .byte 10
restart_failure:
    .ascii "AETHER_LINUX_RESTART_FAIL\n"
    .set restart_failure_len, . - restart_failure
default_failure:
    .ascii "AETHER_LINUX_SIGNAL_DEFAULT_FAIL\n"
    .set default_failure_len, . - default_failure
group_failure:
    .ascii "AETHER_LINUX_PROCESS_GROUP_KILL_FAIL\n"
    .set group_failure_len, . - group_failure
clone3_failure:
    .ascii "AETHER_LINUX_CLONE3_FAIL stage="
    .set clone3_failure_len, . - clone3_failure
robust_failure:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL\n"
    .set robust_failure_len, . - robust_failure
robust_f0:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=0\n"
robust_f1:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=1\n"
robust_f2:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=2\n"
robust_f3:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=3\n"
robust_f4:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=4\n"
robust_f5:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=5\n"
robust_f6:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=6\n"
robust_f7:
    .ascii "AETHER_LINUX_ROBUST_FUTEX_FAIL stage=7\n"
    .set robust_f_len, 39
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

siginfo_handler:
    /* rdi=signo, rsi=&siginfo, rdx=&ucontext */
    mov %rsp, 0x08(%r12)
    mov %rsi, 0x10(%r12)
    mov %rdx, 0x18(%r12)
    movslq 0(%rsi), %rax
    mov %rax, 0x20(%r12)
    movslq 8(%rsi), %rax
    mov %rax, 0x28(%r12)
    movslq 16(%rsi), %rax
    mov %rax, 0x30(%r12)
    mov 168(%rdx), %rax         /* ucontext.uc_mcontext.rip */
    mov %rax, 0x38(%r12)
    mov 296(%rdx), %rax         /* ucontext.uc_sigmask */
    mov %rax, 0x40(%r12)
    mov 16(%rdx), %rax          /* ucontext.uc_stack.ss_sp */
    mov %rax, 0x58(%r12)
    mov 24(%rdx), %eax          /* ucontext.uc_stack.ss_flags */
    mov %rax, 0x60(%r12)
    incl 0x48(%r12)
    movq $0xdeadbeef, %rbx
    movq $0xdeadbeef, %r13
    movq $0xdeadbeef, %r14
    movq $0xdeadbeef, %r15
    ret

restart_handler:
    incl 0x500(%r12)
    ret

term_handler:
    movl $1, 0x530(%r12)
    ret

group_handler:
    incl 0x510(%r12)
    ret
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

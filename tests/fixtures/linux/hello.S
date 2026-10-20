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

    /* init1(flags=0) requests default blocking mode and must succeed. */
    mov $294, %rax
    xor %rdi, %rdi
    syscall
    test %rax, %rax
    js fail_inotify_flags
    mov %rax, %r15
    mov $3, %rax
    mov %r15, %rdi
    syscall

    /* Nonblocking mode watches the overlay file for IN_MODIFY. */
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

    /* ---- Gate 2: advanced inotify, pseudo-files, policy, quotas ---- */
    sub $1024, %rsp
    /* Nonblocking instance, directory watches on /tmp and /etc. */
    mov $294, %rax
    mov $0x800, %rdi
    syscall
    test %rax, %rax
    js g2_fail_inotify
    mov %rax, %r12
    mov $254, %rax
    mov %r12, %rdi
    lea g2_tmp_path(%rip), %rsi
    mov $0x12e, %rdx            /* CREATE|MODIFY|ATTRIB|OPEN|CLOSE_WRITE */
    syscall
    cmp $1, %rax
    jne g2_fail_inotify
    mov %eax, 904(%rsp)
    mov $254, %rax
    mov %r12, %rdi
    lea g2_etc_path(%rip), %rsi
    mov $0x24, %rdx             /* ATTRIB|OPEN */
    syscall
    cmp $2, %rax
    jne g2_fail_inotify
    mov %eax, 900(%rsp)

    /* Create /tmp/q1: the /tmp watch must report IN_CREATE with a name. */
    mov $257, %rax
    mov $-100, %rdi
    lea g2_q1_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js g2_fail_inotify
    mov %rax, %r13
    mov $1, %rax
    mov %r13, %rdi
    lea g2_byte(%rip), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne g2_fail_inotify
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $512, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify
    mov %rax, %r15
    mov %rsp, %rdi
    mov %r15, %rsi
    mov $0x100, %edx            /* IN_CREATE */
    call g2_scan_mask
    test %rax, %rax
    jz g2_fail_inotify

    /* Open and read /etc/motd: the /etc watch must report IN_OPEN. */
    mov $257, %rax
    mov $-100, %rdi
    lea g2_motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_inotify
    mov %rax, %r13
    mov $0, %rax
    mov %r13, %rdi
    lea 512(%rsp), %rsi
    mov $16, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $512, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify
    mov %rax, %r15
    mov %rsp, %rdi
    mov %r15, %rsi
    mov $0x20, %edx             /* IN_OPEN */
    call g2_scan_mask
    test %rax, %rax
    jz g2_fail_inotify

    /* Removing the /etc watch reports IN_IGNORED for its descriptor. */
    mov $255, %rax
    mov %r12, %rdi
    mov 900(%rsp), %esi
    syscall
    test %rax, %rax
    jne g2_fail_inotify
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $256, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify
    mov %rax, %r15
    mov %rsp, %rdi
    mov %r15, %rsi
    mov $0x8000, %edx           /* IN_IGNORED */
    call g2_scan_mask
    test %rax, %rax
    jz g2_fail_inotify

    /* Overflow: watch /tmp/q1 for IN_MODIFY and queue more than the bound. */
    mov $254, %rax
    mov %r12, %rdi
    lea g2_q1_path(%rip), %rsi
    mov $2, %rdx
    syscall
    cmp $3, %rax
    jne g2_fail_inotify
    mov $257, %rax
    mov $-100, %rdi
    lea g2_q1_path(%rip), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_inotify
    mov %rax, %r13
    mov $12, %r14
g2_overflow_write:
    mov $1, %rax
    mov %r13, %rdi
    lea g2_byte(%rip), %rsi
    mov $1, %rdx
    syscall
    cmp $1, %rax
    jne g2_fail_inotify
    dec %r14
    jne g2_overflow_write
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $256, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify
    mov %rax, %r15
    mov %rsp, %rdi
    mov %r15, %rsi
    mov $0x4000, %edx           /* IN_Q_OVERFLOW */
    call g2_scan_mask
    test %rax, %rax
    jz g2_fail_inotify
    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* Blocking mode: a CLONE_VM thread writes the watched file while the
     * parent is blocked in read, and the record completes that read. */
    mov $257, %rax
    mov $-100, %rdi
    lea g2_block_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js g2_fail_inotify_block
    mov %rax, %r13
    mov $3, %rax
    mov %r13, %rdi
    syscall
    mov $294, %rax
    xor %rdi, %rdi              /* default blocking mode */
    syscall
    test %rax, %rax
    js g2_fail_inotify_block
    mov %rax, %r12
    mov $254, %rax
    mov %r12, %rdi
    lea g2_block_path(%rip), %rsi
    mov $2, %rdx
    syscall
    cmp $1, %rax
    jne g2_fail_inotify_block
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x21, %r10             /* SHARED|ANONYMOUS */
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js g2_fail_inotify_block
    mov %rax, %r14
    movl $0, (%r14)
    movl $0, 8(%r14)
    mov $56, %rax
    mov $0x1290100, %rdi        /* VM|THREAD|SETTLS|CLEARTID|CHILD_SETTID */
    lea 0xf00(%r14), %rsi
    xor %rdx, %rdx
    lea 8(%r14), %r10
    lea 0x200(%r14), %r8
    syscall
    test %rax, %rax
    js g2_fail_inotify_block
    jz g2_block_child
    movl $1, (%r14)
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $512, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_inotify_block
    mov %rax, %r15
    mov %rsp, %rdi
    mov %r15, %rsi
    mov $2, %edx
    call g2_scan_mask
    test %rax, %rax
    jz g2_fail_inotify_block
g2_block_join:
    cmpl $0, 8(%r14)
    je g2_block_joined
    mov $202, %rax
    lea 8(%r14), %rdi
    xor %rsi, %rsi
    syscall
    jmp g2_block_join
g2_block_joined:
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $11, %rax
    mov %r14, %rdi
    mov $4096, %rsi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea g2_inotify_block_message(%rip), %rsi
    mov $g2_inotify_block_message_len, %rdx
    syscall

    /* Pseudo-files: /proc/self/exe, maps, status, fd directory, /dev nodes. */
    sub $512, %rsp
    mov $257, %rax
    mov $-100, %rdi
    lea g2_proc_exe(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_proc
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $4, %rdx
    syscall
    cmp $4, %rax
    jne g2_fail_proc
    cmpl $0x464c457f, (%rsp)
    jne g2_fail_proc
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_proc_maps(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_proc
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $400, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_proc
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_proc_status(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_proc
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $400, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_proc
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_proc_fd(%rip), %rsi
    mov $0x10000, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_proc
    mov %rax, %r12
    mov $217, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $400, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_proc
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea g2_proc_message(%rip), %rsi
    mov $g2_proc_message_len, %rdx
    syscall

    /* /dev/null, /dev/zero, /dev/random, /dev/urandom. */
    mov $257, %rax
    mov $-100, %rdi
    lea g2_dev_null(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_dev
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    test %rax, %rax
    jne g2_fail_dev
    mov $5, %rax
    mov %r12, %rdi
    lea 128(%rsp), %rsi
    syscall
    test %rax, %rax
    jne g2_fail_dev
    mov 152(%rsp), %eax         /* st_mode */
    and $0xf000, %eax
    cmp $0x2000, %eax           /* S_IFCHR */
    jne g2_fail_dev
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_dev_zero(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_dev
    mov %rax, %r12
    movabs $0x1122334455667788, %rax
    mov %rax, (%rsp)
    mov %rax, 8(%rsp)
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    cmp $16, %rax
    jne g2_fail_dev
    cmpq $0, (%rsp)
    jne g2_fail_dev
    cmpq $0, 8(%rsp)
    jne g2_fail_dev
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_dev_random(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_dev
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    cmp $16, %rax
    jne g2_fail_dev
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $257, %rax
    mov $-100, %rdi
    lea g2_dev_urandom(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_dev
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $16, %rdx
    syscall
    cmp $16, %rax
    jne g2_fail_dev
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea g2_dev_message(%rip), %rsi
    mov $g2_dev_message_len, %rdx
    syscall

    /* Syscall policy: unknown, network, device, and the audit pseudo-file. */
    mov $9999, %rax
    syscall
    cmp $-38, %rax
    jne g2_fail_policy
    mov $41, %rax               /* socket */
    mov $2, %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    cmp $-13, %rax
    jne g2_fail_policy
    mov $16, %rax               /* ioctl */
    xor %rdi, %rdi
    xor %rsi, %rsi
    xor %rdx, %rdx
    syscall
    cmp $-13, %rax
    jne g2_fail_policy
    mov $257, %rax
    mov $-100, %rdi
    lea g2_dev_dri(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    cmp $-13, %rax
    jne g2_fail_policy
    mov $257, %rax
    mov $-100, %rdi
    lea g2_path_audit(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fail_policy
    mov %rax, %r12
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $256, %rdx
    syscall
    test %rax, %rax
    jle g2_fail_policy
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov $1, %rax
    mov $1, %rdi
    lea g2_policy_message(%rip), %rsi
    mov $g2_policy_message_len, %rdx
    syscall

    /* Denial: path traversal and a write to the read-only root. */
    mov $257, %rax
    mov $-100, %rdi
    lea g2_path_escape(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    cmp $-2, %rax
    jne g2_fail_denial
    mov $257, %rax
    mov $-100, %rdi
    lea g2_motd_path(%rip), %rsi
    mov $1, %rdx                /* O_WRONLY */
    xor %r10, %r10
    syscall
    cmp $-30, %rax              /* -EROFS */
    jne g2_fail_denial
    mov $1, %rax
    mov $1, %rdi
    lea g2_denial_message(%rip), %rsi
    mov $g2_denial_message_len, %rdx
    syscall
    add $512, %rsp

    /* Quotas: overlay bytes, descriptor count, then VMA count. */
    sub $1024, %rsp
    mov $257, %rax
    mov $-100, %rdi
    lea g2_q2_path(%rip), %rsi
    mov $0x241, %rdx
    mov $0600, %r10
    syscall
    test %rax, %rax
    js g2_fail_quota
    mov %rax, %r12
    mov $1, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $600, %rdx
    syscall
    cmp $-28, %rax              /* -ENOSPC */
    jne g2_fail_quota
    mov $3, %rax
    mov %r12, %rdi
    syscall
    xor %r14, %r14
g2_fd_loop:
    mov $257, %rax
    mov $-100, %rdi
    lea g2_motd_path(%rip), %rsi
    xor %rdx, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_fd_done
    mov %eax, 512(%rsp,%r14,4)
    inc %r14
    cmp $24, %r14
    jb g2_fd_loop
    jmp g2_fail_quota
g2_fd_done:
    cmp $-24, %rax              /* -EMFILE */
    jne g2_fail_quota
    xor %r15, %r15
g2_fd_close:
    cmp %r14, %r15
    jae g2_fd_closed
    mov 512(%rsp,%r15,4), %edi
    mov $3, %rax
    syscall
    inc %r15
    jmp g2_fd_close
g2_fd_closed:
    xor %r13, %r13
g2_vma_loop:
    mov $9, %rax
    xor %rdi, %rdi
    mov $4096, %rsi
    mov $3, %rdx
    mov $0x22, %r10
    mov $-1, %r8
    xor %r9, %r9
    syscall
    test %rax, %rax
    js g2_vma_done
    inc %r13
    cmp $64, %r13
    jb g2_vma_loop
    jmp g2_fail_quota
g2_vma_done:
    cmp $-12, %rax              /* -ENOMEM */
    jne g2_fail_quota
    mov $1, %rax
    mov $1, %rdi
    lea g2_quota_message(%rip), %rsi
    mov $g2_quota_message_len, %rdx
    syscall
    add $1024, %rsp
    jmp g2_extras_done

g2_block_child:
    cmpl $1, (%r14)
    jne g2_block_child
    mov $257, %rax
    mov $-100, %rdi
    lea g2_block_path(%rip), %rsi
    mov $1, %rdx
    xor %r10, %r10
    syscall
    test %rax, %rax
    js g2_block_child_exit
    mov %rax, %r13
    mov $1, %rax
    mov %r13, %rdi
    lea g2_byte(%rip), %rsi
    mov $1, %rdx
    syscall
    mov $3, %rax
    mov %r13, %rdi
    syscall
g2_block_child_exit:
    mov $60, %rax
    xor %rdi, %rdi
    syscall
    jmp g2_block_child_exit

/* Returns 1 when a packed inotify record contains `mask`.
 * in: rdi=buffer, rsi=bytes, edx=bit; out: rax. */
g2_scan_mask:
    xor %eax, %eax
    mov %rdi, %r8
    mov %rsi, %r9
g2_scan_loop:
    cmp $16, %r9
    jb g2_scan_none
    mov 4(%r8), %r10d
    test %edx, %r10d
    jnz g2_scan_found
    mov 12(%r8), %ecx
    add $19, %rcx
    and $-4, %rcx
    add %rcx, %r8
    sub %rcx, %r9
    jmp g2_scan_loop
g2_scan_found:
    mov $1, %eax
    ret
g2_scan_none:
    xor %eax, %eax
    ret

g2_fail_inotify:
    lea g2_inotify_failure(%rip), %rsi
    mov $g2_inotify_failure_len, %rdx
    jmp report_failure
g2_fail_inotify_block:
    lea g2_inotify_block_failure(%rip), %rsi
    mov $g2_inotify_block_failure_len, %rdx
    jmp report_failure
g2_fail_proc:
    lea g2_proc_failure(%rip), %rsi
    mov $g2_proc_failure_len, %rdx
    jmp report_failure
g2_fail_dev:
    lea g2_dev_failure(%rip), %rsi
    mov $g2_dev_failure_len, %rdx
    jmp report_failure
g2_fail_policy:
    lea g2_policy_failure(%rip), %rsi
    mov $g2_policy_failure_len, %rdx
    jmp report_failure
g2_fail_denial:
    lea g2_denial_failure(%rip), %rsi
    mov $g2_denial_failure_len, %rdx
    jmp report_failure
g2_fail_quota:
    lea g2_quota_failure(%rip), %rsi
    mov $g2_quota_failure_len, %rdx
    jmp report_failure
g2_extras_done:

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

g2_tmp_path:
    .asciz "/tmp"
g2_etc_path:
    .asciz "/etc"
g2_q1_path:
    .asciz "/tmp/q1"
g2_q2_path:
    .asciz "/tmp/q2"
g2_block_path:
    .asciz "/tmp/block"
g2_motd_path:
    .asciz "/etc/motd"
g2_proc_exe:
    .asciz "/proc/self/exe"
g2_proc_maps:
    .asciz "/proc/self/maps"
g2_proc_status:
    .asciz "/proc/self/status"
g2_proc_fd:
    .asciz "/proc/self/fd"
g2_dev_null:
    .asciz "/dev/null"
g2_dev_zero:
    .asciz "/dev/zero"
g2_dev_random:
    .asciz "/dev/random"
g2_dev_urandom:
    .asciz "/dev/urandom"
g2_dev_dri:
    .asciz "/dev/dri/card0"
g2_path_audit:
    .asciz "/proc/aether/syscalls"
g2_path_escape:
    .asciz "/etc/../etc/motd"
g2_byte:
    .ascii "x"
g2_inotify_block_message:
    .ascii "AETHER_LINUX_INOTIFY_BLOCK_OK blocking=ok dirs=ok events=ok overflow=ok\n"
    .set g2_inotify_block_message_len, . - g2_inotify_block_message
g2_proc_message:
    .ascii "AETHER_LINUX_PROC_OK exe=ok maps=ok status=ok fd=ok\n"
    .set g2_proc_message_len, . - g2_proc_message
g2_dev_message:
    .ascii "AETHER_LINUX_DEV_OK null=ok zero=ok random=ok\n"
    .set g2_dev_message_len, . - g2_dev_message
g2_policy_message:
    .ascii "AETHER_LINUX_SYSCALL_POLICY_OK unknown=enosys network=denied device=denied audit=ok\n"
    .set g2_policy_message_len, . - g2_policy_message
g2_denial_message:
    .ascii "AETHER_LINUX_DENIAL_OK path=ok rofs=ok device=denied\n"
    .set g2_denial_message_len, . - g2_denial_message
g2_quota_message:
    .ascii "AETHER_LINUX_QUOTA_OK overlay=enospc fd=emfile vma=enomem\n"
    .set g2_quota_message_len, . - g2_quota_message
g2_inotify_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=advanced\n"
    .set g2_inotify_failure_len, . - g2_inotify_failure
g2_inotify_block_failure:
    .ascii "AETHER_LINUX_INOTIFY_FAIL stage=blocking\n"
    .set g2_inotify_block_failure_len, . - g2_inotify_block_failure
g2_proc_failure:
    .ascii "AETHER_LINUX_PROC_FAIL\n"
    .set g2_proc_failure_len, . - g2_proc_failure
g2_dev_failure:
    .ascii "AETHER_LINUX_DEV_FAIL\n"
    .set g2_dev_failure_len, . - g2_dev_failure
g2_policy_failure:
    .ascii "AETHER_LINUX_SYSCALL_POLICY_FAIL\n"
    .set g2_policy_failure_len, . - g2_policy_failure
g2_denial_failure:
    .ascii "AETHER_LINUX_DENIAL_FAIL\n"
    .set g2_denial_failure_len, . - g2_denial_failure
g2_quota_failure:
    .ascii "AETHER_LINUX_QUOTA_FAIL\n"
    .set g2_quota_failure_len, . - g2_quota_failure
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

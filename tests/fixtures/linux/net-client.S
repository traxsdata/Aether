/* SPDX-License-Identifier: Apache-2.0 */
/* Static Linux fixture for the network client path (make net-test).
 *
 * The AppImage's signed record declares network=client, and the kernel is
 * built with net-consent-test, which answers the first-use consent request
 * with "allow" after a delay so the blocking connect really waits for it.
 *
 * It proves, in order:
 *   - bind to a real port and listen are refused (EACCES): clients never
 *     listen;
 *   - connect to loopback is refused (ENETUNREACH);
 *   - a blocking TCP connect to the QEMU host alias reaches the page
 *     tools/test-net.py serves, and the response carries its payload;
 *   - /etc/resolv.conf names the DHCP name server;
 *   - a UDP datagram to the host echo port comes back through the inbound
 *     filter, which only lets in replies from destinations the socket used.
 *
 * Assemble with --defsym AETHER_NET_PORT=<port> (the host test port).
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    sub $8192, %rsp

    /* --- bind(any:80) and listen are refused. --- */
    mov $41, %rax               /* socket(AF_INET, SOCK_STREAM, 0) */
    mov $2, %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $49, %rax               /* bind(fd, any:80, 16) */
    mov %r12, %rdi
    lea bind_address(%rip), %rsi
    mov $16, %rdx
    syscall
    cmp $-13, %rax
    jne fail

    mov $50, %rax               /* listen(fd, 1) */
    mov %r12, %rdi
    mov $1, %rsi
    syscall
    cmp $-13, %rax
    jne fail

    /* --- connect to 127.0.0.1 is unreachable by policy. --- */
    mov $42, %rax
    mov %r12, %rdi
    lea loopback_address(%rip), %rsi
    mov $16, %rdx
    syscall
    cmp $-101, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* --- TCP fetch from the host. Retry while DHCP has no lease yet. --- */
    mov $300, %r14
.Lconnect:
    mov $41, %rax
    mov $2, %rdi
    mov $1, %rsi
    xor %rdx, %rdx
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12

    mov $42, %rax               /* connect(fd, host, 16): blocks for consent */
    mov %r12, %rdi
    lea host_address(%rip), %rsi
    mov $16, %rdx
    syscall
    test %rax, %rax
    jz .Lconnected
    cmp $-101, %rax
    jne fail
    mov $3, %rax
    mov %r12, %rdi
    syscall
    dec %r14
    jz fail
    mov $35, %rax               /* nanosleep(100 ms) */
    lea retry_delay(%rip), %rdi
    xor %rsi, %rsi
    syscall
    jmp .Lconnect

.Lconnected:
    mov $1, %rax                /* write(fd, request, len) */
    mov %r12, %rdi
    lea request(%rip), %rsi
    mov $request_len, %rdx
    syscall
    cmp $request_len, %rax
    jne fail

    xor %r13, %r13              /* bytes received */
.Lread:
    mov $0, %rax
    mov %r12, %rdi
    lea (%rsp,%r13), %rsi
    mov $4096, %rdx
    sub %r13, %rdx
    jz .Lread_done
    syscall
    test %rax, %rax
    js fail
    jz .Lread_done
    add %rax, %r13
    jmp .Lread
.Lread_done:
    mov $3, %rax
    mov %r12, %rdi
    syscall

    mov %rsp, %rdi
    mov %r13, %rsi
    lea payload(%rip), %rdx
    mov $payload_len, %rcx
    call contains
    test %rax, %rax
    jz fail

    /* --- /etc/resolv.conf names the DHCP server. --- */
    mov $257, %rax              /* openat(AT_FDCWD, path, O_RDONLY) */
    mov $-100, %rdi
    lea resolv_path(%rip), %rsi
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
    test %rax, %rax
    jle fail
    mov %rax, %r13
    mov $3, %rax
    mov %r12, %rdi
    syscall
    mov %rsp, %rdi
    mov %r13, %rsi
    lea nameserver(%rip), %rdx
    mov $nameserver_len, %rcx
    call contains
    test %rax, %rax
    jz fail

    /* --- UDP round trip through the host echo server. --- */
    mov $41, %rax               /* socket(AF_INET, SOCK_DGRAM, 0) */
    mov $2, %rdi
    mov $2, %rsi
    xor %rdx, %rdx
    syscall
    test %rax, %rax
    js fail
    mov %rax, %r12
    mov $42, %rax               /* connect(fd, host, 16) */
    mov %r12, %rdi
    lea host_address(%rip), %rsi
    mov $16, %rdx
    syscall
    test %rax, %rax
    jnz fail
    mov $1, %rax
    mov %r12, %rdi
    lea udp_ping(%rip), %rsi
    mov $udp_ping_len, %rdx
    syscall
    cmp $udp_ping_len, %rax
    jne fail
    mov $0, %rax                /* read blocks until the echo arrives */
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $64, %rdx
    syscall
    cmp $udp_ping_len, %rax
    jne fail
    mov %rsp, %rdi
    mov %rax, %rsi
    lea udp_ping(%rip), %rdx
    mov $udp_ping_len, %rcx
    call contains
    test %rax, %rax
    jz fail
    mov $3, %rax
    mov %r12, %rdi
    syscall

    mov $1, %rax
    mov $1, %rdi
    lea success(%rip), %rsi
    mov $success_len, %rdx
    syscall
    mov $231, %rax
    xor %rdi, %rdi
    syscall
1:
    jmp 1b

fail:
    mov $1, %rax
    mov $1, %rdi
    lea failure(%rip), %rsi
    mov $failure_len, %rdx
    syscall
    mov $231, %rax
    mov $1, %rdi
    syscall
2:
    jmp 2b

/* contains(haystack=%rdi, length=%rsi, needle=%rdx, needle_length=%rcx)
 * returns 1 in %rax when the needle occurs in the haystack. */
contains:
    xor %rax, %rax
    cmp %rcx, %rsi
    jb .Lcontains_done
    sub %rcx, %rsi
    xor %r8, %r8                /* candidate start */
.Lcontains_start:
    cmp %rsi, %r8
    ja .Lcontains_done
    xor %r9, %r9
.Lcontains_byte:
    cmp %rcx, %r9
    je .Lcontains_found
    lea (%r8,%r9), %r10
    movzbl (%rdi,%r10), %r11d
    cmpb (%rdx,%r9), %r11b
    jne .Lcontains_next
    inc %r9
    jmp .Lcontains_byte
.Lcontains_next:
    inc %r8
    jmp .Lcontains_start
.Lcontains_found:
    mov $1, %rax
.Lcontains_done:
    ret

    .section .rodata
    .balign 8
retry_delay:
    .quad 0, 100000000
bind_address:
    .short 2
    .byte 0, 80
    .byte 0, 0, 0, 0
    .quad 0
loopback_address:
    .short 2
    .byte AETHER_NET_PORT >> 8, AETHER_NET_PORT & 0xff
    .byte 127, 0, 0, 1
    .quad 0
host_address:
    .short 2
    .byte AETHER_NET_PORT >> 8, AETHER_NET_PORT & 0xff
    .byte 10, 0, 2, 2
    .quad 0
request:
    .ascii "GET /aether-net-test HTTP/1.0\r\nHost: 10.0.2.2\r\n\r\n"
    .set request_len, . - request
payload:
    .ascii "AETHER_NET_PAYLOAD"
    .set payload_len, . - payload
resolv_path:
    .asciz "/etc/resolv.conf"
nameserver:
    .ascii "nameserver 10.0.2.3\n"
    .set nameserver_len, . - nameserver
udp_ping:
    .ascii "AETHER_UDP_PING"
    .set udp_ping_len, . - udp_ping
success:
    .ascii "AETHER_LINUX_NET_OK tcp=ok udp=ok resolv=ok listen=denied loopback=denied\n"
    .set success_len, . - success
failure:
    .ascii "AETHER_LINUX_NET_FAIL\n"
    .set failure_len, . - failure

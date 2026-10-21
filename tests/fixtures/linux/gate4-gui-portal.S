/* SPDX-License-Identifier: Apache-2.0 */
/* Static stand-in for the windowed GridCraft portal path.
 *
 * It reads the real XLSX fixture through the GUI portal grant, stages the same
 * bytes through `<file>.sctmp`, commits them with rename, and exits. The
 * kernel emits the immediate save marker during the commit and performs the
 * post-exit revocation and fresh read/write/rename denial checks itself.
 */
    .section .text
    .global _start
    .type _start, @function
_start:
    sub $8192, %rsp

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

    /* read(fd, stack_buffer, 8192) */
    mov $0, %rax
    mov %r12, %rdi
    mov %rsp, %rsi
    mov $8192, %rdx
    syscall
    cmp $1024, %rax
    jle fail
    mov %rax, %r13

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* Stage the exact workbook bytes. */
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
    mov %rsp, %rsi
    mov %r13, %rdx
    syscall
    cmp %r13, %rax
    jne fail

    mov $3, %rax
    mov %r12, %rdi
    syscall

    /* rename(temp_path, portal_path) */
    mov $82, %rax
    lea temp_path(%rip), %rdi
    lea portal_path(%rip), %rsi
    syscall
    test %rax, %rax
    js fail

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
    .asciz "/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx"
temp_path:
    .asciz "/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx.sctmp"

    .section .note.GNU-stack,"",@progbits

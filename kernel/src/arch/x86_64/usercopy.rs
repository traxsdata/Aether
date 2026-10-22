// SPDX-License-Identifier: Apache-2.0
//! User-memory copy routine with an exception-table fixup.
//!
//! [`copy_raw`] is the one kernel routine allowed to dereference a user
//! virtual address. Its single `rep movsb` is listed in the `.ex_table`
//! section with a recovery address. When that instruction takes a page fault
//! on a user-window address, the exception handler resumes at the recovery
//! address instead of halting, and the copy reports the bytes it did not
//! copy. Every other ring-0 fault still halts the machine.
//!
//! The fixup is a safety net, not the validation. Callers must still check
//! that the user range lies inside `USER_BASE..USER_LIMIT` before copying:
//! nothing here stops a caller from being handed a kernel address.

use aether_abi::user::{USER_BASE, USER_LIMIT};

core::arch::global_asm!(
    r#"
    .section .text
    .p2align 4
    .global aether_user_copy
    .type aether_user_copy, @function
    /* rdi = destination, rsi = source, rdx = length; returns the number of
       bytes not copied in rax. DF is clear on every kernel entry path. */
aether_user_copy:
    mov rcx, rdx
aether_user_copy_insn:
    rep movsb
    xor eax, eax
    ret
aether_user_copy_fixup:
    /* `rep movsb` faults precisely, so rcx holds the bytes left. */
    mov rax, rcx
    ret
    .size aether_user_copy, . - aether_user_copy

    .pushsection .ex_table, "a"
    .balign 8
    .quad aether_user_copy_insn, aether_user_copy_fixup
    .popsection
"#
);

unsafe extern "C" {
    fn aether_user_copy(destination: *mut u8, source: *const u8, length: usize) -> usize;
    static __ex_table_start: ExTableEntry;
    static __ex_table_end: ExTableEntry;
}

/// One exception-table entry: the address of an instruction that may fault on
/// user memory, and where to resume when it does.
#[repr(C)]
#[derive(Clone, Copy)]
struct ExTableEntry {
    fault_rip: u64,
    fixup_rip: u64,
}

/// Copies `length` bytes and returns how many were not copied.
///
/// # Safety
///
/// Each of `destination` and `source` is either a kernel buffer of `length`
/// valid bytes or a user range already checked to lie in the user window of
/// the active address space. A fault is recovered only on the user side.
#[must_use]
pub unsafe fn copy_raw(destination: *mut u8, source: *const u8, length: usize) -> usize {
    // SAFETY: The caller provides the range guarantees above. A user-side
    // page fault resumes at the fixup label, which returns the remainder.
    unsafe { aether_user_copy(destination, source, length) }
}

/// Returns the recovery address for a ring-0 page fault, or `None` if the
/// fault must halt.
///
/// Only a page fault at a listed instruction whose faulting address (CR2) is
/// in the user window is recoverable. A fault on the kernel side of a copy is
/// a kernel bug and is not hidden.
#[must_use]
pub fn fixup_for(vector: u64, rip: u64, cr2: u64) -> Option<u64> {
    if vector != 14 || !(USER_BASE..USER_LIMIT).contains(&cr2) {
        return None;
    }
    lookup(rip)
}

fn lookup(rip: u64) -> Option<u64> {
    // SAFETY: The linker places every `.ex_table` entry between these two
    // symbols, in read-only data, as an array of `ExTableEntry`.
    let entries = unsafe {
        let start = core::ptr::addr_of!(__ex_table_start);
        let end = core::ptr::addr_of!(__ex_table_end);
        let count = (end as usize - start as usize) / core::mem::size_of::<ExTableEntry>();
        core::slice::from_raw_parts(start, count)
    };
    entries
        .iter()
        .find(|entry| entry.fault_rip == rip)
        .map(|entry| entry.fixup_rip)
}

// SPDX-License-Identifier: Apache-2.0
//! Ring-3 system call dispatcher.
//!
//! Every pointer and handle is validated here before use. The current task's
//! PML4 is authoritative: a user address is accepted only when it lies in the
//! user window and every page in the range is present with the user bit (and
//! writable for output). The kernel never trusts a sender or process identity
//! supplied by the caller.

use crate::arch::x86_64::idt::InterruptFrame;
use crate::arch::x86_64::serial;
use crate::memory::paging;
use crate::scheduler;
use crate::serial_line;
use aether_abi::user::{MAX_SYSCALL_IO, Syscall, USER_LIMIT, errno};
use aether_memory::frame::PhysFrame;

/// Dispatches one system call and returns the frame to resume.
///
/// Returning a different frame lets `exit` schedule the next task without
/// returning to a dead user context.
pub fn dispatch(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    // Read fields individually. The ring-3 syscall gate supplies RSP/SS on the
    // stack, but a ring-0 `int 0x80` would not; copying the whole frame would
    // then read past the initialized part of the stack.
    let cs = unsafe { (*frame).cs };
    if cs & 3 != 3 {
        serial_line!("AETHER_BOOT_FAILED reason=syscall from ring 0");
        crate::arch::x86_64::halt()
    }

    let number = unsafe { (*frame).rax };
    let rdi = unsafe { (*frame).rdi };
    let rsi = unsafe { (*frame).rsi };
    let rdx = unsafe { (*frame).rdx };
    let r10 = unsafe { (*frame).r10 };
    let Some(call) = Syscall::from_raw(number) else {
        // SAFETY: The assembly stub passes a live frame on the current kernel
        // stack and does not move it for the duration of the call.
        unsafe { (*frame).rax = errno::ENOSYS as u64 };
        return frame;
    };

    let Some(pml4) = scheduler::current_pml4() else {
        serial_line!("AETHER_BOOT_FAILED reason=syscall without pml4");
        crate::arch::x86_64::halt()
    };

    // `exit` does not return to the caller. Handle it before the generic
    // result write so the retired frame is never reused.
    if call == Syscall::Exit {
        return scheduler::user_exit(unsafe { (*frame).rdi });
    }

    // Yield and sleep must actually give another ready task a chance to run.
    // The frame is saved by the scheduler and resumed after the next tick (or
    // immediately if another task is ready), with RAX set to zero.
    if matches!(call, Syscall::Yield | Syscall::Sleep) {
        // SAFETY: Same frame invariant as above.
        unsafe { (*frame).rax = 0 };
        return scheduler::yield_current(frame);
    }

    let result = match call {
        Syscall::Write => write_console(pml4, rdi, rsi, rdx),
        Syscall::GetPid => scheduler::current_task() as i64,
        Syscall::Uptime => scheduler::ticks() as i64,
        Syscall::TaskCount => scheduler::task_count() as i64,
        Syscall::Open => crate::fs::sys_open(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::PortalOpen => {
            crate::fs::sys_portal_open(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL)
        }
        Syscall::Read => crate::fs::sys_read(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::WriteFile => crate::fs::sys_write(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::Close => crate::fs::sys_close(rdi).unwrap_or(errno::EBADF),
        Syscall::List => crate::fs::sys_list(pml4, rdi, rsi, rdx, r10).unwrap_or(errno::EINVAL),
        Syscall::Stat => crate::fs::sys_stat(pml4, rdi, rsi).unwrap_or(errno::EINVAL),
        _ => errno::ENOSYS,
    };

    // SAFETY: Same frame invariant as above.
    unsafe { (*frame).rax = result as u64 };
    frame
}

fn write_console(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64) -> i64 {
    if descriptor != 1 {
        return errno::EBADF;
    }
    if length as usize > MAX_SYSCALL_IO {
        return errno::EINVAL;
    }
    let Some(bytes) = user_bytes(pml4, pointer, length, false) else {
        return errno::EINVAL;
    };
    serial::write_bytes(bytes);
    length as i64
}

/// Returns a validated read-only user slice.
///
/// # Safety
///
/// The returned slice aliases user memory. The caller must not hold it across a
/// context switch and must not use it after the task's address space changes.
pub fn user_bytes<'a>(pml4: PhysFrame, pointer: u64, length: u64, write: bool) -> Option<&'a [u8]> {
    if length as usize > MAX_SYSCALL_IO {
        return None;
    }
    if pointer >= USER_LIMIT || !paging::user_range_valid(pml4, pointer, length, write) {
        return None;
    }
    if length == 0 {
        return Some(&[]);
    }
    // SAFETY: The range lies inside the caller's user window and every page
    // was validated as present (and writable when required). The kernel runs
    // with interrupts masked by the syscall gate, so the mapping cannot change
    // underneath the copy.
    Some(unsafe { core::slice::from_raw_parts(pointer as *const u8, length as usize) })
}

/// Returns a validated mutable user slice with the same lifetime caveats as
/// [`user_bytes`].
///
/// # Safety
///
/// The caller must ensure the task does not concurrently access the range.
pub unsafe fn user_bytes_mut<'a>(
    pml4: PhysFrame,
    pointer: u64,
    length: u64,
) -> Option<&'a mut [u8]> {
    if length as usize > MAX_SYSCALL_IO {
        return None;
    }
    if pointer >= USER_LIMIT || !paging::user_range_valid(pml4, pointer, length, true) {
        return None;
    }
    if length == 0 {
        return Some(&mut []);
    }
    // SAFETY: The range is inside the user window and every page is present
    // and writable. The caller supplies the aliasing guarantee.
    Some(unsafe { core::slice::from_raw_parts_mut(pointer as *mut u8, length as usize) })
}

/// Copies a bounded user string after validating its range.
pub fn user_str<'a>(pml4: PhysFrame, pointer: u64, length: u64) -> Option<&'a str> {
    const MAX_PATH: u64 = 256;
    if length == 0 || length > MAX_PATH {
        return None;
    }
    let bytes = user_bytes(pml4, pointer, length, false)?;
    core::str::from_utf8(bytes).ok()
}

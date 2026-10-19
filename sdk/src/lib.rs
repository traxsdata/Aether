// SPDX-License-Identifier: Apache-2.0
//! Initial Aether native application SDK.
//!
//! The SDK wraps the raw `int 0x80` system call ABI in ordinary Rust
//! functions. It performs no pointer or capability checks: those belong to the
//! kernel, which validates every handle and user range before acting. A
//! process can therefore be sandboxed even though it links this crate.

#![no_std]

use aether_abi::user::{FileRights, Syscall, errno};

/// Raw syscall with up to four arguments.
///
/// # Safety
///
/// The caller is responsible for passing a valid system call number and
/// arguments. The kernel validates user pointers and handles, but an incorrect
/// number returns `-ENOSYS`.
#[inline]
pub unsafe fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> i64 {
    let result: i64;
    // SAFETY: The ABI fixes the register assignment. `int 0x80` is the only
    // supported entry and does not require a stack switch in user code.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as i64 => result,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            options(nostack)
        );
    }
    result
}

/// Yields the CPU.
pub fn yield_now() -> i64 {
    // SAFETY: No pointers or handles are involved.
    unsafe { syscall4(Syscall::Yield as u64, 0, 0, 0, 0) }
}

/// Sleeps until the next timer tick.
pub fn sleep() -> i64 {
    // SAFETY: No pointers or handles are involved.
    unsafe { syscall4(Syscall::Sleep as u64, 0, 0, 0, 0) }
}

/// Returns the current process identifier.
#[must_use]
pub fn pid() -> u64 {
    // SAFETY: No pointers or handles are involved.
    let value = unsafe { syscall4(Syscall::GetPid as u64, 0, 0, 0, 0) };
    value as u64
}

/// Returns the system uptime in timer ticks.
#[must_use]
pub fn uptime() -> u64 {
    // SAFETY: No pointers or handles are involved.
    let value = unsafe { syscall4(Syscall::Uptime as u64, 0, 0, 0, 0) };
    value as u64
}

/// Terminates the current process.
pub fn exit(status: u64) -> ! {
    // SAFETY: The kernel terminates the caller; the loop only runs if a
    // broken kernel returns to a terminated process.
    let _ = unsafe { syscall4(Syscall::Exit as u64, status, 0, 0, 0) };
    loop {
        // SAFETY: Idling the CPU is harmless.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}

/// Writes bytes to standard output.
pub fn print(bytes: &[u8]) -> i64 {
    // SAFETY: The slice is readable in the caller's address space and the
    // kernel validates the range before copying.
    unsafe {
        syscall4(
            Syscall::Write as u64,
            1,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            0,
        )
    }
}

/// Writes a string to standard output.
pub fn print_str(text: &str) -> i64 {
    print(text.as_bytes())
}

/// Opens a path relative to the process root.
pub fn open(path: &str, rights: u64) -> Result<u64, i64> {
    // SAFETY: The path slice is readable and the kernel validates it.
    let result = unsafe {
        syscall4(
            Syscall::Open as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            rights,
            0,
        )
    };
    if result < 0 { Err(result) } else { Ok(result as u64) }
}

/// Opens a path through the file portal.
pub fn portal_open(path: &str, rights: u64) -> Result<u64, i64> {
    // SAFETY: The path slice is readable and the kernel validates it.
    let result = unsafe {
        syscall4(
            Syscall::PortalOpen as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            rights,
            0,
        )
    };
    if result < 0 { Err(result) } else { Ok(result as u64) }
}

/// Reads from a file handle.
pub fn read(handle: u64, buffer: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: The buffer is writable and the kernel validates the range.
    let result = unsafe {
        syscall4(
            Syscall::Read as u64,
            handle,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Writes to a file handle.
pub fn write_file(handle: u64, bytes: &[u8]) -> Result<usize, i64> {
    // SAFETY: The slice is readable and the kernel validates the range.
    let result = unsafe {
        syscall4(
            Syscall::WriteFile as u64,
            handle,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            0,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Closes a file handle.
pub fn close(handle: u64) -> Result<(), i64> {
    // SAFETY: No pointers are involved.
    let result = unsafe { syscall4(Syscall::Close as u64, handle, 0, 0, 0) };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Lists a directory into `buffer`.
pub fn list(path: &str, buffer: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: Both slices are readable/writable and validated by the kernel.
    let result = unsafe {
        syscall4(
            Syscall::List as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Returns the size of a file.
pub fn file_size(path: &str) -> Result<usize, i64> {
    // SAFETY: The path slice is readable and the kernel validates it.
    let result = unsafe {
        syscall4(
            Syscall::Stat as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            0,
            0,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
}

/// Convenience constants for callers.
pub const READ: u64 = FileRights::READ;
/// Write access.
pub const WRITE: u64 = FileRights::WRITE;
/// Create on open.
pub const CREATE: u64 = FileRights::CREATE;

/// Error names for diagnostics.
#[must_use]
pub fn error_name(error: i64) -> &'static str {
    match error {
        errno::ENOSYS => "ENOSYS",
        errno::EINVAL => "EINVAL",
        errno::EACCES => "EACCES",
        errno::ENOENT => "ENOENT",
        errno::ENOSPC => "ENOSPC",
        errno::EPERM => "EPERM",
        errno::EBADF => "EBADF",
        errno::ENOGRANT => "ENOGRANT",
        _ => "unknown",
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Initial Aether native application SDK.
//!
//! The SDK wraps the raw `int 0x80` system call ABI in ordinary Rust
//! functions. It performs no pointer or capability checks: those belong to the
//! kernel, which validates every handle and user range before acting. A
//! process can therefore be sandboxed even though it links this crate.

#![no_std]

use aether_abi::user::{
    FileRights, PROCESS_RECORD_BYTES, ProcessRecord, SYSTEM_INFO_BYTES, Syscall, SystemInfo, errno,
};

/// Maximum bytes an IPC message payload may carry; enforced by the kernel.
pub const MAX_MESSAGE_BYTES: usize = 64;

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
        );
    }
    result
}

/// Raw syscall with five arguments (the fifth arrives in R8).
///
/// # Safety
///
/// Same contract as [`syscall4`].
#[inline]
pub unsafe fn syscall5(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> i64 {
    let result: i64;
    // SAFETY: The ABI fixes the register assignment; the kernel validates
    // every pointer and handle.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as i64 => result,
            in("rdi") arg0,
            in("rsi") arg1,
            in("rdx") arg2,
            in("r10") arg3,
            in("r8") arg4,
        );
    }
    result
}

/// Yields the CPU.
pub fn yield_now() -> i64 {
    // SAFETY: No pointers or handles are involved.
    unsafe { syscall4(Syscall::Yield as u64, 0, 0, 0, 0) }
}

/// Sleeps for `ticks` timer ticks, parking the caller out of the ready queue.
///
/// `ticks` is measured in scheduler ticks (100 Hz on the supported platform).
/// The caller is blocked until its wake deadline elapses, so an idle program
/// neither burns CPU nor steals time slices while it waits. `0` still yields
/// to the next tick. Returns zero.
pub fn sleep(ticks: u32) -> i64 {
    // SAFETY: `ticks` is a scalar duration; no pointers or handles involved.
    unsafe { syscall4(Syscall::Sleep as u64, u64::from(ticks), 0, 0, 0) }
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
    if result < 0 {
        Err(result)
    } else {
        Ok(result as u64)
    }
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
    if result < 0 {
        Err(result)
    } else {
        Ok(result as u64)
    }
}

/// Asks the desktop for an exact-file portal grant.
///
/// The request only appears in the consent UI; it grants nothing. A request
/// index is returned so a caller can display or retry, and the path only
/// becomes usable after the user approves it and `portal_open` succeeds.
pub fn portal_request(path: &str, rights: u64) -> Result<usize, i64> {
    // SAFETY: The path slice is readable and the kernel validates it.
    let result = unsafe {
        syscall4(
            Syscall::PortalRequest as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            rights,
            0,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as usize)
    }
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

/// Creates a bounded IPC endpoint and returns its capability handle.
///
/// The kernel endpoint pool is fixed (eight endpoints), so creation past
/// that bound fails with `ENOSPC` instead of growing kernel memory. The returned
/// capability carries `SEND`, `RECEIVE`, and `TRANSFER` rights.
pub fn endpoint_create() -> Result<u64, i64> {
    // SAFETY: No pointers or handles are involved.
    let result = unsafe { syscall4(Syscall::EndpointCreate as u64, 0, 0, 0, 0) };
    if result < 0 {
        Err(result)
    } else {
        Ok(result as u64)
    }
}

/// Sends `payload` through an IPC endpoint capability.
///
/// Returns `EAGAIN` when the endpoint queue is full; the caller may retry on
/// a later iteration.
pub fn endpoint_send(handle: u64, payload: &[u8]) -> Result<(), i64> {
    endpoint_send_with_transfer(handle, payload, 0)
}

/// Sends `payload` and optionally transfers another capability the caller
/// owns. The transferred capability must carry the `TRANSFER` right; the
/// receiver's copy does not.
pub fn endpoint_send_with_transfer(handle: u64, payload: &[u8], transfer: u64) -> Result<(), i64> {
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(errno::EINVAL);
    }
    // SAFETY: The payload slice is readable in the caller's address space and
    // the kernel validates the range and both handles before it acts.
    let result = unsafe {
        syscall4(
            Syscall::EndpointSend as u64,
            handle,
            payload.as_ptr() as u64,
            payload.len() as u64,
            transfer,
        )
    };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Closes an endpoint capability and revokes every alias of it in the caller's
/// table. If the caller owns the endpoint, the object is destroyed, so aliases
/// held by other tasks become stale and report `EBADF` on use.
pub fn endpoint_close(handle: u64) -> Result<(), i64> {
    // SAFETY: No pointers are involved; the kernel validates the handle.
    let result = unsafe { syscall4(Syscall::EndpointClose as u64, handle, 0, 0, 0) };
    if result < 0 { Err(result) } else { Ok(()) }
}

/// Receives one message from an endpoint capability.
///
/// The buffer must be able to hold a maximum-size message so a receive is
/// never partial. Returns the payload length and the handle of a transferred
/// capability (`0` when none arrived). Returns `EAGAIN` while the queue is
/// empty.
pub fn endpoint_receive(handle: u64, buffer: &mut [u8]) -> Result<(usize, u64), i64> {
    if buffer.len() < MAX_MESSAGE_BYTES {
        return Err(errno::EINVAL);
    }
    let mut transferred = 0u32;
    // SAFETY: `transferred` is a local 4-byte field and the buffer is
    // writable in the caller's own address space; the kernel validates both
    // ranges and both handles before it acts.
    let result = unsafe {
        syscall4(
            Syscall::EndpointReceive as u64,
            handle,
            buffer.as_mut_ptr() as u64,
            MAX_MESSAGE_BYTES as u64,
            core::ptr::addr_of_mut!(transferred) as u64,
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok((result.max(0) as usize, u64::from(transferred)))
    }
}

/// Receives one message, yielding the CPU until the queue has a message or
/// `timeout_ticks` timer ticks have elapsed.
///
/// `timeout_ticks` is measured in scheduler ticks (100 Hz on the supported
/// platform); zero waits without a deadline. Returns `ETIMEDOUT` when the
/// deadline passes, and `EAGAIN` only for a non-blocking receive.
pub fn endpoint_receive_timeout(
    handle: u64,
    buffer: &mut [u8],
    timeout_ticks: u32,
) -> Result<(usize, u64), i64> {
    if buffer.len() < MAX_MESSAGE_BYTES {
        return Err(errno::EINVAL);
    }
    let mut transferred = 0u32;
    // SAFETY: Same validated ranges as `endpoint_receive`; the fifth argument
    // is a scalar timeout, not a pointer.
    let result = unsafe {
        syscall5(
            Syscall::EndpointReceiveTimeout as u64,
            handle,
            buffer.as_mut_ptr() as u64,
            MAX_MESSAGE_BYTES as u64,
            core::ptr::addr_of_mut!(transferred) as u64,
            u64::from(timeout_ticks),
        )
    };
    if result < 0 {
        Err(result)
    } else {
        Ok((result.max(0) as usize, u64::from(transferred)))
    }
}

fn result(value: i64) -> Result<u64, i64> {
    if value < 0 {
        Err(value)
    } else {
        Ok(value as u64)
    }
}

/// Starts a terminal program. `args` is the argument block: the working
/// directory, the program name, then its arguments, NUL separated. `stdin`
/// and `stdout` are 0 for the terminal or a pipe id the caller holds.
pub fn spawn(args: &[u8], stdin: u64, stdout: u64) -> Result<u64, i64> {
    // SAFETY: The slice is readable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::Spawn as u64,
            args.as_ptr() as u64,
            args.len() as u64,
            stdin,
            stdout,
        )
    })
}

/// Collects a child's exit status. Returns `Err(EAGAIN)` while it runs.
pub fn wait(pid: u64) -> Result<i64, i64> {
    // SAFETY: No pointers are involved.
    let value = unsafe { syscall4(Syscall::Wait as u64, pid, 0, 0, 0) };
    if value < 0 { Err(value) } else { Ok(value) }
}

/// Terminates a program in the caller's terminal session.
pub fn kill(pid: u64) -> Result<(), i64> {
    // SAFETY: No pointers are involved.
    result(unsafe { syscall4(Syscall::Kill as u64, pid, 0, 0, 0) }).map(|_| ())
}

/// Fills `records` with the process table and returns how many were written.
pub fn process_list(records: &mut [ProcessRecord]) -> Result<usize, i64> {
    let mut buffer = [0u8; 4096];
    let room = (records.len() * PROCESS_RECORD_BYTES).min(buffer.len());
    // SAFETY: The buffer is writable; the kernel validates the range.
    let count = result(unsafe {
        syscall4(
            Syscall::ProcessList as u64,
            buffer.as_mut_ptr() as u64,
            room as u64,
            0,
            0,
        )
    })? as usize;
    for (index, record) in records.iter_mut().take(count).enumerate() {
        let mut raw = [0u8; PROCESS_RECORD_BYTES];
        raw.copy_from_slice(
            &buffer[index * PROCESS_RECORD_BYTES..(index + 1) * PROCESS_RECORD_BYTES],
        );
        *record = ProcessRecord::decode(&raw);
    }
    Ok(count)
}

/// Returns the system summary.
pub fn system_info() -> Result<SystemInfo, i64> {
    let mut buffer = [0u8; SYSTEM_INFO_BYTES];
    // SAFETY: The buffer is writable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::SystemInfo as u64,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
        )
    })?;
    Ok(SystemInfo::decode(&buffer))
}

/// Reads standard input. `Ok(0)` is end of input; `Err(EAGAIN)` means none
/// is queued yet.
pub fn read_input(buffer: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: The buffer is writable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::ReadInput as u64,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
        )
    })
    .map(|count| count as usize)
}

/// Returns the terminal size as `(columns, rows)`.
pub fn terminal_size() -> Result<(u16, u16), i64> {
    // SAFETY: No pointers are involved.
    let value = result(unsafe { syscall4(Syscall::TerminalSize as u64, 0, 0, 0, 0) })?;
    Ok(((value >> 16) as u16, value as u16))
}

/// Creates a pipe; the caller holds both ends.
pub fn pipe_create() -> Result<u64, i64> {
    // SAFETY: No pointers are involved.
    result(unsafe { syscall4(Syscall::PipeCreate as u64, 0, 0, 0, 0) })
}

/// Reads from a pipe. `Ok(0)` is end of input; `Err(EAGAIN)` means empty.
pub fn pipe_read(pipe: u64, buffer: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: The buffer is writable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::PipeRead as u64,
            pipe,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
        )
    })
    .map(|count| count as usize)
}

/// Writes to a pipe; may accept fewer bytes than offered.
pub fn pipe_write(pipe: u64, bytes: &[u8]) -> Result<usize, i64> {
    // SAFETY: The slice is readable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::PipeWrite as u64,
            pipe,
            bytes.as_ptr() as u64,
            bytes.len() as u64,
            0,
        )
    })
    .map(|count| count as usize)
}

/// Releases one end of a pipe (`PIPE_READ` or `PIPE_WRITE`).
pub fn pipe_close(pipe: u64, end: u64) -> Result<(), i64> {
    // SAFETY: No pointers are involved.
    result(unsafe { syscall4(Syscall::PipeClose as u64, pipe, end, 0, 0) }).map(|_| ())
}

/// Deletes a file or an empty directory.
pub fn remove(path: &str) -> Result<(), i64> {
    // SAFETY: The path slice is readable; the kernel validates it.
    result(unsafe {
        syscall4(
            Syscall::Remove as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            0,
            0,
        )
    })
    .map(|_| ())
}

/// Renames or moves a path.
pub fn rename(from: &str, to: &str) -> Result<(), i64> {
    // SAFETY: Both slices are readable; the kernel validates them.
    result(unsafe {
        syscall4(
            Syscall::Rename as u64,
            from.as_ptr() as u64,
            from.len() as u64,
            to.as_ptr() as u64,
            to.len() as u64,
        )
    })
    .map(|_| ())
}

/// Creates a directory.
pub fn make_dir(path: &str) -> Result<(), i64> {
    // SAFETY: The path slice is readable; the kernel validates it.
    result(unsafe {
        syscall4(
            Syscall::MakeDir as u64,
            path.as_ptr() as u64,
            path.len() as u64,
            0,
            0,
        )
    })
    .map(|_| ())
}

/// Queries the terminal mode and standard stream kinds without changing them.
pub fn terminal_mode() -> Result<u64, i64> {
    // SAFETY: No pointers are involved.
    result(unsafe { syscall4(Syscall::TerminalMode as u64, 0, 0, 0, 0) })
}

/// Sets the terminal mode (`TERMINAL_RAW` or 0) and returns the previous
/// query result.
pub fn set_terminal_mode(mode: u64) -> Result<u64, i64> {
    // SAFETY: No pointers are involved.
    result(unsafe { syscall4(Syscall::TerminalMode as u64, 1, mode, 0, 0) })
}

/// Copies the caller's argument block into `buffer`.
pub fn arguments(buffer: &mut [u8]) -> Result<usize, i64> {
    // SAFETY: The buffer is writable; the kernel validates the range.
    result(unsafe {
        syscall4(
            Syscall::Arguments as u64,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
            0,
            0,
        )
    })
    .map(|count| count as usize)
}

/// Convenience constants for callers.
pub const READ: u64 = FileRights::READ;
/// Write access.
pub const WRITE: u64 = FileRights::WRITE;
/// Create on open.
pub const CREATE: u64 = FileRights::CREATE;

/// Human-readable error descriptions, in the style of `strerror`.
#[must_use]
pub fn error_message(error: i64) -> &'static str {
    match error {
        errno::ENOSYS => "Function not implemented",
        errno::EINVAL => "Invalid argument",
        errno::EACCES => "Permission denied",
        errno::ENOENT => "No such file or directory",
        errno::ENOSPC => "No space or resource left",
        errno::EPERM => "Outside your home folder",
        errno::EBADF => "Bad handle",
        errno::ENOGRANT => "No portal grant",
        errno::EIO => "Input/output error",
        errno::EAGAIN => "Resource temporarily unavailable",
        errno::ETIMEDOUT => "Timed out",
        errno::EPIPE => "Broken pipe",
        errno::EEXIST => "File exists",
        errno::ENOTDIR => "Not a directory",
        errno::EISDIR => "Is a directory",
        errno::ENOTEMPTY => "Directory not empty",
        errno::ENAMETOOLONG => "File name too long",
        _ => "Unknown error",
    }
}

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
        errno::EIO => "EIO",
        errno::EAGAIN => "EAGAIN",
        errno::ETIMEDOUT => "ETIMEDOUT",
        errno::EPIPE => "EPIPE",
        errno::EEXIST => "EEXIST",
        errno::ENOTDIR => "ENOTDIR",
        errno::EISDIR => "EISDIR",
        errno::ENOTEMPTY => "ENOTEMPTY",
        errno::ENAMETOOLONG => "ENAMETOOLONG",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_too_long_renders_a_readable_diagnostic() {
        assert_eq!(error_message(errno::ENAMETOOLONG), "File name too long");
        assert_eq!(error_name(errno::ENAMETOOLONG), "ENAMETOOLONG");
    }
}

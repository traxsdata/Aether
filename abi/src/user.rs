// SPDX-License-Identifier: Apache-2.0
//! User-space syscall ABI.
//!
//! The first user-mode contract is deliberately small. System calls enter
//! through vector `0x80` with the number in `RAX`, arguments in
//! `RDI`, `RSI`, `RDX`, `R10`, `R8`, and `R9`, and the result in `RAX`.
//! Negative results are errno values encoded as two's-complement `i64`.
//!
//! Handles are opaque table indices with a generation check performed by the
//! kernel. They are never kernel addresses. User pointers are interpreted only
//! inside the caller's user window and are validated by the kernel before any
//! copy.

/// Interrupt vector used for system calls.
pub const SYSCALL_VECTOR: u8 = 0x80;

/// Virtual base of the user address window.
///
/// PML4 slot 4 is reserved for user mappings. Kernel memory lives in the low
/// identity map and is never mapped with the user bit.
pub const USER_BASE: u64 = 0x0000_0200_0000_0000;

/// Size of the user address window (128 MiB).
pub const USER_WINDOW_BYTES: u64 = 128 * 1024 * 1024;

/// Highest address a user pointer may name.
pub const USER_LIMIT: u64 = USER_BASE + USER_WINDOW_BYTES;

/// User stack top, mapped downward from this address.
pub const USER_STACK_TOP: u64 = USER_BASE + 64 * 1024 * 1024;

/// Number of stack pages mapped for a user task.
pub const USER_STACK_PAGES: u64 = 16;

/// Maximum bytes copied by one syscall.
pub const MAX_SYSCALL_IO: usize = 4096;

/// System call numbers.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Syscall {
    /// Yield the CPU to the next runnable task. Returns zero.
    Yield = 0,
    /// Terminate the current process with a status. Does not return.
    Exit = 1,
    /// Write bytes to a file descriptor. Only descriptor 1 (console) is
    /// initially defined. Returns the number of bytes written.
    Write = 2,
    /// Return the current process identifier.
    GetPid = 3,
    /// Open a path relative to the caller's root. `arg1` is a user pointer,
    /// `arg2` the length, and `arg3` a [`FileRights`] bit set.
    Open = 4,
    /// Read from an open file handle.
    Read = 5,
    /// Write to an open file handle.
    WriteFile = 6,
    /// Close an open file handle.
    Close = 7,
    /// List a directory into a user buffer. Returns bytes written.
    List = 8,
    /// Return a file's size and rights without opening it.
    Stat = 9,
    /// Yield until the next timer tick. Used by tests and cooperative loops.
    Sleep = 10,
    /// Return the current system uptime in timer ticks.
    Uptime = 11,
    /// Open a file through the file portal, which requires a desktop grant.
    PortalOpen = 12,
    /// Ask the desktop to display a window for the current process.
    CreateWindow = 13,
    /// Return the scheduler's active task count (diagnostics only).
    TaskCount = 14,
    /// Create a new IPC endpoint for the caller. The endpoint is a bounded,
    /// task-local message queue, and the caller receives a full-rights
    /// capability (`SEND|RECEIVE|TRANSFER`). Returns the capability handle,
    /// or `ENOSPC` when the endpoint pool is exhausted. Creating endpoints is
    /// a privileged kernel mechanism exposed through this call; the global
    /// bound makes repeated creation fail instead of growing kernel memory.
    EndpointCreate = 15,
    /// Send a bounded IPC message through an endpoint capability owned by the
    /// caller. `arg1` is the endpoint capability handle, `arg2` the payload
    /// pointer, `arg3` the payload length (the kernel enforces the endpoint
    /// message bound of 32 bytes), and `arg4` an optional capability handle
    /// to transfer (0 = no transfer). The transferred capability must carry
    /// the `TRANSFER` right and loses it on arrival. Returns zero, or
    /// `EAGAIN` when the endpoint queue is full.
    EndpointSend = 16,
    /// Receive a bounded IPC message. `arg1` is the endpoint capability
    /// handle, `arg2` the buffer pointer, `arg3` the buffer length (the
    /// kernel requires room for a maximum-size message), and `arg4` the
    /// pointer of a required writable 4-byte field that receives the
    /// transferred capability handle (0 is rejected, so a transferred handle
    /// is never installed without a place to report it). The sender identity
    /// comes from the kernel; a message field never names the sender. Returns
    /// the payload length, or `EAGAIN` when the queue is empty.
    EndpointReceive = 17,
}

impl Syscall {
    /// Decodes a raw system call number, rejecting reserved values.
    #[must_use]
    pub const fn from_raw(value: u64) -> Option<Self> {
        match value {
            0 => Some(Self::Yield),
            1 => Some(Self::Exit),
            2 => Some(Self::Write),
            3 => Some(Self::GetPid),
            4 => Some(Self::Open),
            5 => Some(Self::Read),
            6 => Some(Self::WriteFile),
            7 => Some(Self::Close),
            8 => Some(Self::List),
            9 => Some(Self::Stat),
            10 => Some(Self::Sleep),
            11 => Some(Self::Uptime),
            12 => Some(Self::PortalOpen),
            13 => Some(Self::CreateWindow),
            14 => Some(Self::TaskCount),
            15 => Some(Self::EndpointCreate),
            16 => Some(Self::EndpointSend),
            17 => Some(Self::EndpointReceive),
            _ => None,
        }
    }
}

/// File access flags passed to [`Syscall::Open`].
pub struct FileRights;

impl FileRights {
    /// Read access.
    pub const READ: u64 = 1 << 0;
    /// Write access.
    pub const WRITE: u64 = 1 << 1;
    /// Create the file if it does not exist.
    pub const CREATE: u64 = 1 << 2;
}

/// Errno values returned as negative `i64`.
pub mod errno {
    /// Operation is not implemented for this object.
    pub const ENOSYS: i64 = -1;
    /// Invalid argument or malformed pointer.
    pub const EINVAL: i64 = -2;
    /// The caller lacks the required capability or right.
    pub const EACCES: i64 = -3;
    /// The named object does not exist.
    pub const ENOENT: i64 = -4;
    /// The operation would exceed a quota or table bound.
    pub const ENOSPC: i64 = -5;
    /// The path escapes the caller's root.
    pub const EPERM: i64 = -6;
    /// The handle is stale, forged, or names the wrong object type.
    pub const EBADF: i64 = -7;
    /// No grant exists for this portal request.
    pub const ENOGRANT: i64 = -8;
    /// The backing device reported an I/O or format error.
    pub const EIO: i64 = -9;
    /// The endpoint queue is full (send) or empty (receive); retry later.
    pub const EAGAIN: i64 = -10;
}

/// Result of the boot-time user-mode self-test.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserTestResult {
    /// The user task has not reported yet.
    Pending = 0,
    /// The user task completed normally.
    Passed = 1,
    /// A syscall or sandbox expectation failed.
    Failed = 2,
    /// The user task attempted an operation outside its sandbox and was
    /// terminated by a fault.
    Faulted = 3,
}

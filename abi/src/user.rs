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
pub const USER_WINDOW_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Highest address a user pointer may name.
pub const USER_LIMIT: u64 = USER_BASE + USER_WINDOW_BYTES;

/// User stack top, mapped downward from this address.
pub const USER_STACK_TOP: u64 = USER_BASE + 64 * 1024 * 1024;

/// Number of stack pages mapped for a user task.
pub const USER_STACK_PAGES: u64 = 1024;

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
    /// message bound of 64 bytes), and `arg4` an optional capability handle
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
    /// Close an endpoint capability. Every alias of that endpoint in the
    /// caller's table is revoked. When the caller owns the endpoint, the
    /// object is destroyed and all outstanding aliases in every task become
    /// stale (`EBADF`). Returns zero.
    EndpointClose = 18,
    /// Receive a bounded IPC message, waiting until one arrives or the
    /// timeout expires. Arguments are the same as `EndpointReceive`, plus
    /// `arg5` (R8) whose low 32 bits are the timeout in timer ticks; zero
    /// means "wait without a deadline". Returns the payload length,
    /// `EAGAIN` on timeout, or another endpoint error. The task yields the
    /// CPU while it waits.
    EndpointReceiveTimeout = 19,
    /// Ask the desktop for an exact-file portal grant. `arg1` is the path
    /// pointer, `arg2` the length, and `arg3` the `FileRights` set. The
    /// request is display state only; it grants no authority until a user
    /// approves it. Returns the pending-request index, or `ENOSPC`/`EINVAL`.
    PortalRequest = 20,
    /// Start a terminal program as a child of the caller. `arg1`/`arg2` name
    /// an argument block (see [`ARGUMENT_BLOCK_BYTES`]): NUL-separated strings
    /// whose first entry is the working directory and whose second is the
    /// program name from [`TERMINAL_PROGRAMS`]. `arg3` selects standard input
    /// and `arg4` standard output: 0 is the terminal, any other value a pipe
    /// the caller holds the matching end of. Only tasks attached to the
    /// terminal session may spawn. Returns the child's process id.
    Spawn = 21,
    /// Collect a child's exit status without blocking. Returns the status
    /// (0..=255 for `exit`, 128 + n when killed or faulted), `EAGAIN` while
    /// the child runs, or `ENOENT` when `arg1` is not the caller's child.
    Wait = 22,
    /// Terminate a process in the caller's terminal session. Returns zero, or
    /// `EACCES` for a process outside the session.
    Kill = 23,
    /// Fill `arg1`/`arg2` with [`PROCESS_RECORD_BYTES`]-byte process records.
    /// Returns the number of records written. Terminal session only.
    ProcessList = 24,
    /// Fill `arg1`/`arg2` with a [`SYSTEM_INFO_BYTES`]-byte system summary.
    /// Returns the number of bytes written. Terminal session only.
    SystemInfo = 25,
    /// Read standard input into `arg1`/`arg2`. Returns the byte count, zero
    /// at end of input (a pipe whose writers are gone), or `EAGAIN` while no
    /// input is queued.
    ReadInput = 26,
    /// Return the terminal size as `(columns << 16) | rows`, or `EACCES` for a
    /// task without a terminal.
    TerminalSize = 27,
    /// Create a bounded pipe. The caller holds both ends. Returns its id.
    PipeCreate = 28,
    /// Read from a pipe the caller holds the read end of; same results as
    /// [`Syscall::ReadInput`].
    PipeRead = 29,
    /// Write to a pipe the caller holds the write end of. Returns the bytes
    /// accepted (possibly fewer than requested), `EAGAIN` when full, or
    /// `EPIPE` when no reader remains.
    PipeWrite = 30,
    /// Release one end of a pipe: `arg2` is [`PIPE_READ`] or [`PIPE_WRITE`].
    PipeClose = 31,
    /// Delete a file or an empty directory below the caller's root.
    /// Terminal session only.
    Remove = 32,
    /// Rename or move a path below the caller's root: `arg1`/`arg2` name the
    /// source and `arg3`/`arg4` the destination. Terminal session only.
    Rename = 33,
    /// Create a directory below the caller's root. Terminal session only.
    MakeDir = 34,
    /// Copy the caller's argument block into `arg1`/`arg2`. Returns its
    /// length.
    Arguments = 35,
    /// Query or change terminal input handling. When `arg1` is 1 the mode
    /// becomes `arg2` ([`TERMINAL_RAW`] or 0). Returns the mode in bits 0..8
    /// plus [`STDIN_IS_TERMINAL`] and [`STDOUT_IS_TERMINAL`].
    TerminalMode = 36,
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
            18 => Some(Self::EndpointClose),
            19 => Some(Self::EndpointReceiveTimeout),
            20 => Some(Self::PortalRequest),
            21 => Some(Self::Spawn),
            22 => Some(Self::Wait),
            23 => Some(Self::Kill),
            24 => Some(Self::ProcessList),
            25 => Some(Self::SystemInfo),
            26 => Some(Self::ReadInput),
            27 => Some(Self::TerminalSize),
            28 => Some(Self::PipeCreate),
            29 => Some(Self::PipeRead),
            30 => Some(Self::PipeWrite),
            31 => Some(Self::PipeClose),
            32 => Some(Self::Remove),
            33 => Some(Self::Rename),
            34 => Some(Self::MakeDir),
            35 => Some(Self::Arguments),
            36 => Some(Self::TerminalMode),
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
    /// A blocking operation reached its deadline without a message.
    pub const ETIMEDOUT: i64 = -11;
    /// A pipe write found no remaining reader.
    pub const EPIPE: i64 = -12;
    /// The destination name already exists.
    pub const EEXIST: i64 = -13;
    /// A path component is a file where a directory was required.
    pub const ENOTDIR: i64 = -14;
    /// The operation needs a file but the path names a directory.
    pub const EISDIR: i64 = -15;
    /// The directory is not empty.
    pub const ENOTEMPTY: i64 = -16;
}

/// Maximum size of a [`Syscall::Spawn`] argument block.
pub const ARGUMENT_BLOCK_BYTES: usize = 512;
/// Maximum number of strings (working directory included) in an argument
/// block.
pub const MAX_ARGUMENTS: usize = 32;

/// Programs the terminal can start. They are entry points of the one native
/// userland image the kernel embeds; the kernel refuses any other name.
pub const TERMINAL_PROGRAMS: &[&str] = &[
    "sh", "ls", "cat", "cp", "mv", "rm", "mkdir", "rmdir", "touch", "echo", "pwd", "head", "tail",
    "wc", "grep", "find", "stat", "tree", "sort", "uniq", "tee", "du", "df", "ps", "kill",
    "uptime", "free", "uname", "clear", "whoami", "id", "hostname", "sleep", "true", "false",
    "env", "seq", "yes", "basename", "dirname", "htop", "top", "nano", "help",
];

/// Returns `true` when `name` is a program the terminal may start.
#[must_use]
pub fn is_terminal_program(name: &str) -> bool {
    TERMINAL_PROGRAMS.contains(&name)
}

/// Terminal mode bit: Ctrl+C is delivered to the program as byte 3 instead
/// of interrupting the running job. The mode ends with the task that set it.
pub const TERMINAL_RAW: u64 = 1;
/// [`Syscall::TerminalMode`] result bit: standard input is the terminal.
pub const STDIN_IS_TERMINAL: u64 = 1 << 8;
/// [`Syscall::TerminalMode`] result bit: standard output is the terminal.
pub const STDOUT_IS_TERMINAL: u64 = 1 << 9;

/// [`Syscall::PipeClose`] selector for the read end.
pub const PIPE_READ: u64 = 1;
/// [`Syscall::PipeClose`] selector for the write end.
pub const PIPE_WRITE: u64 = 2;

/// Bytes in one [`Syscall::ProcessList`] record.
pub const PROCESS_RECORD_BYTES: usize = 48;
/// Bytes of a process name in a record (NUL padded).
pub const PROCESS_NAME_BYTES: usize = 24;

/// Scheduling state reported in a process record.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    /// Waiting for the CPU.
    Ready = 0,
    /// On the CPU.
    Running = 1,
    /// Waiting for an event.
    Blocked = 2,
    /// Exited; the parent has not collected the status yet.
    Exited = 3,
    /// Retired by a fault.
    Faulted = 4,
}

/// Kind of task reported in a process record.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessKind {
    /// The kernel idle loop (desktop, input, console).
    Idle = 0,
    /// Ring-0 kernel worker.
    Kernel = 1,
    /// Native ring-3 application.
    Native = 2,
    /// Native ring-3 service.
    Service = 3,
    /// Linux ABI process.
    Linux = 4,
    /// Linux ABI thread.
    LinuxThread = 5,
    /// Native program started from the terminal.
    Terminal = 6,
}

/// Process-record flag: the task belongs to the terminal session.
pub const PROCESS_FLAG_SESSION: u8 = 1 << 0;
/// Process-record flag: the task runs at CPL3.
pub const PROCESS_FLAG_USER: u8 = 1 << 1;

/// One entry of [`Syscall::ProcessList`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessRecord {
    /// Process id.
    pub pid: u32,
    /// Parent process id (0 for kernel-created tasks).
    pub parent: u32,
    /// Scheduling state.
    pub state: u8,
    /// [`ProcessKind`] value.
    pub kind: u8,
    /// `PROCESS_FLAG_*` bits.
    pub flags: u8,
    /// Resident memory in KiB, or zero when not tracked.
    pub memory_kib: u32,
    /// Timer ticks spent on the CPU.
    pub cpu_ticks: u64,
    /// Program name, NUL padded.
    pub name: [u8; PROCESS_NAME_BYTES],
}

impl ProcessRecord {
    /// Serializes the record in its little-endian wire form.
    #[must_use]
    pub fn encode(&self) -> [u8; PROCESS_RECORD_BYTES] {
        let mut out = [0u8; PROCESS_RECORD_BYTES];
        out[0..4].copy_from_slice(&self.pid.to_le_bytes());
        out[4..8].copy_from_slice(&self.parent.to_le_bytes());
        out[8] = self.state;
        out[9] = self.kind;
        out[10] = self.flags;
        out[12..16].copy_from_slice(&self.memory_kib.to_le_bytes());
        out[16..24].copy_from_slice(&self.cpu_ticks.to_le_bytes());
        out[24..48].copy_from_slice(&self.name);
        out
    }

    /// Parses a record from its wire form.
    #[must_use]
    pub fn decode(bytes: &[u8; PROCESS_RECORD_BYTES]) -> Self {
        let u32_at = |offset: usize| {
            u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        let mut cpu = [0u8; 8];
        cpu.copy_from_slice(&bytes[16..24]);
        let mut name = [0u8; PROCESS_NAME_BYTES];
        name.copy_from_slice(&bytes[24..48]);
        Self {
            pid: u32_at(0),
            parent: u32_at(4),
            state: bytes[8],
            kind: bytes[9],
            flags: bytes[10],
            memory_kib: u32_at(12),
            cpu_ticks: u64::from_le_bytes(cpu),
            name,
        }
    }

    /// Returns the name without its NUL padding.
    #[must_use]
    pub fn name(&self) -> &str {
        let length = self
            .name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(PROCESS_NAME_BYTES);
        core::str::from_utf8(&self.name[..length]).unwrap_or("?")
    }
}

/// Bytes in a [`Syscall::SystemInfo`] summary.
pub const SYSTEM_INFO_BYTES: usize = 96;

/// The [`Syscall::SystemInfo`] summary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SystemInfo {
    /// Timer ticks since boot.
    pub uptime_ticks: u64,
    /// Timer frequency.
    pub tick_hz: u32,
    /// Processors the scheduler runs tasks on.
    pub cpu_count: u32,
    /// Usable physical memory in KiB.
    pub memory_total_kib: u64,
    /// Free physical memory in KiB.
    pub memory_free_kib: u64,
    /// Ticks the idle task spent on the CPU.
    pub idle_ticks: u64,
    /// Live tasks.
    pub tasks: u32,
    /// Task-table capacity.
    pub max_tasks: u32,
    /// Filesystem capacity in bytes.
    pub fs_total_bytes: u32,
    /// Filesystem bytes in use.
    pub fs_used_bytes: u32,
    /// Filesystem inode capacity.
    pub fs_inodes: u32,
    /// Filesystem inodes in use.
    pub fs_inodes_used: u32,
    /// Kernel version, NUL padded.
    pub version: [u8; 16],
    /// Build commit, NUL padded.
    pub commit: [u8; 16],
}

impl SystemInfo {
    /// Serializes the summary in its little-endian wire form.
    #[must_use]
    pub fn encode(&self) -> [u8; SYSTEM_INFO_BYTES] {
        let mut out = [0u8; SYSTEM_INFO_BYTES];
        out[0..8].copy_from_slice(&self.uptime_ticks.to_le_bytes());
        out[8..12].copy_from_slice(&self.tick_hz.to_le_bytes());
        out[12..16].copy_from_slice(&self.cpu_count.to_le_bytes());
        out[16..24].copy_from_slice(&self.memory_total_kib.to_le_bytes());
        out[24..32].copy_from_slice(&self.memory_free_kib.to_le_bytes());
        out[32..40].copy_from_slice(&self.idle_ticks.to_le_bytes());
        out[40..44].copy_from_slice(&self.tasks.to_le_bytes());
        out[44..48].copy_from_slice(&self.max_tasks.to_le_bytes());
        out[48..52].copy_from_slice(&self.fs_total_bytes.to_le_bytes());
        out[52..56].copy_from_slice(&self.fs_used_bytes.to_le_bytes());
        out[56..60].copy_from_slice(&self.fs_inodes.to_le_bytes());
        out[60..64].copy_from_slice(&self.fs_inodes_used.to_le_bytes());
        out[64..80].copy_from_slice(&self.version);
        out[80..96].copy_from_slice(&self.commit);
        out
    }

    /// Parses a summary from its wire form.
    #[must_use]
    pub fn decode(bytes: &[u8; SYSTEM_INFO_BYTES]) -> Self {
        let u32_at = |offset: usize| {
            let mut raw = [0u8; 4];
            raw.copy_from_slice(&bytes[offset..offset + 4]);
            u32::from_le_bytes(raw)
        };
        let u64_at = |offset: usize| {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&bytes[offset..offset + 8]);
            u64::from_le_bytes(raw)
        };
        let mut version = [0u8; 16];
        version.copy_from_slice(&bytes[64..80]);
        let mut commit = [0u8; 16];
        commit.copy_from_slice(&bytes[80..96]);
        Self {
            uptime_ticks: u64_at(0),
            tick_hz: u32_at(8),
            cpu_count: u32_at(12),
            memory_total_kib: u64_at(16),
            memory_free_kib: u64_at(24),
            idle_ticks: u64_at(32),
            tasks: u32_at(40),
            max_tasks: u32_at(44),
            fs_total_bytes: u32_at(48),
            fs_used_bytes: u32_at(52),
            fs_inodes: u32_at(56),
            fs_inodes_used: u32_at(60),
            version,
            commit,
        }
    }
}

/// Copies `text` into a NUL-padded fixed field, truncating if needed.
#[must_use]
pub fn fixed_name<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0u8; N];
    let length = text.len().min(N);
    out[..length].copy_from_slice(&text.as_bytes()[..length]);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_syscall_number_round_trips() {
        for raw in 0..64u64 {
            if let Some(call) = Syscall::from_raw(raw) {
                assert_eq!(call as u64, raw);
            }
        }
        assert_eq!(Syscall::from_raw(36), Some(Syscall::TerminalMode));
        assert_eq!(Syscall::from_raw(37), None);
    }

    #[test]
    fn process_record_round_trips() {
        let record = ProcessRecord {
            pid: 12,
            parent: 9,
            state: ProcessState::Running as u8,
            kind: ProcessKind::Terminal as u8,
            flags: PROCESS_FLAG_SESSION | PROCESS_FLAG_USER,
            memory_kib: 4200,
            cpu_ticks: 0x1_0000_0001,
            name: fixed_name("htop"),
        };
        let decoded = ProcessRecord::decode(&record.encode());
        assert_eq!(decoded, record);
        assert_eq!(decoded.name(), "htop");
    }

    #[test]
    fn system_info_round_trips() {
        let info = SystemInfo {
            uptime_ticks: 123_456,
            tick_hz: 100,
            cpu_count: 1,
            memory_total_kib: 1 << 20,
            memory_free_kib: 1 << 19,
            idle_ticks: 99,
            tasks: 7,
            max_tasks: 19,
            fs_total_bytes: 131_072,
            fs_used_bytes: 4096,
            fs_inodes: 32,
            fs_inodes_used: 20,
            version: fixed_name("0.1.0"),
            commit: fixed_name("abcdef123456"),
        };
        assert_eq!(SystemInfo::decode(&info.encode()), info);
    }

    #[test]
    fn program_table_has_unique_names() {
        for (index, name) in TERMINAL_PROGRAMS.iter().enumerate() {
            assert!(!TERMINAL_PROGRAMS[index + 1..].contains(name), "{name}");
            assert!(name.len() < PROCESS_NAME_BYTES);
        }
        assert!(is_terminal_program("htop"));
        assert!(!is_terminal_program("aether-init"));
    }
}

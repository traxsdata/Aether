// SPDX-License-Identifier: Apache-2.0
//! Default-deny Linux syscall policy and per-class audit counters.
//!
//! The dispatcher asks this module for a class before it executes any call.
//! A number that is not in the table is unknown and returns `ENOSYS`. Network
//! device facilities are classified as denied and return `EACCES`. Network
//! calls are routed to the socket layer, which applies the per-application
//! network policy and records its own denials. Every known call increments its
//! class counter, and every denial increments both the class and total denial
//! counters. `/proc/aether/syscalls` exposes the bounded read-only view.

use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Audit class for a Linux syscall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// File, descriptor, directory, and filesystem metadata calls.
    File,
    /// Address-space and mapping calls.
    Memory,
    /// Process, thread, and scheduling calls.
    Process,
    /// Signal installation and delivery calls.
    Signal,
    /// Time queries.
    Time,
    /// Synchronization primitives and multiplexing.
    Sync,
    /// Device and privileged kernel facilities, denied by policy.
    Device,
    /// Socket calls. Routed to the socket layer, which allows local sockets
    /// and denies Internet sockets without the application's network policy.
    Network,
    /// Unknown call number.
    Unknown,
}

const CLASSES: usize = 9;

static CALLS: [AtomicUsize; CLASSES] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];
static DENIED: AtomicUsize = AtomicUsize::new(0);

/// Classifies a Linux x86-64 syscall number.
///
/// The table lists every call the dispatcher may execute plus the denied
/// network/device groups. It is deliberately explicit: an oversight results
/// in `ENOSYS`, not an ambient facility.
#[must_use]
pub const fn classify(number: u64) -> Class {
    match number {
        // read, write, close, stat, fstat, lstat, poll (sync), lseek, pread64,
        // writev, access, dup/dup2, getdents64, openat, statx, readlink,
        // fcntl, prlimit64, sched_getaffinity, inotify, exec/atexit helpers,
        // getcwd, rename, unlink, and getrandom (a file-descriptor-adjacent
        // utility with no device handle in this prototype).
        0 | 1 | 3 | 4 | 5 | 6 | 8 | 17 | 20 | 21 | 28 | 32 | 33 | 72 | 77 | 78 | 79 | 80 | 82
        | 87 | 89 | 204 | 217 | 254 | 255 | 257 | 262 | 267 | 294 | 302 | 318 | 319 | 332 => {
            Class::File
        }
        // mmap, mprotect, munmap, brk, mremap, msync, arch_prctl.
        9 | 10 | 11 | 12 | 25 | 26 | 158 => Class::Memory,
        // clone, fork, vfork, execve, exit, wait4, getpid, set/get groups,
        // set_tid_address and similar process bookkeeping.
        39 | 56 | 57 | 58 | 59 | 60 | 61 | 63 | 102 | 104 | 107 | 108 | 109 | 110 | 112 | 121
        | 186 | 218 | 231 | 273 | 274 | 334 | 435 => Class::Process,
        // rt_sigaction, rt_sigprocmask, rt_sigreturn, kill, tgkill, sigaltstack.
        13 | 14 | 15 | 62 | 131 | 234 => Class::Signal,
        // clock_gettime, nanosleep, clock_nanosleep, timerfd.
        35 | 228 | 230 | 283 | 286 | 287 => Class::Time,
        // poll/yield/futex/epoll/eventfd/pipe.
        7 | 24 | 202 | 232 | 233 | 281 | 290 | 291 | 293 => Class::Sync,
        // Socket calls: routed to the socket layer and its network policy.
        41..=55 | 288 | 299 | 307 => Class::Network,
        // Device, module, mount, tracing, and privileged kernel facilities.
        16
        | 101
        | 139
        | 159
        | 163
        | 164
        | 165..=179
        | 246
        | 298
        | 303..=305
        | 310
        | 311
        | 313
        | 320
        | 321
        | 323 => Class::Device,
        _ => Class::Unknown,
    }
}

/// Records an allowed dispatch in its class counter.
pub fn record(class: Class) {
    let index = index_of(class);
    CALLS[index].fetch_add(1, Ordering::Relaxed);
}

/// Records a denied dispatch and its class counter.
pub fn record_denied(class: Class) {
    let index = index_of(class);
    CALLS[index].fetch_add(1, Ordering::Relaxed);
    DENIED.fetch_add(1, Ordering::Relaxed);
}

/// Records a socket call the socket layer refused by policy. The call itself
/// was already counted in the network class when it was dispatched.
pub fn record_network_denial() {
    DENIED.fetch_add(1, Ordering::Relaxed);
}

/// Records an unknown syscall number.
pub fn record_unknown() {
    CALLS[index_of(Class::Unknown)].fetch_add(1, Ordering::Relaxed);
}

const fn index_of(class: Class) -> usize {
    match class {
        Class::File => 0,
        Class::Memory => 1,
        Class::Process => 2,
        Class::Signal => 3,
        Class::Time => 4,
        Class::Sync => 5,
        Class::Device => 6,
        Class::Network => 7,
        Class::Unknown => 8,
    }
}

/// Returns the number of denied network/device calls.
#[must_use]
pub fn denials() -> usize {
    DENIED.load(Ordering::Relaxed)
}

struct Text {
    buffer: [u8; 512],
    length: usize,
}

impl Write for Text {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let end = self
            .length
            .checked_add(value.len())
            .ok_or(core::fmt::Error)?;
        if end > self.buffer.len() {
            return Err(core::fmt::Error);
        }
        self.buffer[self.length..end].copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}

/// Copies the audit report into `out` starting at byte `offset`.
///
/// The report is bounded and generated on demand, so it is safe to expose as a
/// read-only pseudo-file. Returns the number of bytes copied.
pub fn audit_text(offset: usize, out: &mut [u8]) -> usize {
    let mut text = Text {
        buffer: [0; 512],
        length: 0,
    };
    let names = [
        "file", "memory", "process", "signal", "time", "sync", "device", "network", "unknown",
    ];
    let _ = writeln!(text, "# aether linux syscall audit");
    for (index, name) in names.iter().enumerate() {
        let _ = writeln!(text, "{name}={}", CALLS[index].load(Ordering::Relaxed));
    }
    let _ = writeln!(text, "denied={}", denials());
    if offset >= text.length {
        return 0;
    }
    let available = text.length - offset;
    let length = available.min(out.len());
    out[..length].copy_from_slice(&text.buffer[offset..offset + length]);
    length
}

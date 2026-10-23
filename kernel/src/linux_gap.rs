// SPDX-License-Identifier: Apache-2.0
//! Per-application Linux ABI gap counter.
//!
//! The Linux layer implements a subset of the syscall and option space. Where
//! a guest asks for something this kernel does not provide — an unknown
//! syscall number, a device-class facility, or unsupported flag bits on
//! `clone`, `clone3`, `mmap`, `mremap`, `prctl`, `fcntl`, `socket`, or
//! `setsockopt` — the dispatcher answers with `ENOSYS`, `EINVAL`,
//! `EOPNOTSUPP`, `EACCES`, or `EAFNOSUPPORT` and records the hit here.
//!
//! A distinct gap is the combination of an application slot, a syscall
//! number, a classification tag, and the offending level/name values. The
//! table is a fixed array: recording never allocates and at most `ROWS`
//! distinct gaps are retained; once the table is full, new distinct gaps are
//! dropped. The first occurrence of each distinct gap writes one serial line,
//!
//! `linux-gap app=<id> syscall=<n>/<name> detail=<tag> level=<hex> name=<hex>`
//!
//! and every later occurrence of the same gap only bumps its count. Syscall
//! numbers outside the dispatcher's own table print with the name `unknown`;
//! app slots at or beyond `linux::MAX_APPS` (kernel and fixture tasks) render
//! as `linux`. The shell command `linux-gaps [APP]` prints the table,
//! highest count first.

use core::cell::UnsafeCell;

use crate::arch::x86_64::timer::InterruptGuard;
use crate::linux;
use crate::scheduler;
use crate::serial_line;

/// Maximum distinct gaps retained for the whole boot, all apps combined.
const ROWS: usize = 48;

/// What was unsupported inside an otherwise known syscall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    /// A syscall number the dispatcher does not implement at all.
    UnknownSyscall,
    /// A device-class syscall the default-deny policy fails with `EACCES`.
    DeviceFacility,
    /// Unsupported `clone` flag or signal bits.
    CloneFlags,
    /// Unsupported `clone3` flags, fields, or combinations.
    Clone3Flags,
    /// Unsupported `mmap` flag or protection bits.
    MmapFlags,
    /// Unsupported `mremap` flag bits.
    MremapFlags,
    /// An `fcntl` command the dispatcher does not implement.
    FcntlCommand,
    /// A `prctl` option the dispatcher does not implement.
    PrctlOption,
    /// An unsupported socket family or kind bits.
    SocketKind,
    /// An unsupported `setsockopt` level or option.
    SetsockoptOption,
}

/// One retained distinct gap.
#[derive(Clone, Copy)]
struct Row {
    app: u64,
    nr: u64,
    tag: Tag,
    /// Primary offending value: flag bits, option, command, or domain.
    level: u32,
    /// Secondary value where present: protections, kind, or option name.
    name: u32,
    count: u64,
}

struct Table {
    rows: [Row; ROWS],
    used: usize,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static TABLE: Global<Table> = Global(UnsafeCell::new(Table {
    rows: [const {
        Row {
            app: 0,
            nr: 0,
            tag: Tag::UnknownSyscall,
            level: 0,
            name: 0,
            count: 0,
        }
    }; ROWS],
    used: 0,
}));

/// Records one gap hit for the currently running application.
///
/// `level` and `name` carry the offending values (flag bits, option number,
/// socket level, and so on); pass zero for both when the syscall number
/// itself is the gap.
pub fn record(nr: u64, tag: Tag, level: u32, name: u32) {
    let _guard = InterruptGuard::enter();
    let app = scheduler::current_app_slot() as u64;
    // SAFETY: The interrupt guard serializes table access on the single CPU.
    let table = unsafe { &mut *TABLE.0.get() };
    for row in &mut table.rows[..table.used] {
        if row.app == app
            && row.nr == nr
            && row.tag == tag
            && row.level == level
            && row.name == name
        {
            row.count += 1;
            return;
        }
    }
    if table.used == ROWS {
        return;
    }
    table.rows[table.used] = Row {
        app,
        nr,
        tag,
        level,
        name,
        count: 1,
    };
    table.used += 1;
    serial_line!(
        "linux-gap app={} syscall={}/{} detail={} level={:#x} name={:#x}",
        app_id(app),
        nr,
        syscall_name(nr),
        tag_text(tag),
        level,
        name
    );
}

/// Prints the gap table, highest count first.
///
/// An empty `filter` prints every row; otherwise only rows whose
/// application identity matches it.
pub fn print(filter: &str) {
    let mut entries = [const {
        Row {
            app: 0,
            nr: 0,
            tag: Tag::UnknownSyscall,
            level: 0,
            name: 0,
            count: 0,
        }
    }; ROWS];
    let used = {
        let _guard = InterruptGuard::enter();
        // SAFETY: The interrupt guard serializes table access on the single
        // CPU; `used` is bounded by `ROWS` through the insert path.
        let table = unsafe { &*TABLE.0.get() };
        let used = table.used;
        entries[..used].copy_from_slice(&table.rows[..used]);
        used
    };
    // Selection sort: count descending, then syscall number ascending.
    for i in 0..used.saturating_sub(1) {
        let mut best = i;
        for j in (i + 1)..used {
            if sort_key(&entries[j]) < sort_key(&entries[best]) {
                best = j;
            }
        }
        entries.swap(i, best);
    }
    let mut printed = 0usize;
    for row in &entries[..used] {
        let id = app_id(row.app);
        if !filter.is_empty() && id != filter {
            continue;
        }
        printed += 1;
        serial_line!(
            "linux-gap app={} syscall={}/{} count={} detail={} level={:#x} name={:#x}",
            id,
            row.nr,
            syscall_name(row.nr),
            row.count,
            tag_text(row.tag),
            row.level,
            row.name
        );
    }
    if printed == 0 {
        if filter.is_empty() {
            serial_line!("linux-gap: no gaps recorded");
        } else {
            serial_line!("linux-gap: no gaps for '{filter}'");
        }
    }
}

/// Sort key: higher counts first, then lower syscall numbers first.
fn sort_key(row: &Row) -> (u64, u64) {
    (u64::MAX - row.count, row.nr)
}

fn app_id(app: u64) -> &'static str {
    if app as usize >= linux::MAX_APPS {
        return "linux";
    }
    linux::app_identity(app as usize).map_or("linux", |(id, _, _)| id)
}

/// The dispatcher's own known x86-64 syscall names; anything else is
/// `unknown` rather than guessed.
fn syscall_name(nr: u64) -> &'static str {
    match nr {
        9 => "mmap",
        25 => "mremap",
        41 => "socket",
        54 => "setsockopt",
        56 => "clone",
        72 => "fcntl",
        157 => "prctl",
        317 => "seccomp",
        435 => "clone3",
        _ => "unknown",
    }
}

fn tag_text(tag: Tag) -> &'static str {
    match tag {
        Tag::UnknownSyscall => "unknown syscall",
        Tag::DeviceFacility => "device facility (denied)",
        Tag::CloneFlags => "clone flags",
        Tag::Clone3Flags => "clone3 flags",
        Tag::MmapFlags => "mmap flags",
        Tag::MremapFlags => "mremap flags",
        Tag::FcntlCommand => "fcntl command",
        Tag::PrctlOption => "prctl option",
        Tag::SocketKind => "socket family/kind",
        Tag::SetsockoptOption => "socket option",
    }
}

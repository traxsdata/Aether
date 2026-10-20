// SPDX-License-Identifier: Apache-2.0
//! Kernel file service.
//!
//! The service owns the mounted filesystem and translates per-process roots
//! and file capabilities into filesystem operations. The syscall layer passes
//! only validated user pointers here.
//!
//! A task never names an inode directly. `open` creates an open-file object in
//! the filesystem and grants a generation-checked capability for it in the
//! caller's table. `read`, `write`, and `close` require that capability, so a
//! handle from another task or a stale handle is rejected before any file data
//! is touched.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::scheduler;
use crate::syscall::{user_bytes, user_bytes_mut, user_str};
use aether_abi::user::{FileRights, errno};
use aether_fs::{Access, AetherFs, FsError, OpenId, RamDisk};
use aether_ipc::{CapError, CapHandle, CapTable, ObjectId, ObjectType, Rights};
use aether_memory::frame::PhysFrame;
use aether_services::runtime::RuntimeProfile;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

struct Global<T>(UnsafeCell<T>);

// SAFETY: Every access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static FS: Global<AetherFs<RamDisk>> = Global(UnsafeCell::new(AetherFs::new(RamDisk::new())));

static OPENS: AtomicUsize = AtomicUsize::new(0);
static ESCAPE_DENIALS: AtomicUsize = AtomicUsize::new(0);
static ACCESS_DENIALS: AtomicUsize = AtomicUsize::new(0);
static QUOTA_DENIALS: AtomicUsize = AtomicUsize::new(0);

/// Root directory of the native application.
///
/// The sandbox demo task runs under the installed package's identity, so its
/// private root, permissions, and limits come from one verified manifest.
pub const DEMO_ROOT: &str = "/apps/org.aether.demo";
/// Root directory of the sibling application used by the sandbox test.
pub const OTHER_ROOT: &str = "/apps/other";

/// Formats and seeds the virtual volume.
pub fn init() -> Result<(), FsError> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this is the only writer during boot.
    // The filesystem lives in static storage; constructing a 128 KiB disk on
    // the boot stack would overflow it.
    let filesystem = unsafe { &mut *FS.0.get() };
    filesystem.format()?;

    for directory in [
        "apps",
        "apps/other",
        "apps/other/private",
        "system",
        "system/images",
        "system/updates",
        "home",
        "portal",
    ] {
        filesystem.mkdir("/", directory)?;
    }

    filesystem.write_file(
        "/",
        "/apps/other/private/secret.txt",
        b"other application secret\n",
    )?;
    filesystem.write_file("/", "/system/policy.txt", b"aether policy v1\n")?;
    filesystem.write_file("/", "/system/images/current", b"aether-0.1.0\n")?;
    filesystem.write_file("/", "/system/images/previous", b"aether-0.0.9\n")?;
    filesystem.write_file(
        "/",
        "/system/install.txt",
        b"version=0.1.0\ncounter=1\nchannel=developer\n",
    )?;
    filesystem.write_file("/", "/home/welcome.txt", b"welcome to aether\n")?;
    filesystem.write_file("/", "/portal/README.txt", b"file portal grants\n")?;

    Ok(())
}

/// Creates the private workspace of the installed demo application after the
/// package root exists.
///
/// `packaging::install` owns the package root directory; the task sandbox's
/// `/private` workspace is filesystem service state and must not be part of
/// the signed payload. Seeding therefore runs after package installation.
pub fn seed_application_roots() -> Result<(), FsError> {
    let mut path = [0u8; 80];
    let root = DEMO_ROOT.as_bytes();
    const SUFFIX: &str = "/private";
    if root.len() + SUFFIX.len() > path.len() {
        return Err(FsError::PathTooLong);
    }
    path[..root.len()].copy_from_slice(root);
    let end = root.len() + SUFFIX.len();
    path[root.len()..end].copy_from_slice(SUFFIX.as_bytes());
    let path = core::str::from_utf8(&path[..end]).map_err(|_| FsError::Corrupt)?;
    with_fs(|filesystem| filesystem.mkdir("/", path))
}

/// Returns the number of paths rejected for escaping a process root.
#[must_use]
pub fn escape_denials() -> usize {
    ESCAPE_DENIALS.load(Ordering::Acquire)
}

/// Returns the number of opens rejected for missing rights or scope.
#[must_use]
pub fn access_denials() -> usize {
    ACCESS_DENIALS.load(Ordering::Acquire)
}

/// Returns the number of successful opens.
#[must_use]
pub fn open_count() -> usize {
    OPENS.load(Ordering::Acquire)
}

/// Returns the number of opens rejected for exceeding a per-application
/// resource limit.
#[must_use]
pub fn quota_denials() -> usize {
    QUOTA_DENIALS.load(Ordering::Acquire)
}

fn with_fs<R>(f: impl FnOnce(&mut AetherFs<RamDisk>) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the duration of the closure, and the
    // filesystem is initialized during boot before any task can call in.
    let slot = unsafe { &mut *FS.0.get() };
    f(slot)
}

fn map_error(error: FsError) -> i64 {
    match error {
        FsError::Escape => {
            ESCAPE_DENIALS.fetch_add(1, Ordering::Release);
            errno::EPERM
        }
        FsError::Access => {
            ACCESS_DENIALS.fetch_add(1, Ordering::Release);
            errno::EACCES
        }
        FsError::NotFound => errno::ENOENT,
        FsError::NoSpace | FsError::TooManyOpen | FsError::FileTooLarge => errno::ENOSPC,
        FsError::BadHandle => errno::EBADF,
        FsError::NameTooLong | FsError::PathTooLong => errno::EINVAL,
        FsError::Io | FsError::NotFormatted | FsError::Corrupt => errno::EIO,
        FsError::NotDirectory | FsError::IsDirectory | FsError::Exists => errno::EINVAL,
    }
}

fn map_cap_error(error: CapError) -> i64 {
    match error {
        CapError::InsufficientRights | CapError::InvalidRights => {
            ACCESS_DENIALS.fetch_add(1, Ordering::Release);
            errno::EACCES
        }
        CapError::TableFull => errno::ENOSPC,
        CapError::InvalidHandle | CapError::WrongObjectType => errno::EBADF,
    }
}

fn access_from_rights(rights: u64) -> Option<Access> {
    let known = FileRights::READ | FileRights::WRITE | FileRights::CREATE;
    if rights & !known != 0 {
        return None;
    }
    let mut access = Access::from_bits(0);
    if rights & FileRights::READ != 0 {
        access = access | Access::READ;
    }
    if rights & FileRights::WRITE != 0 {
        access = access | Access::WRITE;
    }
    if rights & FileRights::CREATE != 0 {
        access = access | Access::CREATE;
    }
    if access.bits() == 0 {
        return None;
    }
    Some(access)
}

fn cap_rights(access: Access) -> Rights {
    let mut rights = Rights::empty();
    if access.contains(Access::READ) {
        rights = rights | Rights::READ;
    }
    if access.contains(Access::WRITE) {
        rights = rights | Rights::WRITE;
    }
    rights
}

fn encode_handle(handle: CapHandle) -> u64 {
    (u64::from(handle.generation) << 8) | u64::from(handle.index)
}

fn decode_handle(value: u64) -> Option<CapHandle> {
    // Handle 0 is reserved (slot generations start at 1), and bytes above the
    // 24-bit encoding are rejected so a caller cannot smuggle extra bits and
    // rely on truncation to name a valid slot. This matches the endpoint
    // decoder and the system ABI contract.
    if value == 0 || value > 0x00ff_ffff {
        return None;
    }
    let index = (value & 0xff) as u8;
    let generation = (value >> 8) as u16;
    Some(CapHandle { index, generation })
}

/// Returns the per-application runtime limits for a task root.
///
/// The lookup is by kernel-owned root; a task cannot influence which profile,
/// if any, gates its opens. `/` (kernel console) and unregistered roots have
/// no profile and are bounded only by the filesystem's global quota.
fn profile_for_root(root: &str) -> Option<RuntimeProfile<'static>> {
    crate::runtime::profile_for(root)
}

/// Enforces the verified manifest's portal permission for a task root.
///
/// The desktop grant authorizes one file; the packaged application's declared
/// permissions decide whether it may use the portal contract at all. A task
/// with no registered profile (for example the kernel console) has no profile
/// gate.
fn portal_permission(root: &str, rights: u64) -> Result<(), i64> {
    let Some(profile) = profile_for_root(root) else {
        return Ok(());
    };
    // Every requested right needs its own declaration. A combined READ|WRITE
    // request must not pass on PORTAL_WRITE alone, or an application that
    // declared write-only access would receive a readable capability.
    let needs_read = rights & FileRights::READ != 0;
    let needs_write = rights & FileRights::WRITE != 0;
    if profile.allows_portal(needs_read, needs_write) {
        Ok(())
    } else {
        ACCESS_DENIALS.fetch_add(1, Ordering::Release);
        Err(errno::EACCES)
    }
}

/// Grants a file capability for `open`, enforcing the application's declared
/// open-file limit.
fn grant_file_open(
    root: &str,
    caps: &mut CapTable,
    filesystem: &mut AetherFs<RamDisk>,
    open: OpenId,
    access: Access,
) -> Result<u64, i64> {
    let object = ObjectId {
        index: u16::from(open.index),
        generation: open.generation,
    };
    if let Some(profile) = profile_for_root(root) {
        let held = caps.file_count();
        let next = held.saturating_add(1).min(u8::MAX as usize) as u8;
        if !profile.allows_open_files(next) {
            let _ = filesystem.close(open);
            QUOTA_DENIALS.fetch_add(1, Ordering::Release);
            return Err(errno::ENOSPC);
        }
    }
    match caps.grant(object, ObjectType::File, cap_rights(access)) {
        Ok(handle) => {
            OPENS.fetch_add(1, Ordering::Release);
            Ok(encode_handle(handle))
        }
        Err(error) => {
            let _ = filesystem.close(open);
            Err(map_cap_error(error))
        }
    }
}

/// Opens a path for the calling process.
pub fn sys_open(pml4: PhysFrame, pointer: u64, length: u64, rights: u64) -> Option<i64> {
    let path = user_str(pml4, pointer, length)?;
    let access = access_from_rights(rights)?;
    let result = scheduler::with_current_caps(|root, caps| {
        let open = match with_fs(|filesystem| filesystem.open(root, path, access)) {
            Ok(open) => open,
            Err(error) => return Err(map_error(error)),
        };
        // SAFETY: A separate lookup keeps the filesystem borrow short; the
        // grant enforces the application's declared limit before the handle
        // escapes to user mode.
        with_fs(|filesystem| grant_file_open(root, caps, filesystem, open, access))
    });
    match result {
        Ok(handle) => Some(handle as i64),
        Err(error) => Some(error),
    }
}

/// Opens a file outside the process root through a portal grant.
pub fn sys_portal_open(pml4: PhysFrame, pointer: u64, length: u64, rights: u64) -> Option<i64> {
    let path = user_str(pml4, pointer, length)?;
    let access = access_from_rights(rights)?;
    let result = scheduler::with_current_caps(|root, caps| {
        // The verified manifest decides whether the application may use the
        // portal at all; only then is the exact-path grant consulted. Checking
        // permission first keeps the denial independent of whether a grant
        // happens to exist and avoids counting a grant use that never opens.
        portal_permission(root, rights)?;
        if crate::portal::check(root, path, rights).is_err() {
            return Err(errno::ENOGRANT);
        }
        let open = match with_fs(|filesystem| filesystem.open("/", path, access)) {
            Ok(open) => open,
            Err(error) => return Err(map_error(error)),
        };
        // SAFETY: The grant and permission checks passed under one interrupt
        // guard; the filesystem borrow ends before the capability grant.
        with_fs(|filesystem| grant_file_open(root, caps, filesystem, open, access))
    });
    match result {
        Ok(handle) => Some(handle as i64),
        Err(error) => Some(error),
    }
}

/// Reads from an open file.
pub fn sys_read(pml4: PhysFrame, handle: u64, buffer: u64, length: u64) -> Option<i64> {
    let cap = decode_handle(handle)?;
    let output = unsafe { user_bytes_mut(pml4, buffer, length)? };
    let result = scheduler::with_current_caps(|_root, caps| {
        let object = match caps.file(cap, Rights::READ) {
            Ok(object) => object,
            Err(error) => return Err(map_cap_error(error)),
        };
        let open = match open_id(object) {
            Ok(open) => open,
            Err(code) => return Err(code),
        };
        with_fs(|filesystem| filesystem.read(open, output))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or_else(|error| error))
}

/// Writes to an open file.
pub fn sys_write(pml4: PhysFrame, handle: u64, buffer: u64, length: u64) -> Option<i64> {
    let cap = decode_handle(handle)?;
    let input = user_bytes(pml4, buffer, length, false)?;
    let result = scheduler::with_current_caps(|_root, caps| {
        let object = match caps.file(cap, Rights::WRITE) {
            Ok(object) => object,
            Err(error) => return Err(map_cap_error(error)),
        };
        let open = match open_id(object) {
            Ok(open) => open,
            Err(code) => return Err(code),
        };
        with_fs(|filesystem| filesystem.write(open, input))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or_else(|error| error))
}

/// Converts a capability's object id into a filesystem open-file id.
///
/// File capabilities are minted only from open-file slots, but the conversion
/// is validated instead of truncated so a future minting path cannot silently
/// name a different slot.
fn open_id(object: ObjectId) -> Result<OpenId, i64> {
    match u8::try_from(object.index) {
        Ok(index) => Ok(OpenId {
            index,
            generation: object.generation,
        }),
        Err(_) => Err(errno::EBADF),
    }
}

/// Closes an open file and revokes its capability.
pub fn sys_close(handle: u64) -> Option<i64> {
    let cap = decode_handle(handle)?;
    let result = scheduler::with_current_caps(|_root, caps| {
        let object = match caps.file(cap, Rights::empty()) {
            Ok(object) => object,
            Err(error) => return Err(map_cap_error(error)),
        };
        let open = match open_id(object) {
            Ok(open) => open,
            Err(code) => return Err(code),
        };
        let _ = caps.remove(cap);
        with_fs(|filesystem| filesystem.close(open)).map_err(map_error)
    });
    Some(match result {
        Ok(()) => 0,
        Err(error) => error,
    })
}

/// Closes every file handle a task still holds when it is retired.
///
/// Called on task exit and fault. Open-file slots are a bounded global pool;
/// closing them prevents a completed or crashed task from permanently
/// shrinking the system's remaining capacity. Closing also releases the
/// filesystem's unlink prohibition, so the cleanup cannot corrupt another
/// task's work.
pub fn close_task_files(caps: &mut CapTable) -> usize {
    let (handles, count) = caps.file_handles();
    let mut closed = 0usize;
    for handle in handles.iter().take(count) {
        with_fs(|filesystem| {
            if let Ok(object) = caps.file(*handle, Rights::empty())
                && let Ok(open) = open_id(object)
            {
                let _ = caps.remove(*handle);
                let _ = filesystem.close(open);
                closed += 1;
            }
        });
    }
    closed
}

/// Lists a directory into a user buffer.
pub fn sys_list(
    pml4: PhysFrame,
    path_pointer: u64,
    path_length: u64,
    buffer: u64,
    length: u64,
) -> Option<i64> {
    // Copy the path before creating the mutable output slice. A caller can
    // pass overlapping path and output ranges; keeping both a borrowed user
    // path and a mutable user slice would violate Rust's aliasing rules even
    // though the kernel never dereferences them concurrently.
    let mut path_buffer = [0u8; 256];
    let path_bytes = {
        let path = user_str(pml4, path_pointer, path_length)?;
        let end = path.len();
        path_buffer.get_mut(..end)?.copy_from_slice(path.as_bytes());
        end
    };
    let path = core::str::from_utf8(&path_buffer[..path_bytes]).ok()?;
    let output = unsafe { user_bytes_mut(pml4, buffer, length)? };
    let result = scheduler::with_current_caps(|root, _caps| {
        with_fs(|filesystem| filesystem.list(root, path, output))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or_else(|error| error))
}

/// Creates a directory for the kernel console (no user pointer involved).
pub fn create_dir(path: &str) -> Result<(), FsError> {
    with_fs(|filesystem| filesystem.mkdir("/", path))
}

/// Lists a directory for the kernel console (no user pointer involved).
pub fn list_path(path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    with_fs(|filesystem| filesystem.list("/", path, out))
}

/// Reads a file for the kernel console.
pub fn read_path(path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    with_fs(|filesystem| filesystem.read_file("/", path, out))
}

/// Read-only Files service adapter. Application vaults are deliberately not a
/// browseable location; a window or a path supplied by UI is not a grant.
pub struct BrowserFiles;

fn browser_scope(path: &str) -> Result<(&'static str, &str), FsError> {
    for root in ["/home", "/system", "/portal"] {
        if let Some(relative) = path.strip_prefix(root)
            && (relative.is_empty() || relative.starts_with('/'))
        {
            return Ok((root, if relative.is_empty() { "/" } else { relative }));
        }
    }
    Err(FsError::Access)
}

impl aether_wm::browser::BrowserSource for BrowserFiles {
    fn list(
        &mut self,
        path: &str,
        out: &mut [u8],
    ) -> Result<usize, aether_wm::browser::BrowserError> {
        let result = if path == "/" {
            list_path(path, out) // directory names only; entering /apps is denied
        } else {
            browser_scope(path).and_then(|(root, relative)| {
                with_fs(|filesystem| filesystem.list(root, relative, out))
            })
        };
        result.map_err(browser_error)
    }

    fn read(
        &mut self,
        path: &str,
        out: &mut [u8],
    ) -> Result<usize, aether_wm::browser::BrowserError> {
        browser_scope(path)
            .and_then(|(root, relative)| {
                with_fs(|filesystem| filesystem.read_file(root, relative, out))
            })
            .map_err(browser_error)
    }
}

fn browser_error(error: FsError) -> aether_wm::browser::BrowserError {
    use aether_wm::browser::BrowserError;
    match error {
        FsError::Access | FsError::Escape => BrowserError::Access,
        FsError::PathTooLong | FsError::NameTooLong | FsError::Corrupt => BrowserError::InvalidData,
        _ => BrowserError::Unavailable,
    }
}

/// Replaces a file for the kernel console.
pub fn write_path(path: &str, data: &[u8]) -> Result<(), FsError> {
    with_fs(|filesystem| filesystem.write_file("/", path, data))
}

/// Deletes a file for the kernel console.
pub fn remove_path(path: &str) -> Result<(), FsError> {
    with_fs(|filesystem| filesystem.unlink("/", path))
}

/// Returns a file's size.
pub fn sys_stat(pml4: PhysFrame, path_pointer: u64, path_length: u64) -> Option<i64> {
    let path = user_str(pml4, path_pointer, path_length)?;
    let result = scheduler::with_current_caps(|root, _caps| {
        with_fs(|filesystem| filesystem.stat(root, path))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or_else(|error| error))
}

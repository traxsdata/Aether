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
use aether_ipc::{CapError, CapHandle, ObjectId, ObjectType, Rights};
use aether_memory::frame::PhysFrame;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

struct Global<T>(UnsafeCell<T>);

// SAFETY: Every access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

static FS: Global<AetherFs<RamDisk>> = Global(UnsafeCell::new(AetherFs::new(RamDisk::new())));

static OPENS: AtomicUsize = AtomicUsize::new(0);
static ESCAPE_DENIALS: AtomicUsize = AtomicUsize::new(0);
static ACCESS_DENIALS: AtomicUsize = AtomicUsize::new(0);

/// Root directory of a native application.
pub const DEMO_ROOT: &str = "/apps/demo";
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
        "apps/demo",
        "apps/demo/private",
        "apps/other",
        "apps/other/private",
        "system",
        "system/images",
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
    filesystem.write_file("/", "/home/welcome.txt", b"welcome to aether\n")?;
    filesystem.write_file("/", "/portal/README.txt", b"file portal grants\n")?;

    Ok(())
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
        CapError::InsufficientRights => {
            ACCESS_DENIALS.fetch_add(1, Ordering::Release);
            errno::EACCES
        }
        _ => errno::EBADF,
    }
}

fn access_from_rights(rights: u64) -> Option<Access> {
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
    let index = (value & 0xff) as u8;
    let generation = (value >> 8) as u16;
    Some(CapHandle { index, generation })
}

/// Opens a path for the calling process.
pub fn sys_open(pml4: PhysFrame, pointer: u64, length: u64, rights: u64) -> Option<i64> {
    let path = user_str(pml4, pointer, length)?;
    let access = access_from_rights(rights)?;
    match scheduler::with_current_caps(|root, caps| {
        let open = match with_fs(|filesystem| filesystem.open(root, path, access)) {
            Ok(open) => open,
            Err(error) => return Err(map_error(error)),
        };
        let object = ObjectId {
            index: u16::from(open.index),
            generation: open.generation,
        };
        match caps.grant(object, ObjectType::File, cap_rights(access)) {
            Ok(handle) => {
                OPENS.fetch_add(1, Ordering::Release);
                Ok(encode_handle(handle))
            }
            Err(error) => {
                let _ = with_fs(|filesystem| filesystem.close(open));
                Err(map_cap_error(error))
            }
        }
    }) {
        Ok(handle) => Some(handle as i64),
        Err(error) => Some(error),
    }
}

/// Opens a file outside the process root through a portal grant.
pub fn sys_portal_open(pml4: PhysFrame, pointer: u64, length: u64, rights: u64) -> Option<i64> {
    let path = user_str(pml4, pointer, length)?;
    let access = access_from_rights(rights)?;
    match scheduler::with_current_caps(|root, caps| {
        if crate::portal::check(root, path, rights).is_err() {
            return Err(errno::ENOGRANT);
        }
        let open = match with_fs(|filesystem| filesystem.open("/", path, access)) {
            Ok(open) => open,
            Err(error) => return Err(map_error(error)),
        };
        let object = ObjectId {
            index: u16::from(open.index),
            generation: open.generation,
        };
        match caps.grant(object, ObjectType::File, cap_rights(access)) {
            Ok(handle) => {
                OPENS.fetch_add(1, Ordering::Release);
                Ok(encode_handle(handle))
            }
            Err(error) => {
                let _ = with_fs(|filesystem| filesystem.close(open));
                Err(map_cap_error(error))
            }
        }
    }) {
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
        let open = OpenId {
            index: object.index as u8,
            generation: object.generation,
        };
        with_fs(|filesystem| filesystem.read(open, output))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or(errno::EBADF))
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
        let open = OpenId {
            index: object.index as u8,
            generation: object.generation,
        };
        with_fs(|filesystem| filesystem.write(open, input))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or(errno::EBADF))
}

/// Closes an open file and revokes its capability.
pub fn sys_close(handle: u64) -> Option<i64> {
    let cap = decode_handle(handle)?;
    let result = scheduler::with_current_caps(|_root, caps| {
        let object = match caps.file(cap, Rights::empty()) {
            Ok(object) => object,
            Err(error) => return Err(map_cap_error(error)),
        };
        let open = OpenId {
            index: object.index as u8,
            generation: object.generation,
        };
        let _ = caps.remove(cap);
        with_fs(|filesystem| filesystem.close(open)).map_err(map_error)
    });
    Some(match result {
        Ok(()) => 0,
        Err(error) => error,
    })
}

/// Lists a directory into a user buffer.
pub fn sys_list(
    pml4: PhysFrame,
    path_pointer: u64,
    path_length: u64,
    buffer: u64,
    length: u64,
) -> Option<i64> {
    let path = user_str(pml4, path_pointer, path_length)?;
    let output = unsafe { user_bytes_mut(pml4, buffer, length)? };
    let result = scheduler::with_current_caps(|root, _caps| {
        with_fs(|filesystem| filesystem.list(root, path, output))
            .map(|length| length as i64)
            .map_err(map_error)
    });
    Some(result.unwrap_or(errno::EBADF))
}

/// Lists a directory for the kernel console (no user pointer involved).
pub fn list_path(path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    with_fs(|filesystem| filesystem.list("/", path, out))
}

/// Reads a file for the kernel console.
pub fn read_path(path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    with_fs(|filesystem| filesystem.read_file("/", path, out))
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
    Some(result.unwrap_or(errno::EBADF))
}

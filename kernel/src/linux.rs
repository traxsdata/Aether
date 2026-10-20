// SPDX-License-Identifier: Apache-2.0
//! Linux runtime root: signed AppImage/SquashFS import at boot.
//!
//! The kernel embeds a small signed `linux-rootfs` SquashFS image as the boot
//! fixture for the Linux layer. Before the Linux task starts, the image's
//! Ed25519 record is checked against the test trust store, its digest is
//! compared, and the tree is imported with `aether-squashfs`. The resulting
//! read-only file table is what Linux `openat`/`read` see.
//!
//! This is not the final AppImage path: the image is embedded, the trust store
//! is a test fixture, and the import runs once at boot. It does prove signed
//! rootfs consumption and the same parser used by host import.

use crate::serial_line;
use aether_appimage::manifest;
use aether_packaging::signed::{TrustStore, TrustedPublisher};
use aether_squashfs::{Image, Limits};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

/// Maximum path bytes the Linux root accepts.
pub const MAX_PATH: usize = 256;
/// Maximum open files per Linux task.
pub const MAX_FDS: usize = 8;

const ROOTFS: &[u8] = include_bytes!(env!("AETHER_LINUX_ROOTFS"));
const ROOTFS_RECORD: &[u8] = include_bytes!(env!("AETHER_LINUX_ROOTFS_RECORD"));
const TRUST_PUBLIC_KEY: &str = include_str!("../../tests/fixtures/keys/test.pub");
const TRUST_PUBLISHER: &str = "Aether Test Publisher";

pub struct RootFile {
    pub path: String,
    pub data: Vec<u8>,
}

struct Root {
    files: Vec<RootFile>,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: The root is written once during single-CPU boot and is immutable
// afterwards. Readers run with interrupts masked or after the import.
unsafe impl<T> Sync for Global<T> {}

static ROOT: Global<Option<Root>> = Global(UnsafeCell::new(None));

/// Imports the embedded signed runtime rootfs.
///
/// Returns `(files, bytes)` on success. Diagnostics are printed by the caller.
pub fn init() -> Result<(usize, usize), ()> {
    let mut key = [0u8; 32];
    if aether_crypto::from_hex(TRUST_PUBLIC_KEY.trim().as_bytes(), &mut key) != Some(32) {
        serial_line!("linux: invalid built-in trust key");
        return Err(());
    }
    let publishers = [TrustedPublisher {
        name: TRUST_PUBLISHER,
        key,
    }];
    let trust = TrustStore::new(&publishers);
    if manifest::verify_image(ROOTFS_RECORD, &trust, ROOTFS).is_err() {
        serial_line!("linux: rootfs signature or digest verification failed");
        return Err(());
    }
    let image = Image::from_squashfs(ROOTFS).map_err(|error| {
        serial_line!("linux: rootfs parse failed: {error:?}");
    })?;
    let tree = image.import(Limits::default()).map_err(|error| {
        serial_line!("linux: rootfs import failed: {error:?}");
    })?;
    let mut files = Vec::new();
    let mut bytes = 0usize;
    for entry in tree.entries {
        if let aether_squashfs::TreeKind::File(data) = entry.kind {
            bytes = bytes.saturating_add(data.len());
            files.push(RootFile {
                path: entry.path,
                data,
            });
        }
    }
    if files.is_empty() {
        serial_line!("linux: rootfs contains no regular files");
        return Err(());
    }
    let count = files.len();
    // SAFETY: Boot is single-threaded and this is the only write to ROOT.
    unsafe { *ROOT.0.get() = Some(Root { files }) };
    Ok((count, bytes))
}

/// Returns the file index for an absolute, normalized path.
#[must_use]
pub fn lookup(path: &str) -> Option<usize> {
    if !valid_path(path) || path == "/" {
        return None;
    }
    let relative = path.trim_start_matches('/');
    // SAFETY: ROOT is immutable after boot; this runs on the single CPU.
    let root = unsafe { (*ROOT.0.get()).as_ref()? };
    root.files.iter().position(|file| file.path == relative)
}

/// Returns `true` for an absolute path with no empty, `.` or `..` components.
#[must_use]
pub fn valid_path(path: &str) -> bool {
    if !path.starts_with('/') || path.len() > MAX_PATH || path.contains('\0') {
        return false;
    }
    path.split('/')
        .skip(1)
        .all(|component| !component.is_empty() && component != "." && component != "..")
        && path != "/"
}

/// Returns the bytes of a regular file in the imported root.
///
/// The imported table is immutable after boot, so the slice is valid for the
/// rest of the boot. Directories and missing paths return `None`.
#[must_use]
pub fn file_data(path: &str) -> Option<&'static [u8]> {
    let index = lookup(path)?;
    file(index).map(|file| file.data.as_slice())
}

/// Returns `true` when `path` names a directory in the imported root.
///
/// The imported tree is flat, so a directory exists when at least one file
/// path continues below it.
#[must_use]
pub fn is_dir(path: &str) -> bool {
    let directory = normalize(path);
    let Some(root) = root() else {
        return false;
    };
    root.files.iter().any(|file| {
        file.path
            .strip_prefix(directory)
            .is_some_and(|rest| directory.is_empty() || rest.starts_with('/'))
            && !(directory.is_empty() && file.path.is_empty())
    })
}

/// Returns the `index`th entry of a directory.
///
/// Entry 0 is `.` and entry 1 is `..`; following entries are direct children
/// in import order with duplicates removed. The returned name borrows the
/// immutable imported tree, so it lives for the rest of the boot.
#[must_use]
pub fn dir_entry(directory: &str, index: usize) -> Option<(&'static str, u64, u8)> {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    let directory = normalize(directory);
    let root = root()?;
    match index {
        0 => return Some((".", 2, DT_DIR)),
        1 => return Some(("..", 2, DT_DIR)),
        _ => {}
    }
    let mut matched = 0usize;
    for (file_index, file) in root.files.iter().enumerate() {
        let (parent, name) = split_parent(&file.path);
        if parent != directory {
            continue;
        }
        let duplicate = root.files[..file_index].iter().any(|other| {
            let (other_parent, other_name) = split_parent(&other.path);
            other_parent == parent && other_name == name
        });
        if duplicate {
            continue;
        }
        matched += 1;
        if matched + 1 == index {
            let kind = if child_is_dir(root, parent, name) {
                DT_DIR
            } else {
                DT_REG
            };
            return Some((name, file_index as u64 + 1, kind));
        }
    }
    None
}

fn root() -> Option<&'static Root> {
    // SAFETY: ROOT is immutable after boot and lives for the program lifetime.
    unsafe { (*ROOT.0.get()).as_ref() }
}

fn normalize(path: &str) -> &str {
    path.trim_start_matches('/').trim_end_matches('/')
}

fn split_parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn child_is_dir(root: &Root, parent: &str, name: &str) -> bool {
    let mut prefix = [0u8; MAX_PATH + 2];
    let mut length = 0usize;
    if !parent.is_empty() {
        prefix[..parent.len()].copy_from_slice(parent.as_bytes());
        length = parent.len();
        prefix[length] = b'/';
        length += 1;
    }
    if length + name.len() + 1 > prefix.len() {
        return false;
    }
    prefix[length..length + name.len()].copy_from_slice(name.as_bytes());
    length += name.len();
    prefix[length] = b'/';
    length += 1;
    let prefix = core::str::from_utf8(&prefix[..length]).unwrap_or("");
    root.files.iter().any(|file| file.path.starts_with(prefix))
}

/// Returns file metadata by index.
#[must_use]
pub fn file(index: usize) -> Option<&'static RootFile> {
    // SAFETY: ROOT is immutable after boot and lives for the program lifetime.
    let root = unsafe { (*ROOT.0.get()).as_ref()? };
    root.files
        .get(index)
        .map(|file| unsafe { &*(file as *const RootFile) })
}

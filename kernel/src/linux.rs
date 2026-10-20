// SPDX-License-Identifier: Apache-2.0
//! Read-only Linux runtime root for the compatibility layer.
//!
//! The prototype has no persistent block device yet, so the runtime root is a
//! fixed, embedded set of files. Paths are validated before lookup: the layer
//! never resolves `..`, accepts a relative path, or exposes host files. This
//! is the seed of the read-only AppImage root, not the final filesystem
//! service.

/// Maximum path bytes the Linux root accepts.
pub const MAX_PATH: usize = 256;
/// Maximum open files per Linux task.
pub const MAX_FDS: usize = 8;

/// One immutable file in the runtime root.
pub struct File {
    /// Absolute path, already normalized.
    pub path: &'static str,
    /// File contents.
    pub data: &'static [u8],
}

/// Files available to the Linux boot fixture.
pub static FILES: &[File] = &[
    File {
        path: "/etc/motd",
        data: b"AETHER_LINUX_ROOT_OK source=/etc/motd\n",
    },
    File {
        path: "/etc/hostname",
        data: b"aether\n",
    },
];

/// Returns the file index for an absolute, normalized path.
#[must_use]
pub fn lookup(path: &str) -> Option<usize> {
    if !valid_path(path) {
        return None;
    }
    FILES.iter().position(|file| file.path == path)
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

/// Returns file metadata by index.
#[must_use]
pub fn file(index: usize) -> Option<&'static File> {
    FILES.get(index)
}

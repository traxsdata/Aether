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
const APPIMAGE: &[u8] = include_bytes!(env!("AETHER_LINUX_APPIMAGE"));
const APPIMAGE_RECORD: &[u8] = include_bytes!(env!("AETHER_LINUX_APPIMAGE_RECORD"));
const APPIMAGE_PROGRAM: &str = env!("AETHER_LINUX_APPIMAGE_PROGRAM");
const TRUST_PUBLIC_KEY: &str = include_str!("../../tests/fixtures/keys/test.pub");
const TRUST_PUBLISHER: &str = "Aether Test Publisher";
/// Build-time pin of the runtime manifest's SHA-256.
const RUNTIME_PIN: &str = env!("AETHER_LINUX_RUNTIME_PIN");

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

/// Selected Linux runtime profile from the packaged rootfs.
pub struct RuntimeProfile {
    /// Profile name, for example `pinned-host-glibc`.
    pub profile: String,
    /// Exact glibc version recorded by the package.
    pub glibc: String,
}

/// Imported user AppImage identity and entry program.
pub struct ImportedApp {
    /// Signed application identity.
    pub id: String,
    /// Signed application version.
    pub version: String,
    /// Program path selected by the launch policy.
    pub path: String,
    /// Program bytes copied out of the imported tree.
    program: Vec<u8>,
}

static RUNTIME: Global<Option<RuntimeProfile>> = Global(UnsafeCell::new(None));
static APP: Global<Option<ImportedApp>> = Global(UnsafeCell::new(None));

fn trust() -> Option<TrustStore<'static>> {
    let mut key = [0u8; 32];
    if aether_crypto::from_hex(TRUST_PUBLIC_KEY.trim().as_bytes(), &mut key) != Some(32) {
        return None;
    }
    // The publisher table is a fixed boot constant; leak it into static
    // storage so the borrowed TrustStore lives for the boot.
    let publishers: &'static [TrustedPublisher] = alloc::boxed::Box::leak(
        alloc::vec![TrustedPublisher {
            name: TRUST_PUBLISHER,
            key,
        }]
        .into_boxed_slice(),
    );
    Some(TrustStore::new(publishers))
}

/// Imports the embedded signed runtime rootfs.
///
/// Returns `(files, bytes)` on success. Diagnostics are printed by the caller.
pub fn init() -> Result<(usize, usize), ()> {
    let trust = trust().ok_or(())?;
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
    install_runtime_profile()?;
    Ok((count, bytes))
}

/// Verifies the packaged runtime manifest against the build-time pin.
///
/// A missing manifest means the opt-in host-glibc smoke rootfs was packaged
/// instead; it does not claim a pinned profile. When a manifest is present the
/// digest must equal the pin or the boot fails, so a changed library cannot be
/// silently accepted.
fn install_runtime_profile() -> Result<(), ()> {
    let Some(data) = file_data("/etc/aether-runtime.manifest") else {
        if !RUNTIME_PIN.trim().is_empty() {
            serial_line!("linux: runtime pin present but profile manifest is missing");
            return Err(());
        }
        return Ok(());
    };
    let digest = aether_crypto::sha256(data);
    let mut pin = [0u8; 32];
    if RUNTIME_PIN.trim().is_empty()
        || aether_crypto::from_hex(RUNTIME_PIN.trim().as_bytes(), &mut pin) != Some(32)
        || pin != digest
    {
        serial_line!("linux: runtime manifest does not match the pinned digest");
        return Err(());
    }
    let text = core::str::from_utf8(data).map_err(|_| ())?;
    let mut profile = None;
    let mut glibc = None;
    let mut mesa = None;
    let mut wayland = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "profile" => profile = Some(value),
            "glibc" => glibc = Some(value),
            "mesa" => mesa = Some(value),
            "wayland" => wayland = Some(value),
            _ => {}
        }
    }
    let profile = profile.ok_or(())?;
    let glibc = glibc.ok_or(())?;
    if profile.is_empty() || glibc.is_empty() {
        return Err(());
    }
    let mut hex = [0u8; 64];
    let length = aether_crypto::to_hex(&digest, &mut hex).ok_or(())?;
    let digest_hex = core::str::from_utf8(&hex[..length]).map_err(|_| ())?;
    serial_line!(
        "AETHER_RUNTIME_ROOT_OK profile={} glibc={} mesa={} wayland={} pin={}",
        profile,
        glibc,
        mesa.unwrap_or("unknown"),
        wayland.unwrap_or("unknown"),
        digest_hex
    );
    // SAFETY: Boot is single-threaded and this is the only write to RUNTIME.
    unsafe {
        *RUNTIME.0.get() = Some(RuntimeProfile {
            profile: String::from(profile),
            glibc: String::from(glibc),
        })
    };
    Ok(())
}

/// Returns the verified runtime profile selected from the packaged rootfs.
#[must_use]
pub fn runtime_profile() -> Option<&'static RuntimeProfile> {
    // SAFETY: RUNTIME is written once during boot and immutable afterwards.
    unsafe { (*RUNTIME.0.get()).as_ref() }
}

/// Verifies and imports the user-supplied AppImage fixture.
///
/// The identity, version, and digest come from the signed `linux-appimage`
/// record, not from kernel constants. The record and image digest are checked
/// before the container parser or SquashFS importer sees either one.
pub fn import_appimage() -> Result<(), ()> {
    let trust = trust().ok_or(())?;
    let record = manifest::verify_image(APPIMAGE_RECORD, &trust, APPIMAGE).map_err(|error| {
        serial_line!("linux: AppImage record rejected: {error:?}");
    })?;
    let info = aether_appimage::parse(APPIMAGE).map_err(|error| {
        serial_line!("linux: AppImage container rejected: {error:?}");
    })?;
    let start = info.squashfs_offset;
    let end = start
        .checked_add(usize::try_from(info.squashfs_bytes).map_err(|_| ())?)
        .ok_or(())?;
    let image = Image::from_squashfs(APPIMAGE.get(start..end).ok_or(())?).map_err(|error| {
        serial_line!("linux: AppImage SquashFS rejected: {error:?}");
    })?;
    let tree = image.import(Limits::default()).map_err(|error| {
        serial_line!("linux: AppImage import failed: {error:?}");
    })?;
    let program_path = APPIMAGE_PROGRAM.trim_start_matches('/');
    let mut program = None;
    for entry in tree.entries {
        if entry.path == program_path
            && let aether_squashfs::TreeKind::File(data) = entry.kind
        {
            program = Some(data);
            break;
        }
    }
    let program = program.ok_or_else(|| {
        serial_line!("linux: AppImage lacks the selected program");
    })?;
    // SAFETY: Boot is single-threaded and this is the only write to APP.
    unsafe {
        *APP.0.get() = Some(ImportedApp {
            id: String::from(record.id),
            version: String::from(record.version),
            path: String::from(APPIMAGE_PROGRAM),
            program,
        })
    };
    Ok(())
}

/// Returns the imported AppImage identity, version, and launch path.
#[must_use]
pub fn app_identity() -> Option<(&'static str, &'static str, &'static str)> {
    // SAFETY: APP is written once during boot and immutable afterwards.
    let app = unsafe { (*APP.0.get()).as_ref() }?;
    Some((app.id.as_str(), app.version.as_str(), app.path.as_str()))
}

/// Returns the program bytes selected from the imported AppImage.
#[must_use]
pub fn app_program() -> Option<&'static [u8]> {
    // SAFETY: APP is written once during boot and immutable afterwards.
    let app = unsafe { (*APP.0.get()).as_ref() }?;
    Some(app.program.as_slice())
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

// SPDX-License-Identifier: Apache-2.0
//! Linux runtime root: signed AppImage/SquashFS import at boot.
//!
//! The kernel embeds a small signed `linux-rootfs` SquashFS image as the boot
//! fixture for the Linux runtime. Before the Linux task starts, the image's
//! Ed25519 record is checked against the test trust store, its digest is
//! compared, and the tree is imported with `aether-squashfs`. The resulting
//! read-only file table is what Linux `openat`/`read` see.
//!
//! The user AppImage is different: the UEFI loader reads it from
//! `\linux\app.AppImage` together with `\linux\app.import`, retains the pages,
//! and reports their physical extents in `BootInfo`. The kernel verifies the
//! signed record and digest before either the container parser or the
//! SquashFS importer sees the bytes. The AppImage runtime is never executed.
//!
//! The trust store is still a test fixture and the import runs once at boot.

use crate::serial_line;
use aether_abi::BootArtifact;
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
const APPIMAGE_PROGRAM: &str = env!("AETHER_LINUX_APPIMAGE_PROGRAM");
/// Optional arguments passed to the selected AppImage program.
///
/// The value is a `|`-separated list compiled into the kernel, for example
/// `eval|=1+1`. It is empty by default so the static boot fixture keeps its
/// original argv (the program path only).
const APPIMAGE_ARGS: &str = match option_env!("AETHER_LINUX_APPIMAGE_ARGS") {
    Some(value) => value,
    None => "",
};
/// Maximum configured arguments; the loader allows 16 argv entries including
/// the program path that this list is prefixed with.
pub const MAX_APPIMAGE_ARGUMENTS: usize = 15;
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

/// Physical extents published by the loader for the staged AppImage.
struct StagedApp {
    appimage: BootArtifact,
    record: BootArtifact,
}

static STAGED: Global<Option<StagedApp>> = Global(UnsafeCell::new(None));

/// Physical extents published by the loader for the staged runtime rootfs.
struct StagedRootfs {
    image: BootArtifact,
    record: BootArtifact,
}

static STAGED_ROOTFS: Global<Option<StagedRootfs>> = Global(UnsafeCell::new(None));

/// Records the loader-staged runtime rootfs and its signed import record.
///
/// The Gate 4 Mesa/lavapipe rootfs is too large to embed in the kernel image.
/// Nothing here is trusted until [`init`] verifies the record and digest.
pub fn stage_runtime_rootfs(image: BootArtifact, record: BootArtifact) {
    // SAFETY: Boot is single-threaded and this is the only write to STAGED_ROOTFS.
    unsafe { *STAGED_ROOTFS.0.get() = Some(StagedRootfs { image, record }) };
}

/// Records the loader-staged AppImage and its signed import record.
///
/// Called once from the kernel entry point after `BootInfo` validation. The
/// distinct artifact type keeps these pages separate from the embedded runtime
/// rootfs; nothing here is trusted until [`import_appimage`] verifies the
/// record against the trust store.
pub fn stage_appimage(appimage: BootArtifact, record: BootArtifact) {
    // SAFETY: Boot is single-threaded and this is the only write to STAGED.
    unsafe { *STAGED.0.get() = Some(StagedApp { appimage, record }) };
}

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
    /// Whether the program requests a `PT_INTERP` interpreter.
    dynamic: bool,
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
    let (rootfs, record) = runtime_rootfs().ok_or_else(|| {
        serial_line!("linux: runtime rootfs is missing");
    })?;
    if manifest::verify_image(record, &trust, rootfs).is_err() {
        serial_line!("linux: rootfs signature or digest verification failed");
        return Err(());
    }
    let image = Image::from_squashfs(rootfs).map_err(|error| {
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
    serial_line!(
        "linux: runtime rootfs imported files={count} bytes={bytes} heap={}",
        crate::memory::heap_used_bytes()
    );
    install_runtime_profile()?;
    Ok((count, bytes))
}

/// Returns the loader-staged rootfs pair, or the embedded Gate 3 fixture.
///
/// A present staged descriptor is authoritative: a malformed extent or a
/// missing record fails closed instead of silently falling back to the small
/// embedded image.
fn runtime_rootfs() -> Option<(&'static [u8], &'static [u8])> {
    // SAFETY: STAGED_ROOTFS is written once during boot and immutable afterwards.
    if let Some(staged) = unsafe { (*STAGED_ROOTFS.0.get()).as_ref() } {
        if !staged.image.is_present() || !staged.record.is_present() {
            serial_line!("linux: staged runtime rootfs is incomplete");
            return None;
        }
        let image = staged_bytes(staged.image).inspect_err(|()| {
            serial_line!("linux: staged runtime rootfs extent is invalid");
        });
        let record = staged_bytes(staged.record).inspect_err(|()| {
            serial_line!("linux: staged runtime rootfs record extent is invalid");
        });
        return match (image, record) {
            (Ok(image), Ok(record)) => Some((image, record)),
            _ => None,
        };
    }
    Some((ROOTFS, ROOTFS_RECORD))
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
    let mut files = 0usize;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "profile" => profile = Some(value),
            "glibc" => glibc = Some(value),
            "mesa" => mesa = Some(value),
            "wayland" => wayland = Some(value),
            "file_sha256" => {
                verify_manifest_file(value)?;
                files += 1;
            }
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
        "AETHER_RUNTIME_ROOT_OK profile={} glibc={} mesa={} wayland={} pin={} files={}",
        profile,
        glibc,
        mesa.unwrap_or("unknown"),
        wayland.unwrap_or("unknown"),
        digest_hex,
        files
    );
    // SAFETY: Boot is single-threaded and this is the only write to RUNTIME.
    unsafe {
        *RUNTIME.0.get() = Some(RuntimeProfile {
            profile: String::from(profile),
            glibc: String::from(glibc),
        })
    };
    if profile.starts_with("gate4-") {
        serial_line!(
            "AETHER_GATE4_RUNTIME_OK profile={} mesa={} wayland={} files={}",
            profile,
            mesa.unwrap_or("unknown"),
            wayland.unwrap_or("unknown"),
            files
        );
    }
    Ok(())
}

/// Verifies one `file_sha256=<absolute path>:<hex>` manifest entry.
///
/// The manifest digest is pinned at build time, so these entries bind every
/// packaged file the runtime depends on. A missing file or a mismatched digest
/// fails the boot instead of importing a partially verified tree.
fn verify_manifest_file(value: &str) -> Result<(), ()> {
    let (path, expected) = value.split_once(':').ok_or(())?;
    let data = file_data(path).ok_or_else(|| {
        serial_line!("linux: runtime manifest names a missing file: {path}");
    })?;
    let mut expected_digest = [0u8; 32];
    if aether_crypto::from_hex(expected.as_bytes(), &mut expected_digest) != Some(32) {
        serial_line!("linux: runtime manifest has a malformed file digest: {path}");
        return Err(());
    }
    if aether_crypto::sha256(data) != expected_digest {
        serial_line!("linux: runtime manifest file digest mismatch: {path}");
        return Err(());
    }
    Ok(())
}

/// Returns the verified runtime profile selected from the packaged rootfs.
#[must_use]
pub fn runtime_profile() -> Option<&'static RuntimeProfile> {
    // SAFETY: RUNTIME is written once during boot and immutable afterwards.
    unsafe { (*RUNTIME.0.get()).as_ref() }
}

/// Verifies and imports the loader-staged user AppImage.
///
/// The identity, version, and digest come from the signed `linux-appimage`
/// record on the boot volume, not from kernel constants. The loader reported
/// only a physical extent; the record and image digest are checked before the
/// container parser or SquashFS importer sees either one. A missing or
/// tampered artifact fails closed before any parsing or launch.
pub fn import_appimage() -> Result<(), ()> {
    // SAFETY: STAGED is written once during boot and immutable afterwards.
    let Some(staged) = (unsafe { (*STAGED.0.get()).as_ref() }) else {
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=missing");
        return Err(());
    };
    if !staged.appimage.is_present() || !staged.record.is_present() {
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=missing");
        return Err(());
    }
    let appimage = staged_bytes(staged.appimage).map_err(|()| {
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=bounds");
    })?;
    let record_bytes = staged_bytes(staged.record).map_err(|()| {
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=record-bounds");
    })?;
    let trust = trust().ok_or_else(|| {
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=trust");
    })?;
    let record = manifest::verify_image(record_bytes, &trust, appimage).map_err(|error| {
        serial_line!("linux: AppImage record rejected: {error:?}");
        serial_line!("AETHER_LINUX_APPIMAGE_STAGE_FAILED reason=verify");
    })?;
    serial_line!(
        "AETHER_LINUX_APPIMAGE_STAGED_OK source=loader id={} version={} bytes={}",
        record.id,
        record.version,
        appimage.len()
    );
    let info = aether_appimage::parse(appimage).map_err(|error| {
        serial_line!("linux: AppImage container rejected: {error:?}");
    })?;
    let start = info.squashfs_offset;
    let end = start
        .checked_add(usize::try_from(info.squashfs_bytes).map_err(|_| ())?)
        .ok_or(())?;
    let image = Image::from_squashfs(appimage.get(start..end).ok_or(())?).map_err(|error| {
        serial_line!("linux: AppImage SquashFS rejected: {error:?}");
    })?;
    let tree = image.import(Limits::default()).map_err(|error| {
        serial_line!("linux: AppImage import failed: {error:?}");
    })?;
    serial_line!(
        "linux: AppImage imported heap={}",
        crate::memory::heap_used_bytes()
    );
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
    let dynamic = aether_linux_elf::parse(&program)
        .map(|image| image.interpreter().is_some())
        .unwrap_or(false);
    // SAFETY: Boot is single-threaded and this is the only write to APP.
    unsafe {
        *APP.0.get() = Some(ImportedApp {
            id: String::from(record.id),
            version: String::from(record.version),
            path: String::from(APPIMAGE_PROGRAM),
            program,
            dynamic,
        })
    };
    Ok(())
}

/// Borrows a loader-staged artifact as a byte slice.
///
/// `BootInfo::validate` bounds the address and length before this is called.
/// The loader allocated the pages below 4 GiB and retained them across
/// `ExitBootServices`; the kernel identity map covers the full extent.
fn staged_bytes(artifact: BootArtifact) -> Result<&'static [u8], ()> {
    let length = usize::try_from(artifact.length).map_err(|_| ())?;
    if !artifact.is_present() || length == 0 {
        return Err(());
    }
    // SAFETY: The ABI validation bounded `address + length` below 2^52, and
    // the loader-owned pages remain mapped and readable for the whole boot.
    Ok(unsafe { core::slice::from_raw_parts(artifact.address as *const u8, length) })
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

/// Returns `true` when the imported program requests an interpreter.
///
/// A dynamically linked application does not own the runtime-configuration
/// files the static fixture writes, so the post-exit overlay self-test is only
/// meaningful for the static fixture.
#[must_use]
pub fn app_is_dynamic() -> bool {
    // SAFETY: APP is written once during boot and immutable afterwards.
    unsafe { (*APP.0.get()).as_ref() }.is_some_and(|app| app.dynamic)
}

/// Returns the configured arguments for the imported program.
///
/// `None` means the compile-time list exceeds the loader's bounded argv array;
/// the caller must reject the launch rather than silently truncate it.
#[must_use]
pub fn app_arguments() -> Option<([&'static str; MAX_APPIMAGE_ARGUMENTS], usize)> {
    let mut arguments = [""; MAX_APPIMAGE_ARGUMENTS];
    let mut count = 0usize;
    for argument in APPIMAGE_ARGS.split('|').filter(|value| !value.is_empty()) {
        if count == MAX_APPIMAGE_ARGUMENTS {
            return None;
        }
        arguments[count] = argument;
        count += 1;
    }
    Some((arguments, count))
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

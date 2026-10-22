// SPDX-License-Identifier: Apache-2.0
//! Package installation and verification self-test.
//!
//! The kernel verifies an embedded, signed package with the same crate the
//! release tools use, then installs its files into a root derived from the
//! application identity. An untrusted publisher, a modified payload, and a
//! broken signature must all be rejected before any file is written.
//!
//! The package fixtures are signed with the test keys under
//! `tests/fixtures/keys`. They exist to exercise the verifier, not to
//! represent a release trust root.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::fs;
use crate::serial_line;
use aether_crypto::from_hex;
use aether_packaging::manifest::{Channel, Limits, Permissions, Version};
use aether_packaging::package::VerifiedPackage;
use aether_packaging::signed::{TrustStore, TrustedPublisher};
use aether_packaging::{PackageError, verify_package};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU and
// the values are written once during boot.
unsafe impl<T> Sync for Global<T> {}

const PUBLISHER: &str = "Aether Test Publisher";
const MAX_ROOT: usize = 64;

static DEMO_PACKAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/packages/demo.aether"
));
static UNTRUSTED_PACKAGE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tests/fixtures/packages/untrusted.aether"
));
/// The kernel's trust root, chosen at build time (see `kernel/build.rs`).
static TRUSTED_KEY: &str = env!("AETHER_TRUST_PUBLIC_KEY");

static TRUST: Global<[TrustedPublisher<'static>; 1]> =
    Global(UnsafeCell::new([TrustedPublisher {
        name: PUBLISHER,
        key: [0; 32],
    }]));
static ROOT: Global<[u8; MAX_ROOT]> = Global(UnsafeCell::new([0; MAX_ROOT]));
static INSTALLED: Global<Option<Installed>> = Global(UnsafeCell::new(None));

/// Metadata for the installed package.
#[derive(Clone, Copy)]
pub struct Installed {
    /// Application identity.
    pub id: &'static str,
    /// Application version.
    pub version: Version,
    /// Publisher display name.
    pub publisher: &'static str,
    /// Private storage root.
    pub root: &'static str,
    /// Number of installed files.
    pub files: usize,
    /// Declared permissions.
    pub permissions: Permissions,
    /// Declared limits.
    pub limits: Limits,
    /// Update channel.
    pub channel: Channel,
}

/// Installs the embedded package and proves the negative paths.
pub fn init() -> Result<(), &'static str> {
    let _guard = InterruptGuard::enter();

    // SAFETY: Interrupts are masked and this runs once during boot. The trust
    // entry is static storage owned by this module.
    let trust = unsafe { &mut *TRUST.0.get() };
    if env!("AETHER_TRUST_IS_TEST_KEY") == "1" {
        // The checked-in test seed is public: anyone can sign for this key.
        serial_line!("AETHER_TRUST_WARNING root=test-key release=unsafe");
    }
    if from_hex(TRUSTED_KEY.trim().as_bytes(), &mut trust[0].key) != Some(32) {
        return Err("embedded publisher key is not 32-byte hex");
    }
    let store = TrustStore::new(trust);

    let verified = verify_package(DEMO_PACKAGE, &store).map_err(|error| {
        serial_line!("package: valid fixture rejected: {error:?}");
        "valid package fixture rejected"
    })?;
    if verified.manifest.publisher != PUBLISHER {
        return Err("package fixture publisher mismatch");
    }

    let mut rejected = 0usize;
    match verify_package(UNTRUSTED_PACKAGE, &store) {
        Err(PackageError::UnknownPublisher) => rejected += 1,
        other => {
            serial_line!("package: untrusted fixture outcome: {other:?}");
            return Err("untrusted package was not rejected as an unknown publisher");
        }
    }

    let mut tampered = [0u8; 1024];
    if DEMO_PACKAGE.len() > tampered.len() {
        return Err("package fixture exceeds the self-test buffer");
    }
    tampered[..DEMO_PACKAGE.len()].copy_from_slice(DEMO_PACKAGE);
    // The last payload byte precedes the 64-byte signature.
    let payload_index = DEMO_PACKAGE
        .len()
        .checked_sub(65)
        .ok_or("package fixture is too short")?;
    tampered[payload_index] ^= 0x01;
    match verify_package(&tampered[..DEMO_PACKAGE.len()], &store) {
        Err(PackageError::DigestMismatch) => rejected += 1,
        other => {
            serial_line!("package: tampered fixture outcome: {other:?}");
            return Err("tampered package was not rejected as a digest mismatch");
        }
    }

    let mut bad_signature = [0u8; 1024];
    bad_signature[..DEMO_PACKAGE.len()].copy_from_slice(DEMO_PACKAGE);
    let signature_start = DEMO_PACKAGE.len() - 64;
    bad_signature[signature_start..DEMO_PACKAGE.len()].fill(0);
    match verify_package(&bad_signature[..DEMO_PACKAGE.len()], &store) {
        Err(PackageError::BadSignature) => rejected += 1,
        other => {
            serial_line!("package: zeroed-signature outcome: {other:?}");
            return Err("package with a zeroed signature was not rejected");
        }
    }

    let root = install_root(verified.manifest.id)?;
    install(&verified, root)?;

    // SAFETY: Interrupts are masked and this runs once during boot.
    unsafe {
        *INSTALLED.0.get() = Some(Installed {
            id: verified.manifest.id,
            version: verified.manifest.version,
            publisher: verified.manifest.publisher,
            root,
            files: verified.payload.len(),
            permissions: verified.manifest.permissions,
            limits: verified.manifest.limits,
            channel: verified.manifest.channel,
        });
    }

    let (version, version_length) = version_text(verified.manifest.version);
    serial_line!(
        "package: installed {} {} at {}",
        verified.manifest.id,
        core::str::from_utf8(&version[..version_length]).unwrap_or("?"),
        root
    );
    serial_line!(
        "AETHER_PACKAGE_OK installed=1 rejected={rejected} files={}",
        verified.payload.len()
    );
    Ok(())
}

/// Returns the installed package metadata.
#[must_use]
pub fn installed() -> Option<Installed> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and the value was written during boot.
    unsafe { *INSTALLED.0.get() }
}

fn install_root(id: &str) -> Result<&'static str, &'static str> {
    // SAFETY: Interrupts are masked by `init` and this buffer is written once.
    let buffer = unsafe { &mut *ROOT.0.get() };
    let prefix = b"/apps/";
    let length = prefix
        .len()
        .checked_add(id.len())
        .ok_or("package root is too long")?;
    if length > MAX_ROOT {
        return Err("package root is too long");
    }
    buffer[..prefix.len()].copy_from_slice(prefix);
    buffer[prefix.len()..length].copy_from_slice(id.as_bytes());
    core::str::from_utf8(&buffer[..length]).map_err(|_| "package root is not UTF-8")
}

fn install(package: &VerifiedPackage<'_>, root: &str) -> Result<(), &'static str> {
    fs::create_dir(root).map_err(|error| {
        serial_line!("package: cannot create {root}: {error:?}");
        "cannot create the package root"
    })?;
    for file in package.files() {
        let mut path = [0u8; 128];
        let mut length = root.len();
        path[..length].copy_from_slice(root.as_bytes());
        path[length] = b'/';
        length += 1;
        let end = length + file.name.len();
        if end > path.len() {
            return Err("package file path is too long");
        }
        path[length..end].copy_from_slice(file.name.as_bytes());
        let path =
            core::str::from_utf8(&path[..end]).map_err(|_| "package file path is not UTF-8")?;
        fs::write_path(path, file.data).map_err(|error| match error {
            aether_fs::FsError::FileTooLarge => "package file exceeds the filesystem limit",
            _ => "cannot write a package file",
        })?;
    }
    Ok(())
}

fn version_text(version: Version) -> ([u8; 24], usize) {
    let mut buffer = [0u8; 24];
    let mut written = 0usize;
    let mut digits = [0u8; 5];
    for (index, part) in [version.major, version.minor, version.patch]
        .into_iter()
        .enumerate()
    {
        if index != 0 {
            buffer[written] = b'.';
            written += 1;
        }
        let mut value = part;
        let mut count = 0usize;
        loop {
            digits[count] = b'0' + (value % 10) as u8;
            value /= 10;
            count += 1;
            if value == 0 {
                break;
            }
        }
        for offset in 0..count {
            buffer[written] = digits[count - 1 - offset];
            written += 1;
        }
    }
    (buffer, written)
}

/// Prints the package status for the shell.
pub fn print_status() {
    let installed = installed();
    serial_line!("packages: installed={}", usize::from(installed.is_some()));
    if let Some(installed) = installed {
        let (version, version_length) = version_text(installed.version);
        serial_line!(
            "package {} version={} channel={} publisher=\"{}\"",
            installed.id,
            core::str::from_utf8(&version[..version_length]).unwrap_or("?"),
            installed.channel.name(),
            installed.publisher
        );
        serial_line!(
            "package root={} files={} permissions={:#x} limits={}KiB/{}files/{}task",
            installed.root,
            installed.files,
            installed.permissions.bits(),
            installed.limits.memory_kib,
            installed.limits.open_files,
            installed.limits.tasks
        );
    }
}

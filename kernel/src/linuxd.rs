// SPDX-License-Identifier: Apache-2.0
//! Bounded `linuxd` runtime manager prototype.
//!
//! This module is the service boundary for Linux runtime management: it owns
//! the selected runtime profile, the imported AppImage identity, and the
//! launch policy. It is a kernel-side supervisor service in this slice; the
//! user-space process and the syscall-remoting protocol are not implemented
//! yet. The kernel still dispatches Linux syscalls directly, and that work is
//! recorded in the compatibility documents rather than implied here.
//!
//! The service never trusts an application-provided string. Identity, version,
//! and digest come from the signed import record; the runtime profile comes
//! from the packaged, digest-pinned manifest. A missing profile or a failed
//! import fails the boot instead of launching with ambient authority.

use crate::serial_line;

/// Overlay store slot owned by the imported AppImage domain.
pub const APP_OVERLAY_ID: u8 = 1;

/// Runtime manager failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxdError {
    /// The packaged runtime profile is missing or unverified.
    NoRuntimeProfile,
    /// The signed AppImage record or image failed verification.
    ImportFailed,
    /// The signed identity or version is not acceptable.
    Policy,
    /// The selected launch path is not an absolute normalized path.
    LaunchPath,
}

/// Imports the user AppImage and applies the launch policy.
pub fn init() -> Result<(), LinuxdError> {
    let Some(profile) = crate::linux::runtime_profile() else {
        serial_line!("linuxd: no verified runtime profile");
        return Err(LinuxdError::NoRuntimeProfile);
    };
    crate::linux::import_appimage().map_err(|_| LinuxdError::ImportFailed)?;
    let Some((id, version, path)) = crate::linux::app_identity() else {
        return Err(LinuxdError::ImportFailed);
    };
    if !aether_packaging::manifest::is_valid_identity(id)
        || id == "org.aether.linux-runtime"
        || version.is_empty()
        || version.len() > 32
    {
        serial_line!("linuxd: AppImage identity rejected");
        return Err(LinuxdError::Policy);
    }
    if !path.starts_with('/')
        || path.len() > 128
        || path.split('/').skip(1).any(|part| part.is_empty())
    {
        serial_line!("linuxd: AppImage launch path rejected");
        return Err(LinuxdError::LaunchPath);
    }
    serial_line!(
        "AETHER_LINUXD_OK profile={} glibc={} apps=1 id={} version={} path={}",
        profile.profile,
        profile.glibc,
        id,
        version,
        path
    );
    Ok(())
}

/// Checks overlay persistence after the imported application task has exited.
///
/// The check runs only after the task is `Done`, so it can observe what the
/// application wrote. It then exercises the same erasure entry point the
/// `linux-erase` shell command uses. Nothing here survives a power cycle: the
/// filesystem service is a RAM disk and there is no persistent block device.
pub fn post_app_exit() {
    let (entries, bytes) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    if entries == 0 {
        serial_line!("AETHER_BOOT_FAILED reason=linuxd overlay did not persist");
        return;
    }
    serial_line!(
        "AETHER_LINUX_OVERLAY_PERSIST_OK app={} entries={} bytes={}",
        crate::linux::app_identity().map_or("unknown", |(id, _, _)| id),
        entries,
        bytes
    );
    let erased = crate::scheduler::linux_overlay_erase(APP_OVERLAY_ID);
    let (remaining, _) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    if erased != entries || remaining != 0 {
        serial_line!("AETHER_BOOT_FAILED reason=linuxd overlay erase");
        return;
    }
    serial_line!("AETHER_LINUX_OVERLAY_ERASE_OK app={APP_OVERLAY_ID} erased={erased}");
}

/// Prints the runtime manager status for the shell and security view.
pub fn print_status() {
    let profile = crate::linux::runtime_profile();
    let app = crate::linux::app_identity();
    let (entries, bytes) = crate::scheduler::linux_overlay_stats(APP_OVERLAY_ID);
    serial_line!(
        "linuxd: profile={} glibc={} app={} version={} path={} overlay_entries={entries} overlay_bytes={bytes}",
        profile.map_or("none", |profile| profile.profile.as_str()),
        profile.map_or("none", |profile| profile.glibc.as_str()),
        app.map_or("none", |(id, _, _)| id),
        app.map_or("none", |(_, version, _)| version),
        app.map_or("none", |(_, _, path)| path)
    );
}

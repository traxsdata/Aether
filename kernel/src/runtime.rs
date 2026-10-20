// SPDX-License-Identifier: Apache-2.0
//! Runtime isolation for packaged applications.
//!
//! The profile is derived from the verified package manifest; an application
//! cannot choose a shared prefix, request undeclared permissions, or raise its
//! own limits. The boot self-test exercises containment, permission, limit,
//! and prefix-sharing decisions so a regression fails the boot test.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_packaging::manifest::Permissions;
use aether_services::runtime::{RuntimeError, RuntimeProfile, RuntimeTable, isolated};
use core::cell::UnsafeCell;

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU and
// the table is written once during boot.
unsafe impl<T> Sync for Global<T> {}

const MAX_PROFILES: usize = 8;

static PROFILES: Global<RuntimeTable<'static, MAX_PROFILES>> =
    Global(UnsafeCell::new(RuntimeTable::new()));

/// Registers the installed application and checks the isolation policy.
pub fn init() -> Result<(), RuntimeError> {
    let _guard = InterruptGuard::enter();
    let Some(app) = crate::packaging::installed() else {
        serial_line!("AETHER_RUNTIME_OK apps=0 isolated=0");
        return Ok(());
    };

    let profile = isolated(app.id, app.root, app.permissions, app.limits)?;
    // SAFETY: Interrupts are masked and this runs once during boot.
    let table = unsafe { &mut *PROFILES.0.get() };
    table.register(profile)?;

    // A second application cannot register the same prefix.
    let duplicate = isolated(app.id, app.root, app.permissions, app.limits)?;
    if table.register(duplicate) != Err(RuntimeError::PrefixInUse) {
        return Err(RuntimeError::PrefixInUse);
    }

    let mut checks = 0usize;
    if profile.check_relative_path("../other/private/secret.txt") != Err(RuntimeError::PrefixEscape)
    {
        return Err(RuntimeError::PrefixEscape);
    }
    checks += 1;
    // The fixture package declares the portal contract (and not input access),
    // and the file service enforces that declaration before any portal grant
    // is honored.
    if !profile.allows(Permissions::PORTAL_READ) || profile.allows(Permissions::INPUT_READ) {
        return Err(RuntimeError::NotPermitted);
    }
    checks += 1;
    if profile.allows_open_files(app.limits.open_files.saturating_add(1))
        || !profile.allows_open_files(app.limits.open_files)
    {
        return Err(RuntimeError::LimitExceeded);
    }
    checks += 1;

    // Using another application's prefix requires explicit consent.
    let mut shared = profile;
    let other = isolated(
        "org.aether.other",
        "/apps/org.aether.other",
        Permissions::parse_list("file.read").map_err(|_| RuntimeError::ManifestInvalid)?,
        app.limits,
    )?;
    if shared.adopt_prefix(&other, false) != Err(RuntimeError::SharingNotAuthorized) {
        return Err(RuntimeError::SharingNotAuthorized);
    }
    checks += 1;

    serial_line!(
        "AETHER_RUNTIME_OK apps={} isolated=1 prefix_checks={checks} shared=0",
        table.len()
    );
    Ok(())
}

/// Prints the runtime profile table for the shell.
pub fn print_status() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let table = unsafe { &*PROFILES.0.get() };
    serial_line!("runtime: profiles={}", table.len());
    for profile in table.profiles() {
        serial_line!(
            "runtime {} prefix={} permissions={:#x} limits={}KiB/{}files/{}task shared={}",
            profile.id,
            profile.prefix,
            profile.permissions.bits(),
            profile.limits.memory_kib,
            profile.limits.open_files,
            profile.limits.tasks,
            profile.shared_prefix
        );
    }
}

/// Returns the registered runtime profile for a private application root.
///
/// The file service matches the kernel-owned task root against the derived
/// prefixes in this table. A task cannot choose the profile that gates it;
/// roots without a registered profile have no declared limits.
#[must_use]
pub fn profile_for(root: &str) -> Option<RuntimeProfile<'static>> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let table = unsafe { &*PROFILES.0.get() };
    table.find_by_prefix(root)
}

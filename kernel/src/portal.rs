// SPDX-License-Identifier: Apache-2.0
//! File portal.
//!
//! The portal is the only path by which a process can obtain a capability for
//! a file outside its private root. A grant is scoped to a specific process
//! root, path, and right set. There is no directory or pattern grant, so a
//! selected file never implies access to its parent or siblings.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_abi::user::FileRights;
use core::cell::UnsafeCell;
use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicUsize, Ordering};

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

const MAX_GRANTS: usize = 8;

static GRANTED: AtomicUsize = AtomicUsize::new(0);
static DENIED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Grant {
    root: &'static str,
    path: &'static str,
    rights: u64,
    revoked: bool,
}

impl Grant {
    const EMPTY: Self = Self {
        root: "",
        path: "",
        rights: 0,
        revoked: false,
    };
}

static GRANTS: Global<[Grant; MAX_GRANTS]> = Global(UnsafeCell::new([Grant::EMPTY; MAX_GRANTS]));

/// Portal failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalError {
    /// No grant matches the caller, path, and requested rights.
    NotGranted,
}

/// Seeds the boot-time portal grants.
pub fn init() {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this runs once during boot.
    let grants = unsafe { &mut *GRANTS.0.get() };
    grants[0] = Grant {
        root: crate::fs::DEMO_ROOT,
        path: "/home/welcome.txt",
        rights: FileRights::READ,
        revoked: false,
    };
}

/// Checks whether `root` may open `path` with `rights` through the portal.
///
/// Grant and denial outcomes are counted here so the Security Center and the
/// shell can show the portal's actual access history instead of its declared
/// policy.
pub fn check(root: &str, path: &str, rights: u64) -> Result<(), PortalError> {
    let outcome = check_grant(root, path, rights);
    if outcome.is_ok() {
        GRANTED.fetch_add(1, Ordering::Release);
    } else {
        DENIED.fetch_add(1, Ordering::Release);
    }
    outcome
}

fn check_grant(root: &str, path: &str, rights: u64) -> Result<(), PortalError> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    for grant in grants {
        if grant.revoked || grant.root.is_empty() {
            continue;
        }
        if grant.root == root
            && grant.path == path
            && (grant.rights & rights) == rights
            && rights != 0
        {
            return Ok(());
        }
    }
    Err(PortalError::NotGranted)
}

/// Revokes the grant matching `root` and `path`.
///
/// Revocation is checked when a new portal open is attempted. A file
/// capability that was already issued remains valid until it is closed:
/// revoking a grant cannot undo bytes the application has already read, and
/// the first implementation chooses not to invalidate an in-flight handle.
pub fn revoke(root: &str, path: &str) -> Result<(), PortalError> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this is the only writer.
    let grants = unsafe { &mut *GRANTS.0.get() };
    for grant in grants {
        if !grant.revoked && !grant.root.is_empty() && grant.root == root && grant.path == path {
            grant.revoked = true;
            audit_revoke(root, path);
            return Ok(());
        }
    }
    Err(PortalError::NotGranted)
}

/// Revokes a grant by its displayed table index.
pub fn revoke_index(index: usize) -> Result<(), PortalError> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked and this is the only writer.
    let grants = unsafe { &mut *GRANTS.0.get() };
    let Some(grant) = grants.get_mut(index) else {
        return Err(PortalError::NotGranted);
    };
    if grant.revoked || grant.root.is_empty() {
        return Err(PortalError::NotGranted);
    }
    grant.revoked = true;
    audit_revoke(grant.root, grant.path);
    Ok(())
}

/// Emits the revocation audit line for the console log.
///
/// The serial console is this prototype's audit surface; the Security Center
/// derives its state from the same counters and table.
fn audit_revoke(root: &str, path: &str) {
    serial_line!("portal: audit revoked root={root} path={path}");
}

/// Returns the number of portal checks that were granted.
#[must_use]
pub fn grant_uses() -> usize {
    GRANTED.load(Ordering::Acquire)
}

/// Returns the number of portal open attempts that were denied.
#[must_use]
pub fn denials() -> usize {
    DENIED.load(Ordering::Acquire)
}

/// Exercises grant creation, exact matching, and revocation at boot.
///
/// The temporary grant uses a real portal path and is removed afterwards, so
/// the application's seeded grant is unaffected. Returns the number of checks
/// performed.
pub fn self_test() -> Result<usize, PortalError> {
    const PATH: &str = "/portal/README.txt";
    let index = MAX_GRANTS - 1;
    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and the slot is empty at boot.
        let grants = unsafe { &mut *GRANTS.0.get() };
        grants[index] = Grant {
            root: crate::fs::DEMO_ROOT,
            path: PATH,
            rights: FileRights::READ,
            revoked: false,
        };
    }

    let result = (|| {
        let mut checks = 0usize;
        if check(crate::fs::DEMO_ROOT, PATH, FileRights::READ).is_err() {
            return Err(PortalError::NotGranted);
        }
        checks += 1;
        if check(crate::fs::DEMO_ROOT, PATH, FileRights::WRITE).is_ok() {
            return Err(PortalError::NotGranted);
        }
        checks += 1;
        revoke(crate::fs::DEMO_ROOT, PATH)?;
        checks += 1;
        if check(crate::fs::DEMO_ROOT, PATH, FileRights::READ).is_ok() {
            return Err(PortalError::NotGranted);
        }
        checks += 1;
        Ok(checks)
    })();

    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this is the temporary slot.
        let grants = unsafe { &mut *GRANTS.0.get() };
        grants[index] = Grant::EMPTY;
    }
    result
}

/// Returns the number of active grants.
#[must_use]
pub fn grant_count() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    grants
        .iter()
        .filter(|grant| !grant.revoked && !grant.root.is_empty())
        .count()
}

/// Prints the grant table for the shell.
pub fn print_grants() {
    serial_line!("portal: grants={}", grant_count());
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    for (index, grant) in grants.iter().enumerate() {
        if grant.root.is_empty() {
            continue;
        }
        serial_line!(
            "portal: grant {} root={} path={} rights={:#x} revoked={}",
            index,
            grant.root,
            grant.path,
            grant.rights,
            grant.revoked
        );
    }
}

struct SummaryWriter<'a> {
    out: &'a mut [u8],
    written: usize,
}

impl fmt::Write for SummaryWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.written.checked_add(text.len()).ok_or(fmt::Error)?;
        let target = self.out.get_mut(self.written..end).ok_or(fmt::Error)?;
        target.copy_from_slice(text.as_bytes());
        self.written = end;
        Ok(())
    }
}

fn rights_name(rights: u64) -> &'static str {
    match (
        rights & FileRights::READ != 0,
        rights & FileRights::WRITE != 0,
    ) {
        (true, true) => "READ|WRITE",
        (true, false) => "READ",
        (false, true) => "WRITE",
        (false, false) => "NONE",
    }
}

/// Writes the portal state for the desktop.
///
/// The display is derived from the live grant table, not from a hard-coded
/// description, so a revoked grant disappears from the Security Center view.
pub fn summary(out: &mut [u8]) -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    let mut writer = SummaryWriter { out, written: 0 };
    let mut active = 0usize;
    for grant in grants {
        if grant.revoked || grant.root.is_empty() {
            continue;
        }
        active += 1;
        let _ = writeln!(
            writer,
            "ROOT: {}\nPATH: {}\nACCESS: {}\nSCOPE: SINGLE FILE\nREVOCABLE: YES\n",
            grant.root,
            grant.path,
            rights_name(grant.rights)
        );
    }
    if active == 0 {
        let _ = writer.write_str("NO ACTIVE GRANTS\n");
    }
    writer.written
}

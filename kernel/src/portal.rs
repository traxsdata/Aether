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

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

const MAX_GRANTS: usize = 8;

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
pub fn check(root: &str, path: &str, rights: u64) -> Result<(), PortalError> {
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

/// Writes a one-line portal summary for the desktop.
pub fn summary(out: &mut [u8]) -> usize {
    let mut written = 0usize;
    for line in [
        "DEMO -> /HOME/WELCOME.TXT",
        "ACCESS: READ",
        "SCOPE: SINGLE FILE",
        "REVOCABLE: YES",
    ] {
        let bytes = line.as_bytes();
        if written + bytes.len() + 1 > out.len() {
            break;
        }
        out[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
        out[written] = b'\n';
        written += 1;
    }
    written
}

// SPDX-License-Identifier: Apache-2.0
//! File portal, interactive consent, and persistent grant records.
//!
//! The portal is the only path by which a process can obtain a capability for
//! a file outside its private root. A grant is scoped to a specific process
//! root, path, and right set. There is no directory or pattern grant, so a
//! selected file never implies access to its parent or siblings.
//!
//! Grants are stored as exact-file records in `/system/portal/grants`. The
//! record codec lives in the host-tested `aether-services` crate; this module
//! owns the live table, consent requests, and the audit counters. A pending
//! request grants nothing until a desktop decision installs a record.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_abi::user::FileRights;
use aether_services::portal_store::{self, PortalGrant};
use core::cell::UnsafeCell;
use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicUsize, Ordering};

struct Global<T>(UnsafeCell<T>);

// SAFETY: All access is serialized by [`InterruptGuard`] on the single CPU.
unsafe impl<T> Sync for Global<T> {}

const MAX_GRANTS: usize = portal_store::MAX_GRANTS;
const MAX_PENDING: usize = 4;
const STORE_PATH: &str = "/system/portal/grants";
const STORE_BYTES: usize = 2048;

static GRANTED: AtomicUsize = AtomicUsize::new(0);
static DENIED: AtomicUsize = AtomicUsize::new(0);
static REQUESTS: AtomicUsize = AtomicUsize::new(0);
static APPROVED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Slot {
    grant: PortalGrant,
    revoked: bool,
}

impl Slot {
    const EMPTY: Self = Self {
        grant: PortalGrant::EMPTY,
        revoked: false,
    };
}

#[derive(Clone, Copy)]
struct Request {
    grant: PortalGrant,
    active: bool,
}

impl Request {
    const EMPTY: Self = Self {
        grant: PortalGrant::EMPTY,
        active: false,
    };
}

static GRANTS: Global<[Slot; MAX_GRANTS]> = Global(UnsafeCell::new([Slot::EMPTY; MAX_GRANTS]));
static PENDING: Global<[Request; MAX_PENDING]> =
    Global(UnsafeCell::new([Request::EMPTY; MAX_PENDING]));

/// Portal failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalError {
    /// No grant matches the caller, path, and requested rights.
    NotGranted,
    /// The consent queue is full or the request index is stale.
    NoRequest,
    /// A grant record could not be represented or stored.
    InvalidRecord,
}

/// Loads persisted grants and seeds the first-boot demo grant.
pub fn init() {
    let mut stored = [0u8; STORE_BYTES];
    let mut parsed = [PortalGrant::EMPTY; MAX_GRANTS];
    let mut loaded = 0usize;
    let mut corrupt = false;
    if let Ok(length) = crate::fs::read_path(STORE_PATH, &mut stored) {
        match portal_store::parse(&stored[..length], &mut parsed) {
            Ok(count) => loaded = count,
            Err(_) => corrupt = true,
        }
    }

    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this runs once during boot.
        let grants = unsafe { &mut *GRANTS.0.get() };
        *grants = [Slot::EMPTY; MAX_GRANTS];
        for (index, grant) in parsed.iter().take(loaded).enumerate() {
            grants[index] = Slot {
                grant: *grant,
                revoked: false,
            };
        }
        if loaded == 0
            && !corrupt
            && let Some(grant) = PortalGrant::new(
                crate::fs::DEMO_ROOT,
                "/home/welcome.txt",
                portal_store::READ,
            )
        {
            grants[0] = Slot {
                grant,
                revoked: false,
            };
        }
    }

    if corrupt {
        serial_line!("portal: grant store corrupt; no grants loaded");
    } else if loaded == 0 {
        persist();
        serial_line!("portal: seeded default grant store");
    } else {
        serial_line!("portal: restored {} grant record(s)", loaded);
    }
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
    for slot in grants {
        if slot.revoked || slot.grant.is_empty() {
            continue;
        }
        if slot.grant.admits(root, path, rights) {
            return Ok(());
        }
    }
    Err(PortalError::NotGranted)
}

/// Queues an interactive consent request from an application.
///
/// A request is display state only: it grants no authority until
/// [`approve`] installs the record. Identical pending requests and already
/// granted/revoked paths are refused so a process cannot spam the desktop.
pub fn request(root: &str, path: &str, rights: u64) -> Result<usize, PortalError> {
    let grant = PortalGrant::new(root, path, rights).ok_or(PortalError::InvalidRecord)?;
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the table update.
    let pending = unsafe { &mut *PENDING.0.get() };
    let grants = unsafe { &*GRANTS.0.get() };
    if grants
        .iter()
        .any(|slot| !slot.revoked && slot.grant.admits(root, path, rights))
    {
        return Err(PortalError::InvalidRecord);
    }
    for (index, request) in pending.iter_mut().enumerate() {
        if request.active && request.grant.admits(root, path, rights) {
            return Ok(index);
        }
    }
    for (index, request) in pending.iter_mut().enumerate() {
        if !request.active {
            *request = Request {
                grant,
                active: true,
            };
            REQUESTS.fetch_add(1, Ordering::Release);
            return Ok(index);
        }
    }
    Err(PortalError::NoRequest)
}

/// Approves a pending request by index and persists the new grant.
pub fn approve(index: usize) -> Result<(), PortalError> {
    let grant = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the table update.
        let pending = unsafe { &mut *PENDING.0.get() };
        let Some(request) = pending.get_mut(index) else {
            return Err(PortalError::NoRequest);
        };
        if !request.active {
            return Err(PortalError::NoRequest);
        }
        let grant = request.grant;
        let grants = unsafe { &mut *GRANTS.0.get() };
        let Some(free) = grants.iter().position(|slot| slot.grant.is_empty()) else {
            return Err(PortalError::NoRequest);
        };
        request.active = false;
        grants[free] = Slot {
            grant,
            revoked: false,
        };
        APPROVED.fetch_add(1, Ordering::Release);
        grant
    };
    serial_line!(
        "portal: audit approved root={} path={} rights={:#x}",
        grant.root(),
        grant.path(),
        grant.rights
    );
    persist();
    Ok(())
}

/// Denies a pending request by index.
pub fn deny(index: usize) -> Result<(), PortalError> {
    let grant = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the table update.
        let pending = unsafe { &mut *PENDING.0.get() };
        let Some(request) = pending.get_mut(index) else {
            return Err(PortalError::NoRequest);
        };
        if !request.active {
            return Err(PortalError::NoRequest);
        }
        request.active = false;
        DENIED.fetch_add(1, Ordering::Release);
        request.grant
    };
    serial_line!(
        "portal: audit denied root={} path={} rights={:#x}",
        grant.root(),
        grant.path(),
        grant.rights
    );
    Ok(())
}

/// Revokes the grant matching `root` and `path`.
///
/// Revocation is checked when a new portal open is attempted. A file
/// capability that was already issued remains valid until it is closed:
/// revoking a grant cannot undo bytes the application has already read, and
/// the first implementation chooses not to invalidate an in-flight handle.
/// The revocation is persisted so it survives a service restart.
pub fn revoke(root: &str, path: &str) -> Result<(), PortalError> {
    let found = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this is the only writer.
        let grants = unsafe { &mut *GRANTS.0.get() };
        let mut found = false;
        for slot in grants {
            if !slot.revoked
                && !slot.grant.is_empty()
                && slot.grant.root() == root
                && slot.grant.path() == path
            {
                slot.revoked = true;
                found = true;
            }
        }
        found
    };
    if found {
        audit_revoke(root, path);
        persist();
        Ok(())
    } else {
        Err(PortalError::NotGranted)
    }
}

/// Revokes a grant by its displayed table index.
pub fn revoke_index(index: usize) -> Result<(), PortalError> {
    let grant = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this is the only writer.
        let grants = unsafe { &mut *GRANTS.0.get() };
        let Some(slot) = grants.get_mut(index) else {
            return Err(PortalError::NotGranted);
        };
        if slot.revoked || slot.grant.is_empty() {
            return Err(PortalError::NotGranted);
        }
        slot.revoked = true;
        slot.grant
    };
    audit_revoke(grant.root(), grant.path());
    persist();
    Ok(())
}

/// Removes a tombstoned record so the slot can be reused.
///
/// The UI calls this after showing the revocation; the grant is already
/// persisted as absent, so this only compacts the live table.
pub fn forget_revoked() -> usize {
    let mut removed = 0usize;
    {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked and this is the only writer.
        let grants = unsafe { &mut *GRANTS.0.get() };
        for slot in grants {
            if slot.revoked {
                *slot = Slot::EMPTY;
                removed += 1;
            }
        }
    }
    if removed != 0 {
        persist();
    }
    removed
}

/// Emits the revocation audit line for the console log.
fn audit_revoke(root: &str, path: &str) {
    serial_line!("portal: audit revoked root={root} path={path}");
}

/// Writes the active grant table to the persistent store.
fn persist() {
    let mut encoded = [0u8; STORE_BYTES];
    let length = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let grants = unsafe { &*GRANTS.0.get() };
        let active: [PortalGrant; MAX_GRANTS] = core::array::from_fn(|index| {
            if grants[index].revoked {
                PortalGrant::EMPTY
            } else {
                grants[index].grant
            }
        });
        portal_store::encode(&active, &mut encoded)
    };
    let Some(length) = length else {
        serial_line!("portal: failed to encode grant store");
        return;
    };
    if crate::fs::write_path(STORE_PATH, &encoded[..length]).is_err() {
        serial_line!("portal: failed to write grant store");
    }
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

/// Returns the number of interactive consent requests accepted.
#[must_use]
pub fn request_count() -> usize {
    REQUESTS.load(Ordering::Acquire)
}

/// Returns the number of approved consent requests.
#[must_use]
pub fn approved_count() -> usize {
    APPROVED.load(Ordering::Acquire)
}

/// Exercises grant creation, exact matching, revocation, and persistence.
///
/// The temporary request uses real portal paths and is removed afterwards, so
/// the application's seeded grant is unaffected.
pub fn self_test() -> Result<usize, PortalError> {
    const PATH: &str = "/portal/README.txt";
    let mut checks = 0usize;

    let consent_request = request(crate::fs::DEMO_ROOT, PATH, portal_store::READ)?;
    checks += 1;
    if check(crate::fs::DEMO_ROOT, PATH, portal_store::READ).is_ok() {
        return Err(PortalError::NotGranted);
    }
    checks += 1;
    approve(consent_request)?;
    checks += 1;
    if check(crate::fs::DEMO_ROOT, PATH, portal_store::READ).is_err() {
        return Err(PortalError::NotGranted);
    }
    checks += 1;
    if check(crate::fs::DEMO_ROOT, PATH, FileRights::WRITE).is_ok() {
        return Err(PortalError::NotGranted);
    }
    checks += 1;
    revoke(crate::fs::DEMO_ROOT, PATH)?;
    checks += 1;
    if check(crate::fs::DEMO_ROOT, PATH, portal_store::READ).is_ok() {
        return Err(PortalError::NotGranted);
    }
    checks += 1;

    // Round-trip the live table through the persistent codec. This catches a
    // record that the writer accepts but the reader cannot reload before the
    // next boot uses the same store.
    let mut encoded = [0u8; STORE_BYTES];
    let mut parsed = [PortalGrant::EMPTY; MAX_GRANTS];
    let length = {
        let _guard = InterruptGuard::enter();
        // SAFETY: Interrupts are masked for the snapshot.
        let grants = unsafe { &*GRANTS.0.get() };
        let active: [PortalGrant; MAX_GRANTS] = core::array::from_fn(|index| {
            if grants[index].revoked {
                PortalGrant::EMPTY
            } else {
                grants[index].grant
            }
        });
        portal_store::encode(&active, &mut encoded).ok_or(PortalError::InvalidRecord)?
    };
    let count = portal_store::parse(&encoded[..length], &mut parsed)
        .map_err(|_| PortalError::InvalidRecord)?;
    if count != grant_count() {
        return Err(PortalError::InvalidRecord);
    }
    checks += 1;

    // A denied request must never mint a grant.
    let denied_request = request(
        crate::fs::DEMO_ROOT,
        "/portal/denied.txt",
        portal_store::READ,
    )?;
    deny(denied_request)?;
    if check(
        crate::fs::DEMO_ROOT,
        "/portal/denied.txt",
        portal_store::READ,
    )
    .is_ok()
    {
        return Err(PortalError::NotGranted);
    }
    checks += 1;

    forget_revoked();
    persist();
    Ok(checks)
}

/// Returns the number of active grants.
#[must_use]
pub fn grant_count() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    grants
        .iter()
        .filter(|slot| !slot.revoked && !slot.grant.is_empty())
        .count()
}

/// Returns the number of pending consent requests.
#[must_use]
pub fn pending_count() -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let pending = unsafe { &*PENDING.0.get() };
    pending.iter().filter(|request| request.active).count()
}

/// Returns the active grant record at `index` for display or testing.
#[must_use]
pub fn grant(index: usize) -> Option<PortalGrant> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    grants
        .get(index)
        .filter(|slot| !slot.revoked && !slot.grant.is_empty())
        .map(|slot| slot.grant)
}

/// Returns the pending request at `index` for a consent decision.
#[must_use]
pub fn pending(index: usize) -> Option<PortalGrant> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let pending = unsafe { &*PENDING.0.get() };
    pending
        .get(index)
        .filter(|request| request.active)
        .map(|request| request.grant)
}

/// Prints the grant table for the shell.
pub fn print_grants() {
    serial_line!(
        "portal: grants={} pending={} requests={} approved={}",
        grant_count(),
        pending_count(),
        request_count(),
        approved_count()
    );
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let grants = unsafe { &*GRANTS.0.get() };
    let pending = unsafe { &*PENDING.0.get() };
    for (index, slot) in grants.iter().enumerate() {
        if slot.grant.is_empty() {
            continue;
        }
        serial_line!(
            "portal: grant {} root={} path={} rights={:#x} revoked={}",
            index,
            slot.grant.root(),
            slot.grant.path(),
            slot.grant.rights,
            slot.revoked
        );
    }
    for (index, request) in pending.iter().enumerate() {
        if !request.active {
            continue;
        }
        serial_line!(
            "portal: pending {} root={} path={} rights={:#x}",
            index,
            request.grant.root(),
            request.grant.path(),
            request.grant.rights
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
    for slot in grants {
        if slot.revoked || slot.grant.is_empty() {
            continue;
        }
        active += 1;
        let _ = writeln!(
            writer,
            "ROOT: {}\nPATH: {}\nACCESS: {}\nSCOPE: SINGLE FILE\nREVOCABLE: YES\n",
            slot.grant.root(),
            slot.grant.path(),
            rights_name(slot.grant.rights)
        );
    }
    if active == 0 {
        let _ = writer.write_str("NO ACTIVE GRANTS\n");
    }
    writer.written
}

/// Writes pending consent requests for the desktop.
pub fn pending_summary(out: &mut [u8]) -> usize {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the read.
    let pending = unsafe { &*PENDING.0.get() };
    let mut writer = SummaryWriter { out, written: 0 };
    let mut active = 0usize;
    for (index, request) in pending.iter().enumerate() {
        if !request.active {
            continue;
        }
        active += 1;
        let _ = writeln!(
            writer,
            "#{} ROOT: {}\nPATH: {}\nACCESS: {}\n",
            index,
            request.grant.root(),
            request.grant.path(),
            rights_name(request.grant.rights)
        );
    }
    if active == 0 {
        let _ = writer.write_str("NO PENDING REQUESTS\n");
    }
    writer.written
}

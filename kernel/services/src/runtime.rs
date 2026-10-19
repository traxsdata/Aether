// SPDX-License-Identifier: Apache-2.0
//! Per-application runtime profiles.
//!
//! A packaged application does not choose its own storage root. The runtime
//! derives the root from the application identity (`/apps/<id>`), carries the
//! permissions and resource limits declared in the verified manifest, and
//! starts with no network or device access because no such permission exists
//! in the package policy.
//!
//! Sharing a prefix is possible only with explicit consent: the default is one
//! environment per application. The functions here are policy; enforcing the
//! prefix is the filesystem service's per-process root, and enforcing limits is
//! the scheduler and file service's job. Keeping the decision logic separate
//! makes it host-testable.

use aether_packaging::manifest::{Limits, Permissions};

/// Maximum profiles in one table.
pub const MAX_PROFILES: usize = 8;

/// Runtime policy errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    /// The identity is not a valid reverse-domain name.
    InvalidIdentity,
    /// The prefix is not exactly `/apps/<id>`.
    InvalidPrefix,
    /// Another environment already owns the prefix.
    PrefixInUse,
    /// Prefix sharing was attempted without an explicit user decision.
    SharingNotAuthorized,
    /// The requested path leaves the application root.
    PrefixEscape,
    /// The application did not declare the permission.
    NotPermitted,
    /// A resource limit would be exceeded.
    LimitExceeded,
    /// The profile table is full.
    TableFull,
    /// No profile with that identity is registered.
    UnknownProfile,
    /// A manifest-derived value could not be applied.
    ManifestInvalid,
}

/// A runtime environment for one application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeProfile<'a> {
    /// Stable application identity.
    pub id: &'a str,
    /// Private storage root.
    pub prefix: &'a str,
    /// Permissions declared by the verified manifest.
    pub permissions: Permissions,
    /// Resource limits declared by the verified manifest.
    pub limits: Limits,
    /// Whether the user explicitly chose to share this prefix.
    pub shared_prefix: bool,
}

/// Derives the runtime prefix for `id`.
///
/// Returns the prefix length written into `out`, or `None` if the buffer is too
/// small. The caller owns the storage for the returned `&str`.
pub fn derive_prefix<'a>(id: &str, out: &'a mut [u8]) -> Option<&'a str> {
    if !aether_packaging::manifest::is_valid_identity(id) {
        return None;
    }
    let prefix = b"/apps/";
    let length = prefix.len().checked_add(id.len())?;
    let slice = out.get_mut(..length)?;
    slice[..prefix.len()].copy_from_slice(prefix);
    slice[prefix.len()..].copy_from_slice(id.as_bytes());
    core::str::from_utf8(slice).ok()
}

/// Returns `true` if `prefix` is the canonical prefix for `id`.
#[must_use]
pub fn is_canonical_prefix(id: &str, prefix: &str) -> bool {
    let Some(rest) = prefix.strip_prefix("/apps/") else {
        return false;
    };
    rest == id
}

/// Creates an isolated profile.
///
/// The prefix must be derived from the identity; a manifest cannot place an
/// application in another application's storage.
pub fn isolated<'a>(
    id: &'a str,
    prefix: &'a str,
    permissions: Permissions,
    limits: Limits,
) -> Result<RuntimeProfile<'a>, RuntimeError> {
    if !aether_packaging::manifest::is_valid_identity(id) {
        return Err(RuntimeError::InvalidIdentity);
    }
    if !is_canonical_prefix(id, prefix) {
        return Err(RuntimeError::InvalidPrefix);
    }
    Ok(RuntimeProfile {
        id,
        prefix,
        permissions,
        limits,
        shared_prefix: false,
    })
}

impl<'a> RuntimeProfile<'a> {
    /// Returns `true` if the application declared `permission`.
    #[must_use]
    pub const fn allows(&self, permission: u16) -> bool {
        self.permissions.contains(permission)
    }

    /// Returns `true` if the application may open `open` files.
    #[must_use]
    pub const fn allows_open_files(&self, open: u8) -> bool {
        open <= self.limits.open_files
    }

    /// Returns `true` if a memory reservation of `kib` fits the quota.
    #[must_use]
    pub const fn allows_memory(&self, kib: u16) -> bool {
        kib <= self.limits.memory_kib
    }

    /// Returns `true` if `tasks` runtime tasks fit the quota.
    #[must_use]
    pub const fn allows_tasks(&self, tasks: u8) -> bool {
        tasks <= self.limits.tasks
    }

    /// Checks a path relative to the application root.
    ///
    /// Absolute paths, empty components, and `..` are rejected before the
    /// filesystem service sees them. A denied path is a containment failure,
    /// not a hint to retry elsewhere.
    pub fn check_relative_path(&self, path: &str) -> Result<(), RuntimeError> {
        if path.is_empty() || path.starts_with('/') {
            return Err(RuntimeError::PrefixEscape);
        }
        for component in path.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(RuntimeError::PrefixEscape);
            }
        }
        Ok(())
    }

    /// Adopts another application's prefix.
    ///
    /// This is the documented exception to per-application isolation: both
    /// applications may then read each other's files. It requires an explicit
    /// user decision (`consent`), is expected to be rare, and must be surfaced
    /// in the Security Center.
    pub fn adopt_prefix(
        &mut self,
        owner: &RuntimeProfile<'a>,
        consent: bool,
    ) -> Result<(), RuntimeError> {
        if !consent {
            return Err(RuntimeError::SharingNotAuthorized);
        }
        self.prefix = owner.prefix;
        self.shared_prefix = true;
        Ok(())
    }
}

/// Bounded profile table.
pub struct RuntimeTable<'a, const N: usize> {
    profiles: [Option<RuntimeProfile<'a>>; N],
    len: usize,
}

impl<'a, const N: usize> RuntimeTable<'a, N> {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profiles: [None; N],
            len: 0,
        }
    }

    /// Registers an isolated profile.
    pub fn register(&mut self, profile: RuntimeProfile<'a>) -> Result<usize, RuntimeError> {
        if self
            .profiles
            .iter()
            .flatten()
            .any(|existing| existing.prefix == profile.prefix)
        {
            return Err(RuntimeError::PrefixInUse);
        }
        let slot = self
            .profiles
            .iter()
            .position(Option::is_none)
            .ok_or(RuntimeError::TableFull)?;
        self.profiles[slot] = Some(profile);
        self.len += 1;
        Ok(slot)
    }

    /// Looks up a profile by identity.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&RuntimeProfile<'a>> {
        self.profiles
            .iter()
            .flatten()
            .find(|profile| profile.id == id)
    }

    /// Number of profiles.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no profile is registered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Iterates over profiles.
    pub fn profiles(&self) -> impl Iterator<Item = &RuntimeProfile<'a>> {
        self.profiles.iter().flatten()
    }
}

impl<const N: usize> Default for RuntimeTable<'_, N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;

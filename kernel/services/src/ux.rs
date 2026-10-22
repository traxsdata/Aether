// SPDX-License-Identifier: Apache-2.0
//! Host-testable policy for the unified filesystem user experience.
//!
//! AETHER-UX-002 separates three things that traditional desktops conflate:
//! user-owned files, application-private storage, and resources the user has
//! explicitly shared. This module models the parts that must be decided before
//! the kernel stores bytes:
//!
//! * stable resource identities that survive rename and move;
//! * explicit ownership domains;
//! * capability grants scoped to one file or one folder subtree;
//! * one-operation and persistent grant lifetimes;
//! * revocation and search visibility;
//! * application-private storage categories and cleanup rules.
//!
//! The kernel filesystem service remains the authority. These rules are the
//! shared, allocation-free policy it can call, and the host tests exercise the
//! same rules without booting a machine.

/// Maximum resources tracked by a [`ResourceTable`].
pub const MAX_RESOURCES: usize = 64;
/// Maximum grants in a [`GrantTable`].
pub const MAX_GRANTS: usize = 32;
/// Maximum application identity length.
pub const MAX_APP_ID_BYTES: usize = 48;
/// Maximum displayed resource name length.
pub const MAX_RESOURCE_NAME_BYTES: usize = 32;
/// Maximum search results in one bounded query.
pub const MAX_SEARCH_RESULTS: usize = 16;
/// Maximum recent items retained for the user.
pub const MAX_RECENT: usize = 16;

/// Stable application identity, never a display name or install directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId {
    bytes: [u8; MAX_APP_ID_BYTES],
    len: u8,
}

impl AppId {
    /// An empty identity. It never matches a real application.
    pub const EMPTY: Self = Self {
        bytes: [0; MAX_APP_ID_BYTES],
        len: 0,
    };

    /// Creates a validated package identity.
    #[must_use]
    pub fn new(id: &str) -> Option<Self> {
        if id.is_empty()
            || id.len() > MAX_APP_ID_BYTES
            || id == "."
            || id == ".."
            || id
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
        {
            return None;
        }
        let mut app = Self::EMPTY;
        app.bytes[..id.len()].copy_from_slice(id.as_bytes());
        app.len = id.len() as u8;
        Some(app)
    }

    /// Returns the package identity text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }

    /// Returns `true` for the empty sentinel.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Stable resource identity. It is independent of a displayed name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceId(u64);

impl ResourceId {
    /// No resource.
    pub const NONE: Self = Self(0);

    /// Returns the encoded identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns `true` when this names no resource.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

/// Who owns a resource and therefore controls its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// System image, boot, recovery, and logs.
    System,
    /// Private storage belonging to exactly one application identity.
    Application(AppId),
    /// A user's documents and personal workspace.
    User,
    /// A user-owned resource explicitly shared with applications.
    Shared,
    /// Removable media or a remote provider.
    External,
    /// A deleted resource retained for identity and Trash restoration.
    Trash,
}

impl Owner {
    /// Returns `true` when ordinary application browsing must not reveal it.
    #[must_use]
    pub const fn is_application_private(self) -> bool {
        matches!(self, Self::Application(_))
    }
}

/// The kind of filesystem object named by a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A provider-backed collection whose internal records stay private.
    Collection,
    /// A removed resource whose identity is retained for restoration.
    Deleted,
}

/// One resource in the unified namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resource {
    id: ResourceId,
    parent: ResourceId,
    owner: Owner,
    kind: ResourceKind,
    name: FixedName,
}

impl Resource {
    /// Stable identity.
    #[must_use]
    pub const fn id(self) -> ResourceId {
        self.id
    }
    /// Parent identity, or [`ResourceId::NONE`] for a namespace root.
    #[must_use]
    pub const fn parent(self) -> ResourceId {
        self.parent
    }
    /// Ownership domain.
    #[must_use]
    pub const fn owner(self) -> Owner {
        self.owner
    }
    /// Object kind.
    #[must_use]
    pub const fn kind(self) -> ResourceKind {
        self.kind
    }
    /// Displayed name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }
}

/// Bounded name storage used so the model allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FixedName {
    bytes: [u8; MAX_RESOURCE_NAME_BYTES],
    len: u8,
}

impl FixedName {
    const EMPTY: Self = Self {
        bytes: [0; MAX_RESOURCE_NAME_BYTES],
        len: 0,
    };

    fn new(name: &str) -> Option<Self> {
        if name.is_empty()
            || name.len() > MAX_RESOURCE_NAME_BYTES
            || name == "."
            || name == ".."
            || name
                .bytes()
                .any(|byte| byte == b'/' || byte.is_ascii_control())
        {
            return None;
        }
        let mut fixed = Self::EMPTY;
        fixed.bytes[..name.len()].copy_from_slice(name.as_bytes());
        fixed.len = name.len() as u8;
        Some(fixed)
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

/// Resource-table failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UxError {
    /// The table is full.
    Full,
    /// The named resource does not exist or was deleted.
    NotFound,
    /// A name or identity is malformed.
    Invalid,
    /// A resource cannot be its own parent or moved below its descendant.
    Cycle,
    /// The same parent already contains that name.
    Exists,
    /// A grant requests no rights or an invalid combination.
    InvalidRights,
    /// The requesting application may not grant or inspect this resource.
    Denied,
}

/// Bounded resource namespace.
pub struct ResourceTable {
    slots: [Option<Resource>; MAX_RESOURCES],
    next: u64,
}

impl ResourceTable {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [None; MAX_RESOURCES],
            next: 1,
        }
    }

    /// Registers a resource below `parent`, or at a namespace root when
    /// `parent` is [`ResourceId::NONE`].
    pub fn register(
        &mut self,
        parent: ResourceId,
        owner: Owner,
        name: &str,
        kind: ResourceKind,
    ) -> Result<ResourceId, UxError> {
        let name = FixedName::new(name).ok_or(UxError::Invalid)?;
        if !parent.is_none() && self.get(parent)?.kind == ResourceKind::Deleted {
            return Err(UxError::NotFound);
        }
        if self.slots.iter().flatten().any(|resource| {
            resource.parent == parent
                && resource.name == name
                && resource.kind != ResourceKind::Deleted
        }) {
            return Err(UxError::Exists);
        }
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(UxError::Full)?;
        let id = ResourceId(self.next);
        self.next = self.next.checked_add(1).ok_or(UxError::Full)?;
        self.slots[slot] = Some(Resource {
            id,
            parent,
            owner,
            kind,
            name,
        });
        Ok(id)
    }

    /// Returns a live resource.
    pub fn get(&self, id: ResourceId) -> Result<Resource, UxError> {
        self.slots
            .iter()
            .flatten()
            .find(|resource| resource.id == id && resource.kind != ResourceKind::Deleted)
            .copied()
            .ok_or(UxError::NotFound)
    }

    /// Renames a resource without changing its identity.
    pub fn rename(&mut self, id: ResourceId, name: &str) -> Result<(), UxError> {
        let name = FixedName::new(name).ok_or(UxError::Invalid)?;
        let parent = self.get(id)?.parent;
        if self.slots.iter().flatten().any(|resource| {
            resource.id != id
                && resource.parent == parent
                && resource.name == name
                && resource.kind != ResourceKind::Deleted
        }) {
            return Err(UxError::Exists);
        }
        let slot = self.slot(id)?;
        self.slots[slot].as_mut().ok_or(UxError::NotFound)?.name = name;
        Ok(())
    }

    /// Moves a resource without changing its identity.
    pub fn move_to(&mut self, id: ResourceId, parent: ResourceId) -> Result<(), UxError> {
        if id == parent || (!parent.is_none() && self.is_descendant(parent, id)?) {
            return Err(UxError::Cycle);
        }
        if !parent.is_none() {
            let destination = self.get(parent)?;
            if destination.kind != ResourceKind::Directory
                && destination.kind != ResourceKind::Collection
            {
                return Err(UxError::Invalid);
            }
        }
        let name = self.get(id)?.name;
        if self
            .slots
            .iter()
            .flatten()
            .any(|resource| resource.id != id && resource.parent == parent && resource.name == name)
        {
            return Err(UxError::Exists);
        }
        let slot = self.slot(id)?;
        self.slots[slot].as_mut().ok_or(UxError::NotFound)?.parent = parent;
        Ok(())
    }

    /// Marks a resource deleted while retaining its identity.
    ///
    /// A deleted resource cannot be opened or enumerated, but its identifier
    /// remains recognizable so stale references fail closed instead of naming
    /// a later resource that reused the slot.
    pub fn remove(&mut self, id: ResourceId) -> Result<Owner, UxError> {
        let slot = self.slot(id)?;
        let resource = self.slots[slot].as_mut().ok_or(UxError::NotFound)?;
        if resource.kind == ResourceKind::Deleted {
            return Err(UxError::NotFound);
        }
        let previous = resource.owner;
        resource.kind = ResourceKind::Deleted;
        resource.owner = Owner::Trash;
        Ok(previous)
    }

    /// Changes ownership only through an explicit system operation.
    pub fn set_owner(&mut self, id: ResourceId, owner: Owner) -> Result<(), UxError> {
        self.get(id)?;
        let slot = self.slot(id)?;
        self.slots[slot].as_mut().ok_or(UxError::NotFound)?.owner = owner;
        Ok(())
    }

    /// Returns `true` when `child` is at or below `ancestor`.
    pub fn is_descendant(&self, child: ResourceId, ancestor: ResourceId) -> Result<bool, UxError> {
        let mut current = child;
        for _ in 0..=MAX_RESOURCES {
            if current == ancestor {
                return Ok(true);
            }
            if current.is_none() {
                return Ok(false);
            }
            current = self.get(current)?.parent;
        }
        Err(UxError::Invalid)
    }

    /// Number of live (not deleted) resources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|resource| resource.kind != ResourceKind::Deleted)
            .count()
    }

    /// Returns `true` when no live resource exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn slot(&self, id: ResourceId) -> Result<usize, UxError> {
        self.slots
            .iter()
            .position(|resource| resource.is_some_and(|resource| resource.id == id))
            .ok_or(UxError::NotFound)
    }
}

impl Default for ResourceTable {
    fn default() -> Self {
        Self::new()
    }
}

/// File and folder rights checked independently of the opening operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(u8);

impl Rights {
    /// Read file bytes or metadata.
    pub const READ: Self = Self(1 << 0);
    /// Write or replace bytes.
    pub const WRITE: Self = Self(1 << 1);
    /// Create a child entry.
    pub const CREATE: Self = Self(1 << 2);
    /// Delete an entry.
    pub const DELETE: Self = Self(1 << 3);
    /// Rename or move an entry.
    pub const RENAME: Self = Self(1 << 4);
    /// List a directory's children.
    pub const ENUMERATE: Self = Self(1 << 5);
    /// Execute a program image.
    pub const EXECUTE: Self = Self(1 << 6);

    /// No rights.
    pub const NONE: Self = Self(0);

    /// Returns `true` when every requested right is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Returns `true` for an empty right set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` when the set contains only defined rights.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 & !Self::ALL.0 == 0 && self.0 != 0
    }

    const ALL: Self = Self(0x7f);
}

impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Whether a grant names one resource or a folder subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantScope {
    /// Exactly the named resource. Sibling access remains denied.
    File,
    /// The named folder and its descendants, never its parent or siblings.
    Folder,
}

/// Grant lifetime shown to the user and stored by the portal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantLifetime {
    /// One operation or session. The caller consumes it explicitly.
    OneTime,
    /// Retained until revoked or invalidated by policy.
    Persistent,
}

/// One application's capability grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    /// Receiving application identity.
    pub app: AppId,
    /// Granted resource identity.
    pub resource: ResourceId,
    /// File or folder scope.
    pub scope: GrantScope,
    /// Granted rights.
    pub rights: Rights,
    /// One-time or persistent.
    pub lifetime: GrantLifetime,
    /// Whether the user revoked the grant.
    pub revoked: bool,
}

impl Grant {
    /// An empty grant slot.
    pub const EMPTY: Self = Self {
        app: AppId::EMPTY,
        resource: ResourceId::NONE,
        scope: GrantScope::File,
        rights: Rights::NONE,
        lifetime: GrantLifetime::OneTime,
        revoked: false,
    };

    /// Returns `true` when this grant admits the request.
    #[must_use]
    pub fn admits(
        &self,
        resources: &ResourceTable,
        app: AppId,
        resource: ResourceId,
        rights: Rights,
    ) -> bool {
        if self.revoked
            || self.app != app
            || rights.is_empty()
            || !rights.is_valid()
            || !self.rights.contains(rights)
        {
            return false;
        }
        match self.scope {
            GrantScope::File => self.resource == resource,
            GrantScope::Folder => {
                // A folder grant never covers a file's siblings by accident:
                // only the folder itself and resources below it match.
                resources
                    .is_descendant(resource, self.resource)
                    .unwrap_or(false)
            }
        }
    }
}

/// Bounded grant table.
pub struct GrantTable {
    slots: [Grant; MAX_GRANTS],
}

impl GrantTable {
    /// Creates an empty table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [Grant::EMPTY; MAX_GRANTS],
        }
    }

    /// Installs a grant after validating the scope and rights.
    ///
    /// A file grant may not ask for directory enumeration: the file-selection
    /// portal exposes the selected file, not its parent listing.
    pub fn grant(
        &mut self,
        resources: &ResourceTable,
        app: AppId,
        resource: ResourceId,
        scope: GrantScope,
        rights: Rights,
        lifetime: GrantLifetime,
    ) -> Result<usize, UxError> {
        if app.is_empty() || !rights.is_valid() {
            return Err(UxError::InvalidRights);
        }
        let object = resources.get(resource)?;
        if scope == GrantScope::File && object.kind == ResourceKind::Directory {
            return Err(UxError::Invalid);
        }
        if scope == GrantScope::Folder
            && !matches!(
                object.kind,
                ResourceKind::Directory | ResourceKind::Collection
            )
        {
            return Err(UxError::Invalid);
        }
        if scope == GrantScope::File && rights.contains(Rights::ENUMERATE) {
            return Err(UxError::InvalidRights);
        }
        if object.owner.is_application_private() && object.owner != Owner::Application(app) {
            return Err(UxError::Denied);
        }
        let slot = self
            .slots
            .iter()
            .position(|grant| grant.app.is_empty())
            .ok_or(UxError::Full)?;
        self.slots[slot] = Grant {
            app,
            resource,
            scope,
            rights,
            lifetime,
            revoked: false,
        };
        Ok(slot)
    }

    /// Checks whether an application currently holds `rights` on `resource`.
    #[must_use]
    pub fn authorizes(
        &self,
        resources: &ResourceTable,
        app: AppId,
        resource: ResourceId,
        rights: Rights,
    ) -> bool {
        self.slots
            .iter()
            .any(|grant| grant.admits(resources, app, resource, rights))
    }

    /// Checks and consumes a one-time grant.
    ///
    /// Persistent grants remain usable. A one-time grant is marked revoked
    /// after the first successful authorization, so a repeated operation fails
    /// even though the caller kept the old record.
    pub fn consume(
        &mut self,
        resources: &ResourceTable,
        app: AppId,
        resource: ResourceId,
        rights: Rights,
    ) -> bool {
        let Some(index) = self
            .slots
            .iter()
            .position(|grant| grant.admits(resources, app, resource, rights))
        else {
            return false;
        };
        if self.slots[index].lifetime == GrantLifetime::OneTime {
            self.slots[index].revoked = true;
        }
        true
    }

    /// Revokes one grant slot.
    pub fn revoke(&mut self, index: usize) -> bool {
        let Some(grant) = self.slots.get_mut(index) else {
            return false;
        };
        if grant.app.is_empty() || grant.revoked {
            return false;
        }
        grant.revoked = true;
        true
    }

    /// Revokes every grant held by an application, e.g. during uninstall.
    pub fn revoke_app(&mut self, app: AppId) -> usize {
        let mut revoked = 0usize;
        for grant in &mut self.slots {
            if !grant.revoked && grant.app == app {
                grant.revoked = true;
                revoked += 1;
            }
        }
        revoked
    }

    /// Counts active grants.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|grant| !grant.app.is_empty() && !grant.revoked)
            .count()
    }

    /// Returns `true` when no active grant exists.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for GrantTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Who is asking a search or metadata question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requester {
    /// The signed-in user, through a trusted system interface.
    User,
    /// An application in its own security domain.
    Application(AppId),
    /// A named system service with its own explicit capability profile.
    Service,
}

/// Returns whether `requester` may see a resource and its metadata.
///
/// The desktop does not gain unrestricted authority by being the desktop: the
/// user view intentionally hides application-private storage, and an
/// application sees only its own vault plus explicit grants.
#[must_use]
pub fn can_see(
    resources: &ResourceTable,
    grants: &GrantTable,
    requester: Requester,
    resource: ResourceId,
) -> bool {
    let Ok(object) = resources.get(resource) else {
        return false;
    };
    match requester {
        Requester::User => !object.owner.is_application_private(),
        Requester::Service => false,
        Requester::Application(app) => {
            if object.owner == Owner::Application(app) {
                return true;
            }
            if object.owner.is_application_private() {
                return false;
            }
            grants.authorizes(resources, app, resource, Rights::READ)
                || grants.authorizes(resources, app, resource, Rights::ENUMERATE)
        }
    }
}

/// Searches visible resource names only.
///
/// The caller cannot learn about an inaccessible resource through a match,
/// count, or suggestion because the filter runs before results are emitted.
pub fn search(
    resources: &ResourceTable,
    grants: &GrantTable,
    requester: Requester,
    query: &str,
    out: &mut [ResourceId],
) -> usize {
    if query.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    for resource in resources.slots.iter().flatten() {
        if resource.kind == ResourceKind::Deleted
            || !can_see(resources, grants, requester, resource.id)
            || !contains_ignore_ascii_case(resource.name(), query)
        {
            continue;
        }
        if count == out.len() || count == MAX_SEARCH_RESULTS {
            break;
        }
        out[count] = resource.id;
        count += 1;
    }
    count
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Application-private storage categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    /// Persistent application state.
    Data,
    /// Regenerable cache.
    Cache,
    /// Preferences and configuration.
    Settings,
    /// Short-lived working data.
    Temporary,
}

/// Per-category storage accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageUsage {
    /// Persistent data bytes.
    pub data: u64,
    /// Cache bytes.
    pub cache: u64,
    /// Settings bytes.
    pub settings: u64,
    /// Temporary bytes.
    pub temporary: u64,
}

impl StorageUsage {
    /// Empty usage.
    pub const ZERO: Self = Self {
        data: 0,
        cache: 0,
        settings: 0,
        temporary: 0,
    };

    /// Total bytes across all categories.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.data
            .saturating_add(self.cache)
            .saturating_add(self.settings)
            .saturating_add(self.temporary)
    }

    /// Removes regenerable cache and returns the bytes actually released.
    pub fn clear_cache(&mut self) -> u64 {
        let released = self.cache;
        self.cache = 0;
        released
    }

    /// Removes temporary data on session end.
    pub fn clear_temporary(&mut self) -> u64 {
        let released = self.temporary;
        self.temporary = 0;
        released
    }
}

/// One application's private storage accounting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppStorage {
    /// Application identity.
    pub app: AppId,
    /// Current usage.
    pub usage: StorageUsage,
    /// Total quota in bytes.
    pub quota: u64,
    /// Whether private data is retained after uninstall.
    pub retain_after_uninstall: bool,
}

impl AppStorage {
    /// Creates empty storage for an application.
    #[must_use]
    pub const fn new(app: AppId, quota: u64) -> Self {
        Self {
            app,
            usage: StorageUsage::ZERO,
            quota,
            retain_after_uninstall: true,
        }
    }

    /// Accounts for a write to one category, enforcing the quota.
    pub fn charge(&mut self, kind: StorageKind, bytes: u64) -> bool {
        let next = self.usage.total().saturating_add(bytes);
        if next > self.quota {
            return false;
        }
        match kind {
            StorageKind::Data => self.usage.data = self.usage.data.saturating_add(bytes),
            StorageKind::Cache => self.usage.cache = self.usage.cache.saturating_add(bytes),
            StorageKind::Settings => {
                self.usage.settings = self.usage.settings.saturating_add(bytes)
            }
            StorageKind::Temporary => {
                self.usage.temporary = self.usage.temporary.saturating_add(bytes)
            }
        }
        true
    }

    /// Cleanup action. Cache or temporary removal never touches persistent
    /// data or settings.
    pub fn cleanup(&mut self, kind: StorageKind) -> u64 {
        match kind {
            StorageKind::Cache => self.usage.clear_cache(),
            StorageKind::Temporary => self.usage.clear_temporary(),
            StorageKind::Data | StorageKind::Settings => 0,
        }
    }
}

/// What an uninstall operation removes and what it preserves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UninstallPlan {
    /// Package files and registrations are removed.
    pub remove_package: bool,
    /// Private persistent data is retained for recovery.
    pub retain_private_data: bool,
    /// User-owned documents are never removed by this operation.
    pub preserve_user_documents: bool,
    /// Number of grants revoked.
    pub grants_revoked: usize,
}

/// Plans an uninstall without touching user-owned resources.
///
/// `delete_private` corresponds to the explicit "Uninstall and Delete
/// Application Data" action. The default uninstall keeps private data.
pub fn plan_uninstall(
    grants: &mut GrantTable,
    storage: &AppStorage,
    delete_private: bool,
) -> UninstallPlan {
    UninstallPlan {
        remove_package: true,
        retain_private_data: !delete_private && storage.retain_after_uninstall,
        preserve_user_documents: true,
        grants_revoked: grants.revoke_app(storage.app),
    }
}

/// Bounded user-facing Recent list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recent {
    items: [ResourceId; MAX_RECENT],
    count: usize,
}

impl Recent {
    /// Creates an empty list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            items: [ResourceId::NONE; MAX_RECENT],
            count: 0,
        }
    }

    /// Records a user-authorized item. Recording does not grant access.
    pub fn record(&mut self, id: ResourceId) {
        if self.items[..self.count].contains(&id) {
            return;
        }
        if self.count < MAX_RECENT {
            self.items[self.count] = id;
            self.count += 1;
            return;
        }
        self.items.copy_within(1.., 0);
        self.items[MAX_RECENT - 1] = id;
    }

    /// Removes one item from Recent. The resource itself is untouched.
    pub fn remove(&mut self, id: ResourceId) -> bool {
        let Some(index) = self.items[..self.count].iter().position(|item| *item == id) else {
            return false;
        };
        self.items.copy_within(index + 1..self.count, index);
        self.count -= 1;
        self.items[self.count] = ResourceId::NONE;
        true
    }

    /// Clears the view without deleting any file.
    pub fn clear(&mut self) {
        self.items = [ResourceId::NONE; MAX_RECENT];
        self.count = 0;
    }

    /// Number of recorded items.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.count
    }

    /// Returns `true` when nothing is recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the recorded identity at `index`.
    #[must_use]
    pub fn item(&self, index: usize) -> Option<ResourceId> {
        (index < self.count).then_some(self.items[index])
    }
}

impl Default for Recent {
    fn default() -> Self {
        Self::new()
    }
}

/// Primary navigation destinations from AETHER-UX-002 §2.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// Workspace overview.
    Home,
    /// Recently opened user-authorized resources.
    Recent,
    /// User-pinned resources.
    Favorites,
    /// User-owned documents.
    Documents,
    /// Downloaded files.
    Downloads,
    /// Images and photos.
    Pictures,
    /// Audio files.
    Music,
    /// Video files.
    Videos,
    /// User-managed working directories.
    Projects,
    /// Explicitly shared resources.
    Shared,
    /// Installed applications and their user-visible documents.
    Applications,
    /// External and removable storage.
    Devices,
    /// Recoverable deleted user files.
    Trash,
}

/// Sidebar order. It is identical for browsing, Open, Save, and portals.
pub const LOCATIONS: [Location; 13] = [
    Location::Home,
    Location::Recent,
    Location::Favorites,
    Location::Documents,
    Location::Downloads,
    Location::Pictures,
    Location::Music,
    Location::Videos,
    Location::Projects,
    Location::Shared,
    Location::Applications,
    Location::Devices,
    Location::Trash,
];

impl Location {
    /// User-facing label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Recent => "Recent",
            Self::Favorites => "Favorites",
            Self::Documents => "Documents",
            Self::Downloads => "Downloads",
            Self::Pictures => "Pictures",
            Self::Music => "Music",
            Self::Videos => "Videos",
            Self::Projects => "Projects",
            Self::Shared => "Shared",
            Self::Applications => "Applications",
            Self::Devices => "Devices",
            Self::Trash => "Trash",
        }
    }

    /// Logical namespace path. These are resource-provider locations, not a
    /// promise that every backend is a conventional physical directory.
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            Self::Home => "/home",
            Self::Recent => "/home/Recent",
            Self::Favorites => "/home/Favorites",
            Self::Documents => "/home/Documents",
            Self::Downloads => "/home/Downloads",
            Self::Pictures => "/home/Pictures",
            Self::Music => "/home/Music",
            Self::Videos => "/home/Videos",
            Self::Projects => "/home/Projects",
            Self::Shared => "/home/Shared",
            Self::Applications => "/system/applications",
            Self::Devices => "/system/devices",
            Self::Trash => "/home/Trash",
        }
    }

    /// Whether deleting an item here is recoverable.
    #[must_use]
    pub const fn recoverable_delete(self) -> bool {
        !matches!(self, Self::Trash | Self::Applications | Self::Devices)
    }
}

#[cfg(test)]
mod tests;

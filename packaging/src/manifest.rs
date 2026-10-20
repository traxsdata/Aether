// SPDX-License-Identifier: Apache-2.0
//! Package manifest structure and value policy.

use crate::MAX_MANIFEST_BYTES;
use crate::text::{FieldError, Fields};
use aether_crypto::{from_hex, to_hex};

/// Maximum identity length.
pub const MAX_ID: usize = 63;
/// Maximum publisher display-name length.
pub const MAX_PUBLISHER: usize = 64;
/// Maximum relative entry-path length.
pub const MAX_PATH: usize = 64;
/// Maximum number of path components.
pub const MAX_PATH_COMPONENTS: usize = 4;
/// Minimum memory quota in KiB.
pub const MIN_MEMORY_KIB: u16 = 64;
/// Maximum memory quota in KiB for the prototype.
pub const MAX_MEMORY_KIB: u16 = 4096;
/// Maximum open files per application.
pub const MAX_OPEN_FILES: u8 = 8;
/// Maximum tasks per application.
pub const MAX_TASKS: u8 = 8;

const KEY_PAYLOAD_DIGEST: &str = "payload.sha256";

const KNOWN_KEYS: [&str; 14] = [
    "payload.sha256",
    "format",
    "id",
    "version",
    "arch",
    "runtime",
    "entry",
    "publisher",
    "publisher-key",
    "channel",
    "permissions",
    "limits.memory_kib",
    "limits.open_files",
    "limits.tasks",
];

/// Permission bits a native package may request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions(u16);

impl Permissions {
    /// Read files inside the application's private root.
    pub const FILE_READ: u16 = 1 << 0;
    /// Write files inside the application's private root.
    pub const FILE_WRITE: u16 = 1 << 1;
    /// Create files inside the application's private root.
    pub const FILE_CREATE: u16 = 1 << 2;
    /// Read files through a portal grant.
    pub const PORTAL_READ: u16 = 1 << 3;
    /// Write files through a portal grant.
    pub const PORTAL_WRITE: u16 = 1 << 4;
    /// Read input events that the session grants.
    pub const INPUT_READ: u16 = 1 << 5;
    /// Compose surfaces in its own window.
    pub const SURFACE_COMPOSE: u16 = 1 << 6;

    /// Known permission names in canonical order.
    pub const NAMES: [(&'static str, u16); 7] = [
        ("file.read", Self::FILE_READ),
        ("file.write", Self::FILE_WRITE),
        ("file.create", Self::FILE_CREATE),
        ("portal.read", Self::PORTAL_READ),
        ("portal.write", Self::PORTAL_WRITE),
        ("input.read", Self::INPUT_READ),
        ("surface.compose", Self::SURFACE_COMPOSE),
    ];

    /// Builds a permission set from a comma-separated list.
    ///
    /// The literal `none` declares an empty set; an empty list is rejected so
    /// a missing value cannot be mistaken for an intentional denial.
    pub fn parse_list(list: &str) -> Result<Self, ManifestError> {
        if list == "none" {
            return Ok(Self(0));
        }
        let mut bits = 0u16;
        for name in list.split(',') {
            let name = name.trim();
            if name.is_empty() {
                return Err(ManifestError::InvalidPermissions);
            }
            let Some((_, bit)) = Self::NAMES.iter().find(|(known, _)| *known == name) else {
                return Err(ManifestError::UnknownPermission);
            };
            if bits & bit != 0 {
                return Err(ManifestError::DuplicatePermission);
            }
            bits |= bit;
        }
        Ok(Self(bits))
    }

    /// Returns the raw permission bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Returns `true` if the set contains `permission`.
    #[must_use]
    pub const fn contains(self, permission: u16) -> bool {
        self.0 & permission == permission
    }

    /// Writes the canonical comma-separated list.
    pub fn write_list(self, out: &mut [u8]) -> Option<usize> {
        if self.0 == 0 {
            out.get_mut(..4)?.copy_from_slice(b"none");
            return Some(4);
        }
        let mut written = 0usize;
        for (name, bit) in Self::NAMES {
            if !self.contains(bit) {
                continue;
            }
            if written != 0 {
                *out.get_mut(written)? = b',';
                written += 1;
            }
            let bytes = name.as_bytes();
            let end = written.checked_add(bytes.len())?;
            out.get_mut(written..end)?.copy_from_slice(bytes);
            written = end;
        }
        Some(written)
    }

    /// Returns the number of permissions.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Returns `true` if no permission is granted.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Resource limits requested by a package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Memory quota in KiB.
    pub memory_kib: u16,
    /// Open file limit.
    pub open_files: u8,
    /// Task limit.
    pub tasks: u8,
}

impl Limits {
    fn validate(self) -> Result<Self, ManifestError> {
        if !(MIN_MEMORY_KIB..=MAX_MEMORY_KIB).contains(&self.memory_kib)
            || !(1..=MAX_OPEN_FILES).contains(&self.open_files)
            || !(1..=MAX_TASKS).contains(&self.tasks)
        {
            return Err(ManifestError::InvalidLimit);
        }
        Ok(self)
    }
}

/// Target architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Architecture {
    /// x86-64, the only implemented target.
    X86_64,
}

impl Architecture {
    /// Canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
        }
    }

    fn parse(value: &str) -> Result<Self, ManifestError> {
        match value {
            "x86_64" => Ok(Self::X86_64),
            _ => Err(ManifestError::UnsupportedArchitecture),
        }
    }
}

/// Application runtime required by the package.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// Native Aether binary.
    Native,
}

impl Runtime {
    /// Canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
        }
    }

    fn parse(value: &str) -> Result<Self, ManifestError> {
        match value {
            "native" => Ok(Self::Native),
            // The compatibility runtime does not exist yet. Refusing the
            // package is honest: installing it would promise an environment
            // the system cannot provide.
            "windows" => Err(ManifestError::UnsupportedRuntime),
            _ => Err(ManifestError::UnknownRuntime),
        }
    }
}

/// Update channel a package belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Developer builds.
    Developer,
    /// Beta channel.
    Beta,
    /// Stable channel.
    Stable,
}

impl Channel {
    /// Canonical name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::Beta => "beta",
            Self::Stable => "stable",
        }
    }

    /// Parses a channel name.
    pub fn parse(value: &str) -> Result<Self, ManifestError> {
        match value {
            "developer" => Ok(Self::Developer),
            "beta" => Ok(Self::Beta),
            "stable" => Ok(Self::Stable),
            _ => Err(ManifestError::UnknownChannel),
        }
    }
}

/// A `MAJOR.MINOR.PATCH` version with numeric ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    /// Major component.
    pub major: u16,
    /// Minor component.
    pub minor: u16,
    /// Patch component.
    pub patch: u16,
}

impl Version {
    /// Creates a version.
    #[must_use]
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses a canonical version string.
    pub fn parse(value: &str) -> Result<Self, ManifestError> {
        let mut parts = value.split('.');
        let major = parse_component(parts.next())?;
        let minor = parse_component(parts.next())?;
        let patch = parse_component(parts.next())?;
        if parts.next().is_some() {
            return Err(ManifestError::InvalidVersion);
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    /// Writes the canonical `MAJOR.MINOR.PATCH` representation.
    pub fn write(self, out: &mut [u8]) -> Option<usize> {
        let mut buffer = [0u8; 5];
        let mut written = 0usize;
        for part in [self.major, self.minor, self.patch] {
            if written != 0 {
                *out.get_mut(written)? = b'.';
                written += 1;
            }
            let text = decimal(part, &mut buffer);
            let end = written.checked_add(text.len())?;
            out.get_mut(written..end)?.copy_from_slice(text);
            written = end;
        }
        Some(written)
    }
}

fn parse_component(value: Option<&str>) -> Result<u16, ManifestError> {
    let value = value.ok_or(ManifestError::InvalidVersion)?;
    if value.is_empty() || value.len() > 5 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ManifestError::InvalidVersion);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(ManifestError::InvalidVersion);
    }
    value.parse().map_err(|_| ManifestError::InvalidVersion)
}

fn decimal(mut value: u16, buffer: &mut [u8; 5]) -> &[u8] {
    let mut index = buffer.len();
    loop {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    &buffer[index..]
}

/// Returns `true` for a stable reverse-domain identity such as
/// `org.aether.filemanager`.
#[must_use]
pub fn is_valid_identity(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID || id.bytes().any(|byte| !is_identity_byte(byte)) {
        return false;
    }
    let mut labels = 0usize;
    for label in id.split('.') {
        if label.is_empty() || label.len() > 20 {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        labels += 1;
    }
    labels >= 2
}

fn is_identity_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'.' || byte == b'-'
}

/// Returns `true` for a relative path with bounded, safe components.
#[must_use]
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH || path.bytes().any(|byte| !is_path_byte(byte)) {
        return false;
    }
    let mut components = 0usize;
    for component in path.split('/') {
        if component.is_empty()
            || component.len() > crate::MAX_FILE_NAME
            || component == "."
            || component == ".."
        {
            return false;
        }
        components += 1;
    }
    (1..=MAX_PATH_COMPONENTS).contains(&components)
}

fn is_path_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase()
        || byte.is_ascii_digit()
        || byte == b'.'
        || byte == b'-'
        || byte == b'_'
        || byte == b'/'
}

/// Manifest validation errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// Field syntax or table error.
    Field(FieldError),
    /// `format` is not a supported package format version.
    UnsupportedFormat,
    /// Identity is not a valid reverse-domain name.
    InvalidIdentity,
    /// Version is not `MAJOR.MINOR.PATCH`.
    InvalidVersion,
    /// Architecture is not supported.
    UnsupportedArchitecture,
    /// Runtime name is unknown.
    UnknownRuntime,
    /// Runtime is known but not implemented. Installing would be dishonest.
    UnsupportedRuntime,
    /// Channel is unknown.
    UnknownChannel,
    /// Entry path is not a safe relative path.
    InvalidEntry,
    /// Publisher display name is empty or too long.
    InvalidPublisher,
    /// Publisher key is not a 32-byte hexadecimal value.
    InvalidPublisherKey,
    /// Permission list is malformed.
    InvalidPermissions,
    /// Permission is not defined.
    UnknownPermission,
    /// Permission appears twice.
    DuplicatePermission,
    /// A resource limit is outside the policy range.
    InvalidLimit,
    /// Payload digest is missing or malformed.
    InvalidDigest,
}

impl From<FieldError> for ManifestError {
    fn from(error: FieldError) -> Self {
        Self::Field(error)
    }
}

/// Validated manifest fields, optionally without a payload digest.
///
/// Build tooling uses this to parse a source manifest, compute the payload
/// digest, then [`build`](Self::build) the canonical manifest that is signed.
#[derive(Debug)]
pub struct ManifestFields<'a> {
    id: &'a str,
    version: Version,
    architecture: Architecture,
    runtime: Runtime,
    entry: &'a str,
    publisher: &'a str,
    publisher_key: [u8; 32],
    channel: Channel,
    permissions: Permissions,
    limits: Limits,
    payload_digest: Option<[u8; 32]>,
}

impl<'a> ManifestFields<'a> {
    /// Parses and validates manifest fields.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ManifestError> {
        let fields = Fields::parse(bytes, MAX_MANIFEST_BYTES)?;
        fields.reject_unknown(&KNOWN_KEYS)?;

        if fields.require("format")? != "1" {
            return Err(ManifestError::UnsupportedFormat);
        }
        let id = fields.require("id")?;
        if !is_valid_identity(id) {
            return Err(ManifestError::InvalidIdentity);
        }
        let version = Version::parse(fields.require("version")?)?;
        let architecture = Architecture::parse(fields.require("arch")?)?;
        let runtime = Runtime::parse(fields.require("runtime")?)?;
        let entry = fields.require("entry")?;
        if !is_safe_relative_path(entry) {
            return Err(ManifestError::InvalidEntry);
        }
        let publisher = fields.require("publisher")?;
        if publisher.is_empty() || publisher.len() > MAX_PUBLISHER {
            return Err(ManifestError::InvalidPublisher);
        }
        let key_text = fields.require("publisher-key")?.as_bytes();
        let mut publisher_key = [0u8; 32];
        if from_hex(key_text, &mut publisher_key) != Some(32) {
            return Err(ManifestError::InvalidPublisherKey);
        }
        let channel = Channel::parse(fields.require("channel")?)?;
        let permissions = Permissions::parse_list(fields.require("permissions")?)?;
        let limits = Limits {
            memory_kib: parse_limit(fields.require("limits.memory_kib")?)?,
            open_files: parse_limit(fields.require("limits.open_files")?)?,
            tasks: parse_limit(fields.require("limits.tasks")?)?,
        }
        .validate()?;

        let payload_digest = match fields.get(KEY_PAYLOAD_DIGEST) {
            Some(text) => {
                let mut digest = [0u8; 32];
                if from_hex(text.as_bytes(), &mut digest) != Some(32) {
                    return Err(ManifestError::InvalidDigest);
                }
                Some(digest)
            }
            None => None,
        };

        Ok(Self {
            id,
            version,
            architecture,
            runtime,
            entry,
            publisher,
            publisher_key,
            channel,
            permissions,
            limits,
            payload_digest,
        })
    }

    /// Returns the digest declared by the manifest, if present.
    #[must_use]
    pub const fn payload_digest(&self) -> Option<[u8; 32]> {
        self.payload_digest
    }

    /// Builds a manifest with `digest` as the payload digest.
    ///
    /// Build tooling passes the computed digest; [`Manifest::parse`] passes the
    /// digest declared in the signed manifest so a mismatch is detected.
    pub fn build(self, digest: [u8; 32]) -> Result<Manifest<'a>, ManifestError> {
        if let Some(declared) = self.payload_digest
            && declared != digest
        {
            return Err(ManifestError::InvalidDigest);
        }
        Ok(Manifest {
            id: self.id,
            version: self.version,
            architecture: self.architecture,
            runtime: self.runtime,
            entry: self.entry,
            publisher: self.publisher,
            publisher_key: self.publisher_key,
            channel: self.channel,
            permissions: self.permissions,
            limits: self.limits,
            payload_digest: digest,
        })
    }
}

fn parse_limit<T: core::str::FromStr>(value: &str) -> Result<T, ManifestError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ManifestError::InvalidLimit);
    }
    value.parse().map_err(|_| ManifestError::InvalidLimit)
}

/// A validated package manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest<'a> {
    /// Stable reverse-domain identity.
    pub id: &'a str,
    /// Application version.
    pub version: Version,
    /// Target architecture.
    pub architecture: Architecture,
    /// Required runtime.
    pub runtime: Runtime,
    /// Entry point inside the payload.
    pub entry: &'a str,
    /// Publisher display name.
    pub publisher: &'a str,
    /// Publisher public key.
    pub publisher_key: [u8; 32],
    /// Update channel.
    pub channel: Channel,
    /// Requested permissions.
    pub permissions: Permissions,
    /// Requested resource limits.
    pub limits: Limits,
    /// SHA-256 of the payload.
    pub payload_digest: [u8; 32],
}

impl<'a> Manifest<'a> {
    /// Parses and validates a manifest, requiring a payload digest.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ManifestError> {
        let fields = ManifestFields::parse(bytes)?;
        let digest = fields
            .payload_digest()
            .ok_or(ManifestError::InvalidDigest)?;
        fields.build(digest)
    }

    /// Writes the canonical manifest text.
    ///
    /// The output is exactly what the verifier parses and the signature
    /// covers, so build tools must use this method rather than hand-formatting
    /// fields.
    pub fn render(&self, out: &mut [u8]) -> Option<usize> {
        let mut writer = Writer::new(out);
        writer.field("format", "1")?;
        writer.field("id", self.id)?;
        let mut version = [0u8; 24];
        let length = self.version.write(&mut version)?;
        writer.field("version", core::str::from_utf8(&version[..length]).ok()?)?;
        writer.field("arch", self.architecture.name())?;
        writer.field("runtime", self.runtime.name())?;
        writer.field("entry", self.entry)?;
        writer.field("publisher", self.publisher)?;
        let mut key = [0u8; 64];
        let length = to_hex(&self.publisher_key, &mut key)?;
        writer.field("publisher-key", core::str::from_utf8(&key[..length]).ok()?)?;
        writer.field("channel", self.channel.name())?;
        let mut permissions = [0u8; 128];
        let length = self.permissions.write_list(&mut permissions)?;
        writer.field(
            "permissions",
            core::str::from_utf8(&permissions[..length]).ok()?,
        )?;
        let mut memory = [0u8; 5];
        writer.field(
            "limits.memory_kib",
            core::str::from_utf8(decimal(self.limits.memory_kib, &mut memory)).ok()?,
        )?;
        let mut open = [0u8; 5];
        let open_text = decimal(u16::from(self.limits.open_files), &mut open);
        writer.field("limits.open_files", core::str::from_utf8(open_text).ok()?)?;
        let mut tasks = [0u8; 5];
        let tasks_text = decimal(u16::from(self.limits.tasks), &mut tasks);
        writer.field("limits.tasks", core::str::from_utf8(tasks_text).ok()?)?;
        let mut digest = [0u8; 64];
        let length = to_hex(&self.payload_digest, &mut digest)?;
        writer.field(
            "payload.sha256",
            core::str::from_utf8(&digest[..length]).ok()?,
        )?;
        Some(writer.written)
    }
}

struct Writer<'a> {
    out: &'a mut [u8],
    written: usize,
}

impl<'a> Writer<'a> {
    fn new(out: &'a mut [u8]) -> Self {
        Self { out, written: 0 }
    }

    fn field(&mut self, key: &str, value: &str) -> Option<()> {
        let line = self.written.checked_add(key.len() + 1 + value.len() + 1)?;
        let slice = self.out.get_mut(self.written..line)?;
        slice[..key.len()].copy_from_slice(key.as_bytes());
        slice[key.len()] = b'=';
        slice[key.len() + 1..key.len() + 1 + value.len()].copy_from_slice(value.as_bytes());
        slice[key.len() + 1 + value.len()] = b'\n';
        self.written = line;
        Some(())
    }
}

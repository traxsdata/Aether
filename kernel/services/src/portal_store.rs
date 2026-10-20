// SPDX-License-Identifier: Apache-2.0
//! Persistent file-portal grant records.
//!
//! The record format is deliberately small, textual, and allocation-free so
//! the kernel can load it from the filesystem during boot and rewrite it after
//! a consent or revocation decision. A record names exactly one root, one
//! path, and a right set; the parser never widens a grant and rejects
//! malformed, duplicated, or out-of-scope records instead of skipping them.

/// Maximum stored grants.
pub const MAX_GRANTS: usize = 8;
/// Maximum bytes in a process root.
pub const MAX_ROOT_BYTES: usize = 64;
/// Maximum bytes in a portal path.
pub const MAX_PATH_BYTES: usize = 128;
/// Linux-visible namespace that fronts the exact-file grant table.
pub const PORTAL_PREFIX: &str = "/run/aether/portal/";
/// A grant id is sixteen lowercase hexadecimal digits.
pub const GRANT_ID_BYTES: usize = 16;

/// One exact-file portal grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalGrant {
    root: [u8; MAX_ROOT_BYTES],
    root_len: u8,
    path: [u8; MAX_PATH_BYTES],
    path_len: u8,
    /// File rights bit set (`READ` = 1, `WRITE` = 2).
    pub rights: u64,
}

impl PortalGrant {
    /// An empty grant record.
    pub const EMPTY: Self = Self {
        root: [0; MAX_ROOT_BYTES],
        root_len: 0,
        path: [0; MAX_PATH_BYTES],
        path_len: 0,
        rights: 0,
    };

    /// Creates a validated grant.
    ///
    /// `root` is an absolute directory path without a trailing slash, `path`
    /// is an absolute file path, and `rights` must contain at least one of
    /// the defined file rights and nothing else.
    pub fn new(root: &str, path: &str, rights: u64) -> Option<Self> {
        if !valid_root(root) || !valid_path(path) || !valid_rights(rights) {
            return None;
        }
        let rights = rights & (crate::portal_store::READ | crate::portal_store::WRITE);
        let mut grant = Self::EMPTY;
        grant.root[..root.len()].copy_from_slice(root.as_bytes());
        grant.root_len = root.len() as u8;
        grant.path[..path.len()].copy_from_slice(path.as_bytes());
        grant.path_len = path.len() as u8;
        grant.rights = rights;
        Some(grant)
    }

    /// Process root named by this grant.
    #[must_use]
    pub fn root(&self) -> &str {
        core::str::from_utf8(&self.root[..usize::from(self.root_len)]).unwrap_or("")
    }

    /// File path named by this grant.
    #[must_use]
    pub fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..usize::from(self.path_len)]).unwrap_or("")
    }

    /// Returns `true` when the record is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root_len == 0 || self.path_len == 0
    }

    /// Checks an exact root/path/right match.
    #[must_use]
    pub fn admits(&self, root: &str, path: &str, rights: u64) -> bool {
        !self.is_empty()
            && self.root() == root
            && self.path() == path
            && rights != 0
            && (self.rights & rights) == rights
    }

    /// Writes one `root<TAB>path<TAB>rights` line and returns its length.
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        if self.is_empty() || !valid_rights(self.rights) {
            return None;
        }
        let mut written = 0usize;
        for field in [self.root(), self.path()] {
            let end = written.checked_add(field.len())?;
            out.get_mut(written..end)?.copy_from_slice(field.as_bytes());
            written = end;
            *out.get_mut(written)? = b'\t';
            written += 1;
        }
        let digits = decimal(self.rights, &mut out[written..])?;
        written += digits;
        *out.get_mut(written)? = b'\n';
        Some(written + 1)
    }

    /// Parses one line. Unknown or unsafe rights are rejected.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let root = fields.next()?;
        let path = fields.next()?;
        let rights = fields.next()?.parse::<u64>().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Self::new(root, path, rights)
    }
}

/// A parsed Linux portal namespace path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalPath<'a> {
    /// Stable id derived from the grant's root and backing path.
    pub grant_id: u64,
    /// Final file component named by the namespace path.
    pub file: &'a str,
}

/// Portal namespace resolution failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortalPathError {
    /// The path is not exactly `/run/aether/portal/<id>/<file>`.
    Malformed,
    /// No active grant matches the caller, grant id, and file.
    NotGranted,
    /// The matching grant was revoked.
    Revoked,
    /// The grant exists but does not contain the requested rights.
    Rights,
}

/// One live grant-table slot, including its revocation tombstone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalEntry {
    /// Stored grant record.
    pub grant: PortalGrant,
    /// Whether the user revoked this record.
    pub revoked: bool,
}

impl PortalEntry {
    /// An empty slot.
    pub const EMPTY: Self = Self {
        grant: PortalGrant::EMPTY,
        revoked: false,
    };
}

/// Derives the stable grant id used in the Linux portal namespace.
///
/// The id is FNV-1a over `root`, a NUL separator, and the backing path. It does
/// not depend on table order, so a tombstone or a compaction cannot silently
/// point the same namespace path at a different file.
#[must_use]
pub fn grant_id(grant: &PortalGrant) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in grant
        .root()
        .bytes()
        .chain(core::iter::once(0))
        .chain(grant.path().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Writes a grant id as sixteen lowercase hexadecimal digits.
pub fn write_grant_id(id: u64, out: &mut [u8; GRANT_ID_BYTES]) -> &str {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (index, byte) in out.iter_mut().enumerate() {
        let shift = (GRANT_ID_BYTES - 1 - index) * 4;
        *byte = HEX[((id >> shift) & 0xf) as usize];
    }
    // Every byte was written from the hexadecimal table.
    core::str::from_utf8(out).unwrap_or("")
}

/// Parses the exact Linux namespace path.
///
/// Duplicate slashes, `.`/`..`, trailing slashes, extra components, overlong
/// names, and non-lowercase ids are rejected. There is deliberately no
/// normalization step: a path that is not already canonical grants nothing.
pub fn parse_namespace(path: &str) -> Result<PortalPath<'_>, PortalPathError> {
    let rest = path
        .strip_prefix(PORTAL_PREFIX)
        .ok_or(PortalPathError::Malformed)?;
    let (id_text, file) = rest.split_once('/').ok_or(PortalPathError::Malformed)?;
    if id_text.len() != GRANT_ID_BYTES
        || file.is_empty()
        || file.len() > MAX_PATH_BYTES
        || file.contains('/')
        || file == "."
        || file == ".."
        || file.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(PortalPathError::Malformed);
    }
    let mut id = 0u64;
    for byte in id_text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return Err(PortalPathError::Malformed),
        };
        id = (id << 4) | u64::from(digit);
    }
    Ok(PortalPath { grant_id: id, file })
}

/// Resolves a namespace path against the live grant table.
///
/// The caller root is always matched exactly. Directory grants do not exist,
/// so the namespace file must equal the grant's final path component. A
/// revoked matching record fails closed instead of falling through to another
/// grant with the same id.
pub fn resolve(
    entries: &[PortalEntry],
    root: &str,
    namespace: &str,
    rights: u64,
) -> Result<PortalGrant, PortalPathError> {
    let parsed = parse_namespace(namespace)?;
    let mut matched = None;
    let mut saw_revoked = false;
    for entry in entries {
        let grant = entry.grant;
        if grant.is_empty()
            || grant.root() != root
            || grant_id(&grant) != parsed.grant_id
            || grant.path().rsplit('/').next() != Some(parsed.file)
        {
            continue;
        }
        if rights == 0 || (grant.rights & rights) != rights {
            if entry.revoked {
                saw_revoked = true;
            } else {
                return Err(PortalPathError::Rights);
            }
            continue;
        }
        if entry.revoked {
            saw_revoked = true;
            continue;
        }
        if matched.is_some() {
            // Two active records produce the same namespace path. Refuse to
            // guess rather than widening either grant.
            return Err(PortalPathError::Malformed);
        }
        matched = Some(grant);
    }
    if let Some(grant) = matched {
        Ok(grant)
    } else if saw_revoked {
        Err(PortalPathError::Revoked)
    } else {
        Err(PortalPathError::NotGranted)
    }
}

/// `READ` file right in the on-disk record.
pub const READ: u64 = 1 << 0;
/// `WRITE` file right in the on-disk record.
pub const WRITE: u64 = 1 << 1;

/// Encodes up to `grants.len()` records. Returns the byte count.
pub fn encode(grants: &[PortalGrant], out: &mut [u8]) -> Option<usize> {
    let mut written = 0usize;
    for grant in grants {
        if grant.is_empty() {
            continue;
        }
        let length = grant.encode(out.get_mut(written..)?)?;
        written += length;
    }
    Some(written)
}

/// Parses a complete grant file into `out`, returning the number of records.
///
/// An empty file is valid and yields zero grants. Any malformed or duplicate
/// line rejects the whole file, so a partially corrupted store cannot silently
/// drop an existing grant and reopen a path that the user did not revoke.
pub fn parse(data: &[u8], out: &mut [PortalGrant]) -> Result<usize, ParseError> {
    let text = core::str::from_utf8(data).map_err(|_| ParseError::Invalid)?;
    let mut count = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let grant = PortalGrant::parse(line).ok_or(ParseError::Invalid)?;
        if count == out.len() || count == MAX_GRANTS {
            return Err(ParseError::TooMany);
        }
        if out[..count]
            .iter()
            .any(|existing| existing.root() == grant.root() && existing.path() == grant.path())
        {
            return Err(ParseError::Duplicate);
        }
        out[count] = grant;
        count += 1;
    }
    Ok(count)
}

/// Grant-store parse failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The file is not UTF-8 or a line is malformed.
    Invalid,
    /// The same root and path appear twice.
    Duplicate,
    /// The file exceeds the bounded record count.
    TooMany,
}

fn valid_rights(rights: u64) -> bool {
    rights & !(READ | WRITE) == 0 && rights != 0
}

fn valid_root(root: &str) -> bool {
    !root.is_empty()
        && root.len() <= MAX_ROOT_BYTES
        && root.starts_with('/')
        && (root == "/" || !root.ends_with('/'))
        && valid_components(root)
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && path.starts_with('/')
        && path != "/"
        && !path.ends_with('/')
        && valid_components(path)
}

fn valid_components(path: &str) -> bool {
    path[1..].split('/').all(|component| {
        !component.is_empty()
            && component != "."
            && component != ".."
            && !component.bytes().any(|byte| byte.is_ascii_control())
    })
}

fn decimal(mut value: u64, out: &mut [u8]) -> Option<usize> {
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    if value == 0 {
        *out.first_mut()? = b'0';
        return Some(1);
    }
    while value > 0 {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    let length = digits.len() - index;
    out.get_mut(..length)?.copy_from_slice(&digits[index..]);
    Some(length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_parses_round_trip() {
        let mut grants = [PortalGrant::EMPTY; MAX_GRANTS];
        grants[0] =
            PortalGrant::new("/apps/org.aether.demo", "/home/welcome.txt", READ).expect("grant");
        grants[1] =
            PortalGrant::new("/apps/other", "/home/outbox.txt", READ | WRITE).expect("grant");
        let mut encoded = [0u8; 512];
        let length = encode(&grants, &mut encoded).expect("encode");
        assert!(encoded[..length].ends_with(b"\n"));

        let mut parsed = [PortalGrant::EMPTY; MAX_GRANTS];
        let count = parse(&encoded[..length], &mut parsed).expect("parse");
        assert_eq!(count, 2);
        assert_eq!(parsed[0], grants[0]);
        assert_eq!(parsed[1], grants[1]);
        assert!(parsed[1].admits("/apps/other", "/home/outbox.txt", WRITE));
        assert!(!parsed[1].admits("/apps/other", "/home/outbox.txt", READ | 4));
    }

    #[test]
    fn rejects_traversal_control_bytes_and_unknown_rights() {
        for grant in [
            PortalGrant::new("../apps", "/home/a", READ),
            PortalGrant::new("/apps", "/home/../system", READ),
            PortalGrant::new("/apps", "home/a", READ),
            PortalGrant::new("/apps", "/home/a", 0),
            PortalGrant::new("/apps", "/home/a", 4),
            PortalGrant::new("/apps", "/home/a\nb", READ),
        ] {
            assert!(grant.is_none(), "{grant:?}");
        }
    }

    #[test]
    fn malformed_duplicate_and_oversized_stores_are_rejected() {
        let mut out = [PortalGrant::EMPTY; MAX_GRANTS];
        assert_eq!(parse(b"/apps\t/home/a\t1\n", &mut out), Ok(1));
        assert_eq!(
            parse(b"/apps\t/home/a\t1\n/apps\t/home/a\t2\n", &mut out),
            Err(ParseError::Duplicate)
        );
        assert_eq!(
            parse(b"/apps\t/home/a\t1\textra\n", &mut out),
            Err(ParseError::Invalid)
        );
        assert_eq!(parse(b"/apps\t/home/a\t1", &mut out), Ok(1));
        assert_eq!(parse(b"\xff", &mut out), Err(ParseError::Invalid));
        let mut small = [PortalGrant::EMPTY; 1];
        assert_eq!(
            parse(b"/apps\t/home/a\t1\n/apps\t/home/b\t1\n", &mut small),
            Err(ParseError::TooMany)
        );
        // An empty store is a valid, empty grant table.
        assert_eq!(parse(b"", &mut out), Ok(0));
    }

    #[test]
    fn insert_order_is_preserved_and_revoked_records_are_not_encoded() {
        let grant = PortalGrant::new("/apps", "/home/a", READ).expect("grant");
        let mut out = [0u8; 128];
        let length = grant.encode(&mut out).expect("encode");
        assert_eq!(&out[..length], b"/apps\t/home/a\t1\n");
        assert_eq!(encode(&[PortalGrant::EMPTY], &mut out), Some(0));
    }

    #[test]
    fn grant_id_is_stable_and_namespace_paths_are_strict() {
        let grant = PortalGrant::new("/apps/org.aether.demo", "/home/gate4.xlsx", READ | WRITE)
            .expect("grant");
        let mut id = [0u8; GRANT_ID_BYTES];
        let id = write_grant_id(grant_id(&grant), &mut id);
        assert_eq!(id, "dd8f879ba78d207c");
        let path = "/run/aether/portal/dd8f879ba78d207c/gate4.xlsx";
        assert_eq!(
            parse_namespace(path),
            Ok(PortalPath {
                grant_id: grant_id(&grant),
                file: "gate4.xlsx",
            })
        );
        for rejected in [
            "/run/aether/portal/dd8f879ba78d207c/",
            "/run/aether/portal//gate4.xlsx",
            "/run/aether/portal/dd8f879ba78d207c/../gate4.xlsx",
            "/run/aether/portal/dd8f879ba78d207c/sub/gate4.xlsx",
            "/run/aether/portal/DD8F879BA78D207C/gate4.xlsx",
            "/run/aether/portal/dd8f879ba78d207/gate4.xlsx",
            "/home/gate4.xlsx",
        ] {
            assert_eq!(parse_namespace(rejected), Err(PortalPathError::Malformed));
        }
        let mut long = [b'a'; MAX_PATH_BYTES + 2];
        long[MAX_PATH_BYTES + 1] = 0;
        let long = core::str::from_utf8(&long[..MAX_PATH_BYTES + 1]).expect("utf8");
        let mut path = [0u8; PORTAL_PREFIX.len() + GRANT_ID_BYTES + 1 + MAX_PATH_BYTES + 1];
        let mut length = 0usize;
        for part in [PORTAL_PREFIX, "dd8f879ba78d207c", "/", long] {
            path[length..length + part.len()].copy_from_slice(part.as_bytes());
            length += part.len();
        }
        let path = core::str::from_utf8(&path[..length]).expect("utf8");
        assert_eq!(parse_namespace(path), Err(PortalPathError::Malformed));
    }

    #[test]
    fn resolves_exact_root_file_rights_and_revocation() {
        let grant = PortalGrant::new("/apps/org.aether.demo", "/home/gate4.xlsx", READ | WRITE)
            .expect("grant");
        let mut id = [0u8; GRANT_ID_BYTES];
        let id = write_grant_id(grant_id(&grant), &mut id);
        let mut namespace = [0u8; 128];
        let mut length = 0usize;
        for part in [PORTAL_PREFIX, id, "/gate4.xlsx"] {
            namespace[length..length + part.len()].copy_from_slice(part.as_bytes());
            length += part.len();
        }
        let namespace = core::str::from_utf8(&namespace[..length]).expect("utf8");
        let entries = [PortalEntry {
            grant,
            revoked: false,
        }];
        assert_eq!(
            resolve(&entries, "/apps/org.aether.demo", namespace, READ),
            Ok(grant)
        );
        assert_eq!(
            resolve(&entries, "/apps/org.aether.demo", namespace, READ | WRITE),
            Ok(grant)
        );
        assert_eq!(
            resolve(&entries, "/apps/other", namespace, READ),
            Err(PortalPathError::NotGranted)
        );
        assert_eq!(
            resolve(&entries, "/apps/org.aether.demo", namespace, 4),
            Err(PortalPathError::Rights)
        );
        let mut wrong_file = [0u8; 128];
        let wrong = namespace.replace("gate4.xlsx", "other.xlsx");
        wrong_file[..wrong.len()].copy_from_slice(wrong.as_bytes());
        let wrong = core::str::from_utf8(&wrong_file[..wrong.len()]).expect("utf8");
        assert_eq!(
            resolve(&entries, "/apps/org.aether.demo", wrong, READ),
            Err(PortalPathError::NotGranted)
        );
        let revoked = [PortalEntry {
            grant,
            revoked: true,
        }];
        assert_eq!(
            resolve(&revoked, "/apps/org.aether.demo", namespace, READ),
            Err(PortalPathError::Revoked)
        );
        assert_eq!(
            resolve(&revoked, "/apps/org.aether.demo", namespace, WRITE),
            Err(PortalPathError::Revoked)
        );
    }
}

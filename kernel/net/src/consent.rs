// SPDX-License-Identifier: Apache-2.0
//! Per-application network decisions.
//!
//! The record file holds one `<app-id>=allow` or `<app-id>=deny` line per
//! application. An application with no line has not been asked yet. The
//! parser fails closed: a malformed file yields an error and the kernel
//! treats every application as not yet decided, so it asks again rather than
//! allowing.

/// Maximum decisions kept.
pub const MAX_DECISIONS: usize = 32;
/// Maximum application identity length.
pub const MAX_ID: usize = 96;

/// The user's answer for one application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Malformed record file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentError {
    Malformed,
    Full,
}

#[derive(Clone, Copy)]
struct Entry {
    id: [u8; MAX_ID],
    len: u8,
    decision: Decision,
}

/// The decision table.
#[derive(Clone, Copy)]
pub struct Decisions {
    entries: [Option<Entry>; MAX_DECISIONS],
}

impl Default for Decisions {
    fn default() -> Self {
        Self::new()
    }
}

impl Decisions {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_DECISIONS],
        }
    }

    /// Parses a record file.
    pub fn parse(text: &[u8]) -> Result<Self, ConsentError> {
        let text = core::str::from_utf8(text).map_err(|_| ConsentError::Malformed)?;
        let mut decisions = Self::new();
        for line in text.split('\n') {
            if line.is_empty() {
                continue;
            }
            let (id, value) = line.split_once('=').ok_or(ConsentError::Malformed)?;
            let decision = match value {
                "allow" => Decision::Allow,
                "deny" => Decision::Deny,
                _ => return Err(ConsentError::Malformed),
            };
            if !valid_id(id) || decisions.get(id).is_some() {
                return Err(ConsentError::Malformed);
            }
            decisions.set(id, decision)?;
        }
        Ok(decisions)
    }

    /// Returns the recorded decision, or `None` if the user was not asked.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Decision> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| &entry.id[..usize::from(entry.len)] == id.as_bytes())
            .map(|entry| entry.decision)
    }

    /// Records or replaces a decision.
    pub fn set(&mut self, id: &str, decision: Decision) -> Result<(), ConsentError> {
        if !valid_id(id) {
            return Err(ConsentError::Malformed);
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|entry| &entry.id[..usize::from(entry.len)] == id.as_bytes())
        {
            entry.decision = decision;
            return Ok(());
        }
        let slot = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(ConsentError::Full)?;
        let mut stored = [0u8; MAX_ID];
        stored[..id.len()].copy_from_slice(id.as_bytes());
        *slot = Some(Entry {
            id: stored,
            len: id.len() as u8,
            decision,
        });
        Ok(())
    }

    /// Forgets a decision, so the application is asked again.
    pub fn forget(&mut self, id: &str) {
        for slot in &mut self.entries {
            if slot.is_some_and(|entry| &entry.id[..usize::from(entry.len)] == id.as_bytes()) {
                *slot = None;
            }
        }
    }

    /// Serializes the table and returns the bytes written, or `None` if
    /// `out` is too small.
    pub fn write(&self, out: &mut [u8]) -> Option<usize> {
        let mut used = 0usize;
        for entry in self.entries.iter().flatten() {
            let value: &[u8] = match entry.decision {
                Decision::Allow => b"allow",
                Decision::Deny => b"deny",
            };
            let id = &entry.id[..usize::from(entry.len)];
            let end = used + id.len() + 1 + value.len() + 1;
            let line = out.get_mut(used..end)?;
            line[..id.len()].copy_from_slice(id);
            line[id.len()] = b'=';
            line[id.len() + 1..id.len() + 1 + value.len()].copy_from_slice(value);
            line[id.len() + 1 + value.len()] = b'\n';
            used = end;
        }
        Some(used)
    }

    /// Iterates over `(id, decision)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Decision)> {
        self.entries.iter().flatten().filter_map(|entry| {
            core::str::from_utf8(&entry.id[..usize::from(entry.len)])
                .ok()
                .map(|id| (id, entry.decision))
        })
    }
}

/// Application identities are reverse-domain names: lowercase ASCII letters,
/// digits, `.`, `-`, and `_`, not empty and not over [`MAX_ID`].
#[must_use]
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
}

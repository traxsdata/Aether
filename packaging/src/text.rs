// SPDX-License-Identifier: Apache-2.0
//! Bounded `key=value` manifest text.
//!
//! The parser rejects duplicate keys, unknown keys at the caller's request,
//! control characters, and oversized input. Values are printable ASCII so a
//! manifest cannot smuggle terminal escapes into a diagnostic view.

use crate::MAX_FIELDS;

/// Maximum length of one field key.
pub const MAX_KEY: usize = 32;
/// Maximum length of one field value.
pub const MAX_VALUE: usize = 512;

/// Field parsing error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// The manifest is larger than the caller allowed.
    TooLarge,
    /// The manifest is not valid UTF-8.
    NotUtf8,
    /// A line is not `key=value`.
    Malformed,
    /// A key or value is empty or contains a forbidden character.
    InvalidCharacter,
    /// The same key appears more than once.
    DuplicateKey,
    /// A key the caller did not declare.
    UnknownKey,
    /// More fields than the fixed table holds.
    TooManyFields,
    /// A required key is missing.
    MissingKey,
}

/// Fields from one manifest, borrowing the input text.
#[derive(Debug)]
pub struct Fields<'a> {
    entries: [Option<(&'a str, &'a str)>; MAX_FIELDS],
    len: usize,
}

impl<'a> Fields<'a> {
    /// Parses `bytes` into at most [`MAX_FIELDS`] unique fields.
    pub fn parse(bytes: &'a [u8], max_bytes: usize) -> Result<Self, FieldError> {
        if bytes.len() > max_bytes {
            return Err(FieldError::TooLarge);
        }
        let text = core::str::from_utf8(bytes).map_err(|_| FieldError::NotUtf8)?;
        let mut fields = Self {
            entries: [None; MAX_FIELDS],
            len: 0,
        };
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(FieldError::Malformed);
            };
            if key.is_empty()
                || key.len() > MAX_KEY
                || value.is_empty()
                || value.len() > MAX_VALUE
                || !key.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'.'
                        || byte == b'-'
                        || byte == b'_'
                })
                || !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            {
                return Err(FieldError::InvalidCharacter);
            }
            if fields.get(key).is_some() {
                return Err(FieldError::DuplicateKey);
            }
            let slot = fields
                .entries
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(FieldError::TooManyFields)?;
            *slot = Some((key, value));
            fields.len += 1;
        }
        Ok(fields)
    }

    /// Returns a field value.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .flatten()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    }

    /// Returns a required field value.
    pub fn require(&self, key: &str) -> Result<&'a str, FieldError> {
        self.get(key).ok_or(FieldError::MissingKey)
    }

    /// Rejects any field not listed in `known`.
    pub fn reject_unknown(&self, known: &[&str]) -> Result<(), FieldError> {
        for (name, _) in self.entries.iter().flatten() {
            if !known.contains(name) {
                return Err(FieldError::UnknownKey);
            }
        }
        Ok(())
    }

    /// Number of fields.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if there are no fields.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

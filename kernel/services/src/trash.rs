// SPDX-License-Identifier: Apache-2.0
//! Persistent metadata for recoverable user deletion.
//!
//! Moving a file into Trash is not enough to restore it: the service must also
//! remember where it came from. This module is the bounded, host-tested codec
//! and table for that metadata. It deliberately stores no file contents and
//! never grants access; the filesystem service performs the move only after
//! validating the recorded destination again.
//!
//! Records use one textual line per item:
//!
//! ```text
//! <trash-name>\t<original-absolute-path>\n
//! ```
//!
//! A malformed, duplicated, traversal-bearing, or oversized record rejects the
//! whole store rather than restoring an item to a guessed location.

/// Maximum records retained in the Trash index.
pub const MAX_TRASH_RECORDS: usize = 16;
/// Maximum bytes in a Trash component.
pub const MAX_TRASH_NAME: usize = 16;
/// Maximum bytes in an original absolute path.
pub const MAX_TRASH_PATH: usize = 128;

/// One recoverable deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrashRecord {
    name: [u8; MAX_TRASH_NAME],
    name_len: u8,
    original: [u8; MAX_TRASH_PATH],
    original_len: u8,
}

impl TrashRecord {
    /// An empty slot.
    pub const EMPTY: Self = Self {
        name: [0; MAX_TRASH_NAME],
        name_len: 0,
        original: [0; MAX_TRASH_PATH],
        original_len: 0,
    };

    /// Creates a validated record.
    ///
    /// `name` is the final component inside Trash. `original` is the absolute
    /// path the item occupied before deletion.
    #[must_use]
    pub fn new(name: &str, original: &str) -> Option<Self> {
        if !valid_name(name) || !valid_original(original) {
            return None;
        }
        let mut record = Self::EMPTY;
        record.name[..name.len()].copy_from_slice(name.as_bytes());
        record.name_len = name.len() as u8;
        record.original[..original.len()].copy_from_slice(original.as_bytes());
        record.original_len = original.len() as u8;
        Some(record)
    }

    /// Trash component.
    #[must_use]
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..usize::from(self.name_len)]).unwrap_or("")
    }

    /// Original absolute path.
    #[must_use]
    pub fn original(&self) -> &str {
        core::str::from_utf8(&self.original[..usize::from(self.original_len)]).unwrap_or("")
    }

    /// Returns `true` for an empty slot.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name_len == 0 || self.original_len == 0
    }

    /// Encodes one line and returns its length.
    pub fn encode(&self, out: &mut [u8]) -> Option<usize> {
        if self.is_empty() || !valid_name(self.name()) || !valid_original(self.original()) {
            return None;
        }
        let mut written = 0usize;
        for field in [self.name(), self.original()] {
            let end = written.checked_add(field.len())?;
            out.get_mut(written..end)?.copy_from_slice(field.as_bytes());
            written = end;
            *out.get_mut(written)? = b'\t';
            written += 1;
        }
        out.get_mut(written - 1)?;
        *out.get_mut(written - 1)? = b'\n';
        Some(written)
    }

    /// Parses one line.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let name = fields.next()?;
        let original = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        Self::new(name, original)
    }
}

/// Bounded Trash metadata table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrashIndex {
    records: [TrashRecord; MAX_TRASH_RECORDS],
}

impl TrashIndex {
    /// Creates an empty index.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: [TrashRecord::EMPTY; MAX_TRASH_RECORDS],
        }
    }

    /// Records a deletion. Re-recording the same Trash name replaces its
    /// original path only when the caller explicitly asks to update it.
    pub fn record(&mut self, name: &str, original: &str) -> Result<usize, TrashError> {
        let record = TrashRecord::new(name, original).ok_or(TrashError::Invalid)?;
        if let Some(index) = self.index_of(name) {
            self.records[index] = record;
            return Ok(index);
        }
        let index = self
            .records
            .iter()
            .position(TrashRecord::is_empty)
            .ok_or(TrashError::Full)?;
        self.records[index] = record;
        Ok(index)
    }

    /// Returns the original path recorded for a Trash name.
    #[must_use]
    pub fn original(&self, name: &str) -> Option<&str> {
        self.records
            .iter()
            .find(|record| !record.is_empty() && record.name() == name)
            .map(TrashRecord::original)
    }

    /// Removes a record and returns its original path.
    pub fn take(&mut self, name: &str) -> Option<TrashRecord> {
        let index = self.index_of(name)?;
        let record = self.records[index];
        self.records[index] = TrashRecord::EMPTY;
        Some(record)
    }

    /// Restores a previously removed record, e.g. when a move failed.
    pub fn put(&mut self, record: TrashRecord) -> Result<(), TrashError> {
        if record.is_empty() || !valid_name(record.name()) || !valid_original(record.original()) {
            return Err(TrashError::Invalid);
        }
        if let Some(index) = self.index_of(record.name()) {
            self.records[index] = record;
            return Ok(());
        }
        let index = self
            .records
            .iter()
            .position(TrashRecord::is_empty)
            .ok_or(TrashError::Full)?;
        self.records[index] = record;
        Ok(())
    }

    /// Number of recorded items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records
            .iter()
            .filter(|record| !record.is_empty())
            .count()
    }

    /// Returns `true` when no item is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.records
            .iter()
            .position(|record| !record.is_empty() && record.name() == name)
    }
}

impl Default for TrashIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Trash metadata failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrashError {
    /// The name or original path is malformed or out of bounds.
    Invalid,
    /// The bounded table is full.
    Full,
}

/// Encodes the index into `out`, returning the byte count.
///
/// Empty slots are omitted. The output is empty when the index is empty.
pub fn encode(index: &TrashIndex, out: &mut [u8]) -> Option<usize> {
    let mut written = 0usize;
    for record in index.records.iter().filter(|record| !record.is_empty()) {
        let length = record.encode(out.get_mut(written..)?)?;
        written += length;
    }
    Some(written)
}

/// Parses a complete index file into `index`.
///
/// An empty file is valid. Any malformed, duplicate, or oversized record
/// rejects the entire file before it can restore an item to the wrong place.
pub fn parse(data: &[u8], index: &mut TrashIndex) -> Result<usize, TrashError> {
    let text = core::str::from_utf8(data).map_err(|_| TrashError::Invalid)?;
    *index = TrashIndex::new();
    let mut count = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let record = TrashRecord::parse(line).ok_or(TrashError::Invalid)?;
        if count == MAX_TRASH_RECORDS {
            return Err(TrashError::Full);
        }
        if index.original(record.name()).is_some() {
            return Err(TrashError::Invalid);
        }
        index.put(record)?;
        count += 1;
    }
    Ok(count)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TRASH_NAME
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_original(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_TRASH_PATH
        && path.starts_with('/')
        && path != "/"
        && !path.ends_with('/')
        && path[1..]
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !path.bytes().any(|byte| byte.is_ascii_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> TrashIndex {
        TrashIndex::new()
    }

    #[test]
    fn records_round_trip_and_preserve_the_original_path() {
        let mut index = index();
        index
            .record("report.odt", "/home/Documents/report.odt")
            .expect("record");
        index
            .record("budget.csv", "/home/Documents/budget.csv")
            .expect("record");
        assert_eq!(index.len(), 2);
        assert_eq!(
            index.original("report.odt"),
            Some("/home/Documents/report.odt")
        );

        let mut encoded = [0u8; 256];
        let length = encode(&index, &mut encoded).expect("encode");
        let mut parsed = TrashIndex::new();
        assert_eq!(parse(&encoded[..length], &mut parsed), Ok(2));
        assert_eq!(parsed, index);
    }

    #[test]
    fn take_removes_only_the_named_record() {
        let mut index = index();
        index.record("a.txt", "/home/a.txt").expect("a");
        index.record("b.txt", "/home/b.txt").expect("b");
        let record = index.take("a.txt").expect("take");
        assert_eq!(record.original(), "/home/a.txt");
        assert_eq!(index.original("a.txt"), None);
        assert_eq!(index.original("b.txt"), Some("/home/b.txt"));
        assert_eq!(index.take("missing"), None);
    }

    #[test]
    fn a_failed_move_can_put_the_record_back() {
        let mut index = index();
        index.record("a.txt", "/home/a.txt").expect("a");
        let record = index.take("a.txt").expect("take");
        assert!(index.is_empty());
        index.put(record).expect("put");
        assert_eq!(index.original("a.txt"), Some("/home/a.txt"));
    }

    #[test]
    fn traversal_control_and_relative_paths_are_rejected() {
        for (name, original) in [
            ("../a", "/home/a"),
            ("a/b", "/home/a"),
            (".", "/home/a"),
            ("a", "home/a"),
            ("a", "/home/../system/a"),
            ("a", "/home/a/"),
            ("a", "/home//a"),
            ("a", "/home/a\nb"),
            ("", "/home/a"),
        ] {
            assert!(
                TrashRecord::new(name, original).is_none(),
                "{name:?} {original:?}"
            );
        }
    }

    #[test]
    fn duplicate_and_malformed_stores_are_rejected() {
        let mut index = TrashIndex::new();
        assert_eq!(
            parse(b"a.txt\t/home/a.txt\na.txt\t/home/b.txt\n", &mut index),
            Err(TrashError::Invalid)
        );
        assert_eq!(
            parse(b"a.txt\t/home/a.txt\textra\n", &mut index),
            Err(TrashError::Invalid)
        );
        assert_eq!(parse(b"\xff", &mut index), Err(TrashError::Invalid));
        assert_eq!(parse(b"", &mut index), Ok(0));
        assert!(index.is_empty());
    }

    #[test]
    fn table_is_bounded() {
        let mut index = TrashIndex::new();
        for i in 0..MAX_TRASH_RECORDS {
            let name = std::format!("f{i}");
            let original = std::format!("/home/f{i}");
            index.record(&name, &original).expect("record");
        }
        assert_eq!(
            index.record("overflow", "/home/overflow"),
            Err(TrashError::Full)
        );
    }
}

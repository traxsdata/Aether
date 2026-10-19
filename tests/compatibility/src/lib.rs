// SPDX-License-Identifier: Apache-2.0
//! Compatibility database and classification rules.
//!
//! Every tested application receives exactly one classification:
//!
//! | Classification | Meaning |
//! |---|---|
//! | `verified` | Reproducibly passes the Aether test suite. |
//! | `compatible` | Works with documented limitations. |
//! | `limited` | Some functionality fails or needs a workaround. |
//! | `unsupported` | Known incompatibility or unmet security requirement. |
//!
//! Records identify the application version, runtime version, platform, test
//! date, the tests that were run, and any limitations. The file format is
//! deliberately simple pipe-separated text so it can be reviewed in a pull
//! request and parsed without a dependency.
//!
//! The database is a claim ledger, not a security boundary. A record must not
//! be upgraded without evidence; the parser enforces that `verified` and
//! `compatible` records name at least one test and that `unsupported` records
//! state a reason.

#![no_std]

#[cfg(test)]
extern crate std;

/// Maximum number of records.
pub const MAX_ENTRIES: usize = 64;
/// Maximum length of one database line.
pub const MAX_LINE: usize = 512;

/// Classification of a tested application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    /// Reproducibly passes the test suite.
    Verified,
    /// Works with documented limitations.
    Compatible,
    /// Some functionality fails or requires a workaround.
    Limited,
    /// Known incompatibility or unmet security requirement.
    Unsupported,
}

impl Classification {
    /// Canonical database name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Compatible => "compatible",
            Self::Limited => "limited",
            Self::Unsupported => "unsupported",
        }
    }

    /// Parses a classification name.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "verified" => Some(Self::Verified),
            "compatible" => Some(Self::Compatible),
            "limited" => Some(Self::Limited),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }
}

/// One compatibility record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Stable reverse-domain application identity.
    pub id: &'a str,
    /// Display name.
    pub name: &'a str,
    /// Tested application version.
    pub version: &'a str,
    /// Runtime name and version (`native`, or a pinned compatibility runtime).
    pub runtime: &'a str,
    /// Platform or virtual hardware used for the test.
    pub platform: &'a str,
    /// Test date in `YYYY-MM-DD` form.
    pub tested: &'a str,
    /// Classification.
    pub classification: Classification,
    /// Comma-separated test identifiers, or `none`.
    pub tests: &'a str,
    /// Documented limitations, or `none`.
    pub limitations: &'a str,
}

impl Entry<'_> {
    /// Number of named tests.
    #[must_use]
    pub fn test_count(&self) -> usize {
        if self.tests == "none" {
            0
        } else {
            self.tests.split(',').count()
        }
    }
}

/// Database parsing errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseError {
    /// A line exceeds the length limit.
    LineTooLong,
    /// A non-annotation line does not have exactly nine fields.
    FieldCount,
    /// A field is empty or contains a forbidden character.
    InvalidField,
    /// The identity is not a reverse-domain name.
    InvalidIdentity,
    /// The test date is not `YYYY-MM-DD`.
    InvalidDate,
    /// The classification name is unknown.
    UnknownClassification,
    /// Two records share an identity.
    DuplicateIdentity,
    /// A verified or compatible record names no test.
    MissingTests,
    /// An unsupported record states no reason.
    MissingReason,
    /// More records than [`MAX_ENTRIES`].
    TooManyEntries,
}

/// A parsed compatibility database.
pub struct Database<'a> {
    entries: [Option<Entry<'a>>; MAX_ENTRIES],
    len: usize,
}

impl<'a> Database<'a> {
    /// Parses the database text. `#` lines and blank lines are ignored.
    pub fn parse(text: &'a str) -> Result<Self, DatabaseError> {
        let mut database = Self {
            entries: [None; MAX_ENTRIES],
            len: 0,
        };
        for raw in text.split('\n') {
            let line = raw.strip_suffix('\r').unwrap_or(raw).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.len() > MAX_LINE {
                return Err(DatabaseError::LineTooLong);
            }
            let entry = parse_entry(line)?;
            if database.get(entry.id).is_some() {
                return Err(DatabaseError::DuplicateIdentity);
            }
            let slot = database
                .entries
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(DatabaseError::TooManyEntries)?;
            *slot = Some(entry);
            database.len += 1;
        }
        Ok(database)
    }

    /// Looks up a record by identity.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Entry<'a>> {
        self.entries.iter().flatten().find(|entry| entry.id == id)
    }

    /// Iterates over records.
    pub fn entries(&self) -> impl Iterator<Item = &Entry<'a>> {
        self.entries.iter().flatten()
    }

    /// Number of records.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the database is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Counts records with `classification`.
    #[must_use]
    pub fn count(&self, classification: Classification) -> usize {
        self.entries()
            .filter(|entry| entry.classification == classification)
            .count()
    }
}

fn parse_entry<'a>(line: &'a str) -> Result<Entry<'a>, DatabaseError> {
    let mut fields = line.split('|');
    // Surrounding whitespace is allowed for readability; values themselves
    // must not be empty.
    let mut next = || {
        fields
            .next()
            .map(str::trim)
            .ok_or(DatabaseError::FieldCount)
    };
    let id = next()?;
    let name = next()?;
    let version = next()?;
    let runtime = next()?;
    let platform = next()?;
    let tested = next()?;
    let classification = next()?;
    let tests = next()?;
    let limitations = next()?;
    if fields.next().is_some() {
        return Err(DatabaseError::FieldCount);
    }
    for field in [
        id,
        name,
        version,
        runtime,
        platform,
        tested,
        classification,
        tests,
        limitations,
    ] {
        if field.is_empty() || field.chars().any(|character| character.is_control()) {
            return Err(DatabaseError::InvalidField);
        }
    }
    if !aether_packaging::manifest::is_valid_identity(id) {
        return Err(DatabaseError::InvalidIdentity);
    }
    if !is_date(tested) {
        return Err(DatabaseError::InvalidDate);
    }
    let classification =
        Classification::parse(classification).ok_or(DatabaseError::UnknownClassification)?;
    let entry = Entry {
        id,
        name,
        version,
        runtime,
        platform,
        tested,
        classification,
        tests,
        limitations,
    };
    match classification {
        Classification::Verified | Classification::Compatible if entry.test_count() == 0 => {
            return Err(DatabaseError::MissingTests);
        }
        Classification::Unsupported if limitations == "none" => {
            return Err(DatabaseError::MissingReason);
        }
        _ => {}
    }
    Ok(entry)
}

fn is_date(value: &str) -> bool {
    value.len() == 10
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
}

/// Outcome of one compatibility test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The test passed.
    Passed,
    /// The feature works but only with a documented limitation.
    PassedWithLimitations,
    /// The test failed.
    Failed,
}

/// One named test result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TestResult<'a> {
    /// Test identifier such as `security.file.sandbox`.
    pub name: &'a str,
    /// Outcome.
    pub outcome: Outcome,
}

/// Derives a classification from a suite run.
///
/// A failure in a test whose name starts with `security.` is an unmet security
/// requirement and classifies the application as `unsupported`, regardless of
/// other results. An empty suite cannot verify anything.
#[must_use]
pub fn classify(results: &[TestResult<'_>]) -> Classification {
    if results.is_empty() {
        return Classification::Unsupported;
    }
    if results
        .iter()
        .any(|result| result.outcome == Outcome::Failed && result.name.starts_with("security."))
    {
        return Classification::Unsupported;
    }
    if results
        .iter()
        .any(|result| result.outcome == Outcome::Failed)
    {
        return Classification::Limited;
    }
    if results
        .iter()
        .any(|result| result.outcome == Outcome::PassedWithLimitations)
    {
        return Classification::Compatible;
    }
    Classification::Verified
}

/// Parses the checked-in database.
///
/// Embedding the records keeps the test suite and documentation on the same
/// text; `make compat-test` fails if the file is inconsistent.
pub fn shipped_database() -> Result<Database<'static>, DatabaseError> {
    Database::parse(include_str!("../database.txt"))
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: Apache-2.0
//! Host-testable size policy for bounded `memfd` objects.
//!
//! The kernel crate owns the physical pages and descriptor references; this
//! module holds the arithmetic that must stay identical between the syscall
//! boundary and the tests.

/// Maximum number of live memfd objects (also the per-task descriptor limit).
pub const MAX_MEMFDS: usize = 32;
/// Maximum bytes in one memfd object (16 MiB).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// Maximum pages in one memfd object.
pub const MAX_PAGES: usize = MAX_BYTES / super::PAGE_SIZE as usize;
/// Global page budget for every live memfd object (512 MiB).
pub const MAX_TOTAL_PAGES: usize = MAX_MEMFDS * MAX_PAGES;

/// Returns the page count needed for `length`, or `None` when over the cap.
#[must_use]
pub fn pages_for(length: usize) -> Option<usize> {
    if length > MAX_BYTES {
        return None;
    }
    Some(length.div_ceil(super::PAGE_SIZE as usize))
}

/// Returns `true` for a legal `ftruncate` length.
#[must_use]
pub fn valid_length(length: i64) -> bool {
    length >= 0 && (length as usize) <= MAX_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_policy_is_bounded_and_page_aligned() {
        assert_eq!(pages_for(0), Some(0));
        assert_eq!(pages_for(1), Some(1));
        assert_eq!(pages_for(super::super::PAGE_SIZE as usize), Some(1));
        assert_eq!(pages_for(super::super::PAGE_SIZE as usize + 1), Some(2));
        assert_eq!(pages_for(MAX_BYTES), Some(MAX_PAGES));
        assert_eq!(pages_for(MAX_BYTES + 1), None);
        assert!(valid_length(0));
        assert!(valid_length(MAX_BYTES as i64));
        assert!(!valid_length(-1));
        assert!(!valid_length(MAX_BYTES as i64 + 1));
    }

    #[test]
    fn total_budget_covers_all_maximum_objects() {
        assert_eq!(MAX_TOTAL_PAGES, MAX_MEMFDS * MAX_PAGES);
        assert!(MAX_TOTAL_PAGES * super::super::PAGE_SIZE as usize <= 512 * 1024 * 1024);
    }
}

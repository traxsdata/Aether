// SPDX-License-Identifier: Apache-2.0
//! Architecture-independent boot logic.
//!
//! The UEFI loader stays deliberately thin. Parsing and validation that can be
//! tested on the host lives here so that malformed kernel images are rejected
//! before any bytes are written into memory.

#![cfg_attr(not(test), no_std)]

pub mod elf;
pub mod graphics;
pub mod memory;

/// Size of an x86-64 page.
pub const PAGE_SIZE: u64 = 4096;

/// Rounds `value` up to a multiple of `alignment`.
///
/// Returns `None` for a non-power-of-two alignment or on overflow.
#[must_use]
pub const fn align_up(value: u64, alignment: u64) -> Option<u64> {
    if !alignment.is_power_of_two() {
        return None;
    }
    match value.checked_add(alignment - 1) {
        Some(value) => Some(value & !(alignment - 1)),
        None => None,
    }
}

/// Rounds `value` down to a multiple of `alignment`.
#[must_use]
pub const fn align_down(value: u64, alignment: u64) -> Option<u64> {
    if !alignment.is_power_of_two() {
        return None;
    }
    Some(value & !(alignment - 1))
}

/// Converts a byte length to a page count, rounding up.
#[must_use]
pub const fn pages_for(len: u64) -> Option<u64> {
    let Some(aligned) = align_up(len, PAGE_SIZE) else {
        return None;
    };
    Some(aligned / PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_up_and_down() {
        assert_eq!(align_up(0, 4096), Some(0));
        assert_eq!(align_up(1, 4096), Some(4096));
        assert_eq!(align_up(4096, 4096), Some(4096));
        assert_eq!(align_up(4097, 4096), Some(8192));
        assert_eq!(align_down(4097, 4096), Some(4096));
    }

    #[test]
    fn rejects_bad_alignments() {
        assert_eq!(align_up(1, 0), None);
        assert_eq!(align_up(1, 3), None);
        assert_eq!(align_down(1, 3), None);
    }

    #[test]
    fn alignment_overflow_is_rejected() {
        assert_eq!(align_up(u64::MAX, 4096), None);
    }

    #[test]
    fn counts_pages() {
        assert_eq!(pages_for(0), Some(0));
        assert_eq!(pages_for(1), Some(1));
        assert_eq!(pages_for(4096), Some(1));
        assert_eq!(pages_for(4097), Some(2));
    }
}

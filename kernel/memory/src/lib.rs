// SPDX-License-Identifier: Apache-2.0
//! Host-testable memory-management primitives.
//!
//! This crate contains the policy-free parts of Aether's physical memory
//! management: a bitmap frame allocator over a validated boot memory map and
//! the encoding of x86-64 page-table entries. Hardware access (CR3, INVLPG,
//! page-table installation) lives in the kernel crate.
//!
//! Every address handled here is a physical address. The kernel currently runs
//! identity-mapped, so this distinction is explicit rather than enforced by a
//! type.

#![no_std]

pub mod frame;
pub mod paging;
pub mod vma;

/// Size of an x86-64 page in bytes.
pub const PAGE_SIZE: u64 = 4096;

/// Size of a large x86-64 page in bytes.
pub const LARGE_PAGE_SIZE: u64 = 2 * 1024 * 1024;

/// Returns the number of pages needed to cover `bytes`, or `None` on overflow.
#[must_use]
pub const fn pages_for(bytes: u64) -> Option<u64> {
    let pages = bytes.div_ceil(PAGE_SIZE);
    if pages == 0 { None } else { Some(pages) }
}

/// Rounds `value` up to the next page boundary.
#[must_use]
pub const fn align_up(value: u64) -> Option<u64> {
    match value.checked_add(PAGE_SIZE - 1) {
        Some(value) => Some(value & !(PAGE_SIZE - 1)),
        None => None,
    }
}

/// Rounds `value` down to the previous page boundary.
#[must_use]
pub const fn align_down(value: u64) -> u64 {
    value & !(PAGE_SIZE - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_math_rounds_and_detects_overflow() {
        assert_eq!(pages_for(1), Some(1));
        assert_eq!(pages_for(PAGE_SIZE), Some(1));
        assert_eq!(pages_for(PAGE_SIZE + 1), Some(2));
        assert_eq!(pages_for(0), None);
        assert_eq!(align_up(0), Some(0));
        assert_eq!(align_up(1), Some(PAGE_SIZE));
        assert_eq!(align_down(PAGE_SIZE + 1), PAGE_SIZE);
        assert_eq!(align_up(u64::MAX), None);
    }
}

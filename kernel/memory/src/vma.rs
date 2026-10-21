// SPDX-License-Identifier: Apache-2.0
//! Host-testable virtual-memory-area tracking for the Linux ABI.
//!
//! The table stores page-aligned ranges in a fixed-size bounded window. It is
//! deliberately policy-free: the caller decides the base, limit, and
//! protections. The table rejects overlapping ranges and writable+executable
//! mappings so the kernel can enforce W^X before installing page permissions.
//!
//! A fixed table keeps every operation bounded. Range insertion, removal, and
//! protection may split an existing area; a failed split reports
//! [`VmaError::OutOfSpace`] instead of corrupting the table.

use crate::{PAGE_SIZE, align_up};

/// Maximum number of tracked areas.
#[cfg(feature = "gate4")]
pub const MAX_VMAS: usize = 512;
#[cfg(not(feature = "gate4"))]
pub const MAX_VMAS: usize = 32;
/// Fragment scratch capacity for split operations.
const MAX_FRAGMENTS: usize = MAX_VMAS * 2 + 1;

/// Read permission bit, matching the Linux `PROT_READ` value.
pub const PROT_READ: u32 = 1;
/// Write permission bit, matching the Linux `PROT_WRITE` value.
pub const PROT_WRITE: u32 = 2;
/// Execute permission bit, matching the Linux `PROT_EXEC` value.
pub const PROT_EXEC: u32 = 4;

/// One mapped area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vma {
    /// First address in the area.
    pub start: u64,
    /// Exclusive end address.
    pub end: u64,
    /// Linux protection bits.
    pub prot: u32,
}

impl Vma {
    /// Number of bytes in the area.
    #[must_use]
    pub const fn size(self) -> u64 {
        self.end - self.start
    }

    /// Returns `true` when the area contains `address`.
    #[must_use]
    pub const fn contains(self, address: u64) -> bool {
        address >= self.start && address < self.end
    }
}

/// Errors returned by [`VmaTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmaError {
    /// The length is zero, unaligned, or overflows.
    InvalidRange,
    /// The range is outside the table's window.
    OutsideWindow,
    /// The range overlaps an existing area.
    Overlap,
    /// No table slot is available for the range or its split fragments.
    OutOfSpace,
    /// The requested range is not fully mapped.
    NotFound,
    /// The protection set is unknown or combines write and execute.
    InvalidProtection,
}

/// Fixed-size virtual-memory-area table.
#[derive(Clone, Copy, Debug)]
pub struct VmaTable {
    window_start: u64,
    base: u64,
    limit: u64,
    entries: [Option<Vma>; MAX_VMAS],
}

impl VmaTable {
    /// Creates an empty table whose allocation cursor and window both start at
    /// `base`.
    ///
    /// Both bounds must be page-aligned and `base < limit`.
    #[must_use]
    pub const fn new(base: u64, limit: u64) -> Self {
        Self::with_window(base, base, limit)
    }

    /// Creates an empty table that can describe mappings anywhere in
    /// `window_start..limit`, while [`Self::insert`] starts searching at
    /// `allocation_base`.
    ///
    /// The Linux layer uses this to keep kernel-loaded program images above
    /// the mmap arena while still tracking them for `mprotect` and `munmap`.
    #[must_use]
    pub const fn with_window(window_start: u64, allocation_base: u64, limit: u64) -> Self {
        Self {
            window_start,
            base: allocation_base,
            limit,
            entries: [None; MAX_VMAS],
        }
    }

    /// Allocation cursor base.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// Exclusive window end.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Number of tracked areas.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    /// Returns `true` when no area is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the area containing `address`.
    #[must_use]
    pub fn get(&self, address: u64) -> Option<Vma> {
        self.entries
            .iter()
            .flatten()
            .copied()
            .find(|entry| entry.contains(address))
    }

    /// Returns `true` when every page in `start..start+length` is mapped.
    #[must_use]
    pub fn covers(&self, start: u64, length: u64) -> bool {
        let Ok(length) = checked_length(length) else {
            return false;
        };
        let Some(end) = start.checked_add(length) else {
            return false;
        };
        let mut page = start;
        while page < end {
            if self.get(page).is_none() {
                return false;
            }
            page += PAGE_SIZE;
        }
        true
    }

    /// Iterates over tracked areas in address order.
    pub fn iter(&self) -> impl Iterator<Item = Vma> + '_ {
        self.entries.iter().flatten().copied()
    }

    /// Clears every area.
    pub fn reset(&mut self) {
        self.entries = [None; MAX_VMAS];
    }

    /// Adds an area at the lowest free address that fits `length`.
    ///
    /// Returns the chosen start address.
    pub fn insert(&mut self, length: u64, prot: u32) -> Result<u64, VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot)?;
        let mut candidate = self.base;
        for entry in self.entries.iter().flatten() {
            if entry.start >= candidate && entry.start - candidate >= length {
                break;
            }
            candidate = candidate.max(entry.end);
        }
        let end = candidate
            .checked_add(length)
            .ok_or(VmaError::InvalidRange)?;
        if end > self.limit {
            return Err(VmaError::OutOfSpace);
        }
        self.insert_at(candidate, length, prot)?;
        Ok(candidate)
    }

    /// Adds an area at an exact address.
    ///
    /// Returns [`VmaError::Overlap`] if part of the range is already mapped, or
    /// [`VmaError::OutOfSpace`] when the table is full.
    pub fn insert_at(&mut self, start: u64, length: u64, prot: u32) -> Result<(), VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot)?;
        if !start.is_multiple_of(PAGE_SIZE) || start < self.window_start {
            return Err(VmaError::InvalidRange);
        }
        let end = start.checked_add(length).ok_or(VmaError::InvalidRange)?;
        if end > self.limit {
            return Err(VmaError::OutsideWindow);
        }
        if self
            .entries
            .iter()
            .flatten()
            .any(|entry| entry.start < end && start < entry.end)
        {
            return Err(VmaError::Overlap);
        }
        let slot = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .ok_or(VmaError::OutOfSpace)?;
        *slot = Some(Vma { start, end, prot });
        self.sort();
        Ok(())
    }

    /// Removes every byte in `start..start+length`.
    ///
    /// Adjacent areas are split when only part of the range overlaps. Removing
    /// an unmapped range is a no-op, matching Linux `munmap`; the returned
    /// boolean is `false` in that case.
    pub fn remove(&mut self, start: u64, length: u64) -> Result<bool, VmaError> {
        let length = checked_length(length)?;
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::InvalidRange);
        }
        let end = start.checked_add(length).ok_or(VmaError::InvalidRange)?;
        let mut fragments = [None; MAX_FRAGMENTS];
        let mut count = 0usize;
        let mut removed = false;
        for entry in self.entries.iter().flatten().copied() {
            if entry.end <= start || entry.start >= end {
                push_fragment(&mut fragments, &mut count, Some(entry))?;
                continue;
            }
            removed = true;
            if entry.start < start {
                push_fragment(
                    &mut fragments,
                    &mut count,
                    Some(Vma {
                        start: entry.start,
                        end: start,
                        prot: entry.prot,
                    }),
                )?;
            }
            if entry.end > end {
                push_fragment(
                    &mut fragments,
                    &mut count,
                    Some(Vma {
                        start: end,
                        end: entry.end,
                        prot: entry.prot,
                    }),
                )?;
            }
        }
        self.replace(fragments, count)?;
        Ok(removed)
    }

    /// Changes protections on a fully mapped range.
    ///
    /// The range may span several areas. Every page in the range must already
    /// be mapped, and writable+executable is rejected.
    pub fn protect(&mut self, start: u64, length: u64, prot: u32) -> Result<(), VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot)?;
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::InvalidRange);
        }
        let end = start.checked_add(length).ok_or(VmaError::InvalidRange)?;
        let mut page = start;
        while page < end {
            if self.get(page).is_none() {
                return Err(VmaError::NotFound);
            }
            page += PAGE_SIZE;
        }

        let mut fragments = [None; MAX_FRAGMENTS];
        let mut count = 0usize;
        for entry in self.entries.iter().flatten().copied() {
            if entry.end <= start || entry.start >= end {
                push_fragment(&mut fragments, &mut count, Some(entry))?;
                continue;
            }
            if entry.start < start {
                push_fragment(
                    &mut fragments,
                    &mut count,
                    Some(Vma {
                        start: entry.start,
                        end: start,
                        prot: entry.prot,
                    }),
                )?;
            }
            if entry.end > end {
                push_fragment(
                    &mut fragments,
                    &mut count,
                    Some(Vma {
                        start: end,
                        end: entry.end,
                        prot: entry.prot,
                    }),
                )?;
            }
        }
        push_fragment(&mut fragments, &mut count, Some(Vma { start, end, prot }))?;
        self.replace(fragments, count)?;
        Ok(())
    }

    fn replace(
        &mut self,
        fragments: [Option<Vma>; MAX_FRAGMENTS],
        count: usize,
    ) -> Result<(), VmaError> {
        if count > MAX_VMAS {
            return Err(VmaError::OutOfSpace);
        }
        self.entries = [None; MAX_VMAS];
        for fragment in fragments.into_iter().take(count).flatten() {
            if let Some(slot) = self.entries.iter_mut().find(|entry| entry.is_none()) {
                *slot = Some(fragment);
            }
        }
        self.sort();
        Ok(())
    }

    fn sort(&mut self) {
        self.entries
            .sort_unstable_by_key(|entry| entry.map_or(u64::MAX, |vma| vma.start));
    }
}

fn checked_length(length: u64) -> Result<u64, VmaError> {
    let aligned = align_up(length).ok_or(VmaError::InvalidRange)?;
    if aligned == 0 {
        return Err(VmaError::InvalidRange);
    }
    Ok(aligned)
}

fn validate_protection(prot: u32) -> Result<(), VmaError> {
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(VmaError::InvalidProtection);
    }
    if prot & (PROT_WRITE | PROT_EXEC) == (PROT_WRITE | PROT_EXEC) {
        return Err(VmaError::InvalidProtection);
    }
    Ok(())
}

fn push_fragment(
    fragments: &mut [Option<Vma>; MAX_FRAGMENTS],
    count: &mut usize,
    fragment: Option<Vma>,
) -> Result<(), VmaError> {
    let Some(fragment) = fragment else {
        return Ok(());
    };
    if fragment.start >= fragment.end {
        return Ok(());
    }
    let slot = fragments.get_mut(*count).ok_or(VmaError::OutOfSpace)?;
    *slot = Some(fragment);
    *count += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u64 = 0x1000_0000;
    const LIMIT: u64 = BASE + 16 * PAGE_SIZE;

    fn table() -> VmaTable {
        VmaTable::new(BASE, LIMIT)
    }

    #[test]
    fn inserts_at_lowest_fit_and_reuses_freed_space() {
        let mut map = table();
        let first = map.insert(3 * PAGE_SIZE, PROT_READ | PROT_WRITE).unwrap();
        assert_eq!(first, BASE);
        let second = map.insert(2 * PAGE_SIZE, PROT_READ).unwrap();
        assert_eq!(second, BASE + 3 * PAGE_SIZE);
        map.remove(first, 3 * PAGE_SIZE).unwrap();
        let reused = map.insert(2 * PAGE_SIZE, PROT_READ | PROT_WRITE).unwrap();
        assert_eq!(reused, BASE);
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn rejects_overlap_and_writable_exec() {
        let mut map = table();
        map.insert_at(BASE, PAGE_SIZE, PROT_READ | PROT_WRITE)
            .unwrap();
        assert_eq!(
            map.insert_at(BASE, PAGE_SIZE, PROT_READ),
            Err(VmaError::Overlap)
        );
        assert_eq!(
            map.insert_at(BASE + PAGE_SIZE, PAGE_SIZE, PROT_WRITE | PROT_EXEC),
            Err(VmaError::InvalidProtection)
        );
        assert_eq!(
            map.insert_at(BASE + PAGE_SIZE, PAGE_SIZE, 8),
            Err(VmaError::InvalidProtection)
        );
    }

    #[test]
    fn splits_on_partial_remove_and_protect() {
        let mut map = table();
        map.insert_at(BASE, 4 * PAGE_SIZE, PROT_READ | PROT_WRITE)
            .unwrap();
        map.remove(BASE + PAGE_SIZE, PAGE_SIZE).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(BASE).unwrap().end, BASE + PAGE_SIZE);
        assert_eq!(
            map.get(BASE + PAGE_SIZE),
            None,
            "removed page must be unmapped"
        );
        assert_eq!(
            map.get(BASE + 2 * PAGE_SIZE).unwrap().prot,
            PROT_READ | PROT_WRITE
        );

        map.protect(BASE + 2 * PAGE_SIZE, 2 * PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(map.get(BASE).unwrap().prot, PROT_READ | PROT_WRITE);
        assert_eq!(map.get(BASE + 2 * PAGE_SIZE).unwrap().prot, PROT_READ);
    }

    #[test]
    fn protect_requires_full_coverage() {
        let mut map = table();
        map.insert_at(BASE, PAGE_SIZE, PROT_READ).unwrap();
        assert_eq!(
            map.protect(BASE, 2 * PAGE_SIZE, PROT_READ),
            Err(VmaError::NotFound)
        );
    }

    #[test]
    fn removes_spanning_areas_and_reports_exhaustion() {
        let mut map = table();
        for page in 0..16 {
            map.insert_at(BASE + page * PAGE_SIZE, PAGE_SIZE, PROT_READ)
                .unwrap();
        }
        assert_eq!(
            map.insert_at(BASE + 16 * PAGE_SIZE, PAGE_SIZE, PROT_READ),
            Err(VmaError::OutsideWindow)
        );
        map.remove(BASE + PAGE_SIZE, 15 * PAGE_SIZE).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(BASE).unwrap().start, BASE);
    }
}

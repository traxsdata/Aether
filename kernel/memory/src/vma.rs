// SPDX-License-Identifier: Apache-2.0
//! Host-testable virtual-memory-area tracking for the Linux ABI.
//!
//! The table stores page-aligned ranges in a fixed-size bounded window. The
//! caller decides the base, limit, and protections. The table rejects
//! overlapping ranges and unknown protection bits. Initial mappings may be
//! writable+executable because the LLVM JIT used by lavapipe requests them
//! directly, while [`VmaTable::protect`] rejects adding write to an executable
//! mapping so a later protection change still enforces W^X.
//!
//! The table is an ordered map keyed by start address, so lookups and splits
//! cost `O(log n)` and a large application can hold tens of thousands of
//! areas. The entry count is still capped per table ([`MAX_VMAS`], like
//! Linux's `vm.max_map_count`); range insertion, removal, and protection may
//! split an existing area, and a split that would exceed the cap reports
//! [`VmaError::OutOfSpace`] before anything changes.

extern crate alloc;

use crate::{PAGE_SIZE, align_up};
use alloc::collections::BTreeMap;

/// Maximum number of tracked areas per table.
///
/// `large-apps` matches Linux's default `vm.max_map_count` for programs such
/// as Firefox. The other values keep the boot fixture's quota test small.
#[cfg(feature = "large-apps")]
pub const MAX_VMAS: usize = 65_530;
#[cfg(all(feature = "gate4", not(feature = "large-apps")))]
pub const MAX_VMAS: usize = 512;
#[cfg(not(any(feature = "gate4", feature = "large-apps")))]
pub const MAX_VMAS: usize = 32;

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
    /// The protection set is unknown, or a protection change combines write
    /// and execute.
    InvalidProtection,
}

/// Ordered virtual-memory-area table.
#[derive(Clone, Debug)]
pub struct VmaTable {
    window_start: u64,
    base: u64,
    limit: u64,
    /// Every address in `base..first_gap` is mapped, so a first-fit search
    /// can start here. Sequential `mmap` calls then cost `O(log n)` each
    /// instead of rescanning every area below.
    first_gap: u64,
    /// Areas keyed by start address; never overlapping, and neighbours with
    /// equal protections are always merged.
    entries: BTreeMap<u64, Vma>,
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
            first_gap: allocation_base,
            entries: BTreeMap::new(),
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
        self.entries.len()
    }

    /// Returns `true` when no area is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the area containing `address`.
    #[must_use]
    pub fn get(&self, address: u64) -> Option<Vma> {
        self.entries
            .range(..=address)
            .next_back()
            .map(|(_, entry)| *entry)
            .filter(|entry| entry.contains(address))
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
        let mut cursor = start;
        while cursor < end {
            match self.get(cursor) {
                Some(entry) => cursor = entry.end,
                None => return false,
            }
        }
        true
    }

    /// Iterates over tracked areas in address order.
    pub fn iter(&self) -> impl Iterator<Item = Vma> + '_ {
        self.entries.values().copied()
    }

    /// Clears every area.
    pub fn reset(&mut self) {
        self.entries.clear();
        self.first_gap = self.base;
    }

    /// Adds an area at the lowest free address that fits `length`.
    ///
    /// Returns the chosen start address.
    pub fn insert(&mut self, length: u64, prot: u32) -> Result<u64, VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot, true)?;
        let mut candidate = self.first_gap.max(self.base);
        // An area that starts below the cursor may still reach past it.
        if let Some(entry) = self.get(candidate) {
            candidate = entry.end;
        }
        // Stays true while every address skipped so far is mapped.
        let mut contiguous = true;
        for entry in self.entries.range(candidate..).map(|(_, entry)| entry) {
            if entry.start - candidate >= length {
                break;
            }
            if entry.start != candidate {
                contiguous = false;
            }
            candidate = entry.end;
        }
        let end = candidate
            .checked_add(length)
            .ok_or(VmaError::InvalidRange)?;
        if end > self.limit {
            return Err(VmaError::OutOfSpace);
        }
        self.insert_at(candidate, length, prot)?;
        if contiguous {
            self.first_gap = end;
        }
        Ok(candidate)
    }

    /// Adds an area at an exact address.
    ///
    /// Returns [`VmaError::Overlap`] if part of the range is already mapped, or
    /// [`VmaError::OutOfSpace`] when the table is full.
    pub fn insert_at(&mut self, start: u64, length: u64, prot: u32) -> Result<(), VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot, true)?;
        if !start.is_multiple_of(PAGE_SIZE) || start < self.window_start {
            return Err(VmaError::InvalidRange);
        }
        let end = start.checked_add(length).ok_or(VmaError::InvalidRange)?;
        if end > self.limit {
            return Err(VmaError::OutsideWindow);
        }
        if self
            .entries
            .range(..end)
            .next_back()
            .is_some_and(|(_, entry)| entry.end > start)
        {
            return Err(VmaError::Overlap);
        }
        if self.entries.len() >= MAX_VMAS {
            return Err(VmaError::OutOfSpace);
        }
        self.entries.insert(start, Vma { start, end, prot });
        self.merge_around(start, end);
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
        let (overlapping, pieces) = self.split_cost(start, end);
        if overlapping == 0 {
            return Ok(false);
        }
        if self.entries.len() - overlapping + pieces > MAX_VMAS {
            return Err(VmaError::OutOfSpace);
        }
        self.cut(start, end);
        self.first_gap = self.first_gap.min(start);
        Ok(true)
    }

    /// Changes protections on a fully mapped range.
    ///
    /// The range may span several areas. Every page in the range must already
    /// be mapped, and writable+executable is rejected.
    pub fn protect(&mut self, start: u64, length: u64, prot: u32) -> Result<(), VmaError> {
        let length = checked_length(length)?;
        validate_protection(prot, false)?;
        if !start.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::InvalidRange);
        }
        let end = start.checked_add(length).ok_or(VmaError::InvalidRange)?;
        if !self.covers(start, length) {
            return Err(VmaError::NotFound);
        }
        let (overlapping, pieces) = self.split_cost(start, end);
        if self.entries.len() - overlapping + pieces + 1 > MAX_VMAS {
            return Err(VmaError::OutOfSpace);
        }
        self.cut(start, end);
        self.entries.insert(start, Vma { start, end, prot });
        self.merge_around(start, end);
        Ok(())
    }

    /// Counts the areas overlapping `start..end` and the edge pieces that
    /// would survive cutting the range out of them.
    fn split_cost(&self, start: u64, end: u64) -> (usize, usize) {
        let mut overlapping = 0usize;
        let mut pieces = 0usize;
        for entry in self.overlapping(start, end) {
            overlapping += 1;
            if entry.start < start {
                pieces += 1;
            }
            if entry.end > end {
                pieces += 1;
            }
        }
        (overlapping, pieces)
    }

    fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = Vma> + '_ {
        let first = self
            .entries
            .range(..=start)
            .next_back()
            .map_or(start, |(key, _)| *key);
        self.entries
            .range(first..end)
            .map(|(_, entry)| *entry)
            .filter(move |entry| entry.end > start)
    }

    /// Removes `start..end`, keeping the parts of split areas outside it.
    ///
    /// The caller has checked the entry cap.
    fn cut(&mut self, start: u64, end: u64) {
        let mut cursor = self
            .entries
            .range(..=start)
            .next_back()
            .map_or(start, |(key, _)| *key);
        while cursor < end
            && let Some((&key, &entry)) = self.entries.range(cursor..end).next()
        {
            cursor = entry.end;
            if entry.end <= start {
                continue;
            }
            self.entries.remove(&key);
            if entry.start < start {
                self.entries.insert(
                    entry.start,
                    Vma {
                        start: entry.start,
                        end: start,
                        prot: entry.prot,
                    },
                );
            }
            if entry.end > end {
                self.entries.insert(
                    end,
                    Vma {
                        start: end,
                        end: entry.end,
                        prot: entry.prot,
                    },
                );
            }
        }
    }

    /// Coalesces the area at `start..end` with equal-protection neighbours.
    ///
    /// Linux merges neighbouring VMAs, and clients such as the LLVM JIT make
    /// many small mappings and protection changes. Without the merge the
    /// table fills with fragments and later `mmap`/`mprotect` calls fail with
    /// `ENOMEM` even though the address space is free.
    fn merge_around(&mut self, start: u64, end: u64) {
        let Some(mut merged) = self.entries.get(&start).copied() else {
            return;
        };
        if let Some(next) = self.entries.get(&end).copied()
            && next.prot == merged.prot
        {
            self.entries.remove(&end);
            merged.end = next.end;
        }
        if let Some((&key, previous)) = self.entries.range(..start).next_back()
            && previous.end == start
            && previous.prot == merged.prot
        {
            self.entries.remove(&start);
            merged.start = key;
        }
        self.entries.insert(merged.start, merged);
    }
}

fn checked_length(length: u64) -> Result<u64, VmaError> {
    let aligned = align_up(length).ok_or(VmaError::InvalidRange)?;
    if aligned == 0 {
        return Err(VmaError::InvalidRange);
    }
    Ok(aligned)
}

fn validate_protection(prot: u32, allow_write_exec: bool) -> Result<(), VmaError> {
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 {
        return Err(VmaError::InvalidProtection);
    }
    // Like Linux, an initial mapping may be writable+executable: the LLVM
    // section allocator requests it directly when it does not use a two-step
    // W^X mapping. A protection change must not add write to an executable
    // mapping, matching the kernel's stricter `mprotect` policy.
    if !allow_write_exec && prot & (PROT_WRITE | PROT_EXEC) == PROT_WRITE | PROT_EXEC {
        return Err(VmaError::InvalidProtection);
    }
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
    fn rejects_overlap_and_unknown_protection_but_allows_initial_wx() {
        let mut map = table();
        map.insert_at(BASE, PAGE_SIZE, PROT_READ | PROT_WRITE)
            .unwrap();
        assert_eq!(
            map.insert_at(BASE, PAGE_SIZE, PROT_READ),
            Err(VmaError::Overlap)
        );
        map.insert_at(BASE + PAGE_SIZE, PAGE_SIZE, PROT_WRITE | PROT_EXEC)
            .unwrap();
        assert_eq!(
            map.get(BASE + PAGE_SIZE).unwrap().prot,
            PROT_WRITE | PROT_EXEC
        );
        assert_eq!(
            map.insert_at(BASE + 2 * PAGE_SIZE, PAGE_SIZE, 8),
            Err(VmaError::InvalidProtection)
        );
        assert_eq!(
            map.protect(BASE + PAGE_SIZE, PAGE_SIZE, PROT_WRITE | PROT_EXEC),
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

    #[test]
    fn coalesces_equal_neighbours_and_keeps_distinct_ones() {
        let mut map = table();
        map.insert_at(BASE, PAGE_SIZE, PROT_READ).unwrap();
        map.insert_at(BASE + 2 * PAGE_SIZE, PAGE_SIZE, PROT_READ)
            .unwrap();
        map.insert_at(BASE + PAGE_SIZE, PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(map.len(), 1);
        map.protect(BASE + PAGE_SIZE, PAGE_SIZE, PROT_READ | PROT_WRITE)
            .unwrap();
        assert_eq!(map.len(), 3);
        map.protect(BASE + PAGE_SIZE, PAGE_SIZE, PROT_READ).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(BASE + 2 * PAGE_SIZE).unwrap().start, BASE);
    }

    #[test]
    fn insert_skips_an_area_that_straddles_the_cursor() {
        let mut map = VmaTable::with_window(BASE - 4 * PAGE_SIZE, BASE, LIMIT);
        map.insert_at(BASE - PAGE_SIZE, 2 * PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(
            map.insert(PAGE_SIZE, PROT_READ | PROT_WRITE).unwrap(),
            BASE + PAGE_SIZE
        );
    }

    #[test]
    fn a_split_past_the_cap_changes_nothing() {
        let limit = BASE + (3 * MAX_VMAS as u64 + 4) * PAGE_SIZE;
        let mut map = VmaTable::new(BASE, limit);
        for index in 0..MAX_VMAS as u64 {
            let prot = if index % 2 == 0 {
                PROT_READ
            } else {
                PROT_READ | PROT_WRITE
            };
            map.insert_at(BASE + index * 3 * PAGE_SIZE, 3 * PAGE_SIZE, prot)
                .unwrap();
        }
        assert_eq!(map.len(), MAX_VMAS);
        let before = map.clone();
        assert_eq!(
            map.remove(BASE + PAGE_SIZE, PAGE_SIZE),
            Err(VmaError::OutOfSpace)
        );
        assert_eq!(
            map.protect(BASE, PAGE_SIZE, PROT_READ | PROT_WRITE),
            Err(VmaError::OutOfSpace)
        );
        assert_eq!(
            map.iter().collect::<alloc::vec::Vec<_>>(),
            before.iter().collect::<alloc::vec::Vec<_>>()
        );
        // Removing a whole area needs no new entry and still succeeds.
        assert_eq!(map.remove(BASE, 3 * PAGE_SIZE), Ok(true));
        assert_eq!(map.len(), MAX_VMAS - 1);
    }

    #[test]
    fn remove_and_protect_span_many_areas() {
        let mut map = VmaTable::new(BASE, BASE + 64 * PAGE_SIZE);
        for index in 0..8u64 {
            let prot = if index % 2 == 0 { PROT_READ } else { PROT_EXEC };
            map.insert_at(BASE + index * 4 * PAGE_SIZE, 4 * PAGE_SIZE, prot)
                .unwrap();
        }
        map.protect(BASE + 2 * PAGE_SIZE, 28 * PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(BASE).unwrap().end, BASE + 30 * PAGE_SIZE);
        assert_eq!(map.remove(BASE + PAGE_SIZE, 30 * PAGE_SIZE), Ok(true));
        let left: alloc::vec::Vec<_> = map.iter().collect();
        assert_eq!(left.len(), 2);
        assert_eq!((left[0].start, left[0].end), (BASE, BASE + PAGE_SIZE));
        assert_eq!(left[1].start, BASE + 31 * PAGE_SIZE);
        assert!(!map.covers(BASE, 2 * PAGE_SIZE));
        assert!(map.covers(BASE + 31 * PAGE_SIZE, PAGE_SIZE));
    }

    #[test]
    fn first_fit_reuses_holes_below_the_sequential_cursor() {
        let mut map = VmaTable::new(BASE, BASE + 64 * PAGE_SIZE);
        let mut starts = alloc::vec::Vec::new();
        for index in 0..8u64 {
            let prot = if index % 2 == 0 { PROT_READ } else { PROT_EXEC };
            starts.push(map.insert(PAGE_SIZE, prot).unwrap());
        }
        assert_eq!(starts[7], BASE + 7 * PAGE_SIZE);
        map.remove(starts[2], PAGE_SIZE).unwrap();
        assert_eq!(map.insert(PAGE_SIZE, PROT_READ).unwrap(), starts[2]);
        assert_eq!(
            map.insert(PAGE_SIZE, PROT_READ).unwrap(),
            BASE + 8 * PAGE_SIZE
        );
        // A fixed mapping above the cursor is skipped, and a hole too small
        // for the request is not used.
        map.insert_at(BASE + 10 * PAGE_SIZE, PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(
            map.insert(2 * PAGE_SIZE, PROT_EXEC).unwrap(),
            BASE + 11 * PAGE_SIZE
        );
        assert_eq!(
            map.insert(PAGE_SIZE, PROT_EXEC).unwrap(),
            BASE + 9 * PAGE_SIZE
        );
        map.reset();
        assert_eq!(map.insert(PAGE_SIZE, PROT_READ).unwrap(), BASE);
    }

    #[test]
    fn insert_corrects_for_fixed_mappings_at_the_cursor() {
        let mut map = table();
        map.insert(2 * PAGE_SIZE, PROT_READ).unwrap();
        // A fixed mapping exactly at the cursor merges with the area below
        // it, so the next search starts inside an area that began earlier.
        map.insert_at(BASE + 2 * PAGE_SIZE, PAGE_SIZE, PROT_READ)
            .unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.insert(PAGE_SIZE, PROT_EXEC).unwrap(),
            BASE + 3 * PAGE_SIZE
        );
        // Below the cursor every address is mapped, so a fixed mapping there
        // overlaps and leaves the cursor alone.
        assert_eq!(
            map.insert_at(BASE + PAGE_SIZE, PAGE_SIZE, PROT_EXEC),
            Err(VmaError::Overlap)
        );
        // A fixed mapping that fills the hole the cursor points at after a
        // removal is skipped by the next search.
        map.remove(BASE, PAGE_SIZE).unwrap();
        map.insert_at(BASE, PAGE_SIZE, PROT_EXEC).unwrap();
        assert_eq!(
            map.insert(PAGE_SIZE, PROT_READ).unwrap(),
            BASE + 4 * PAGE_SIZE
        );
    }
}

// SPDX-License-Identifier: Apache-2.0
//! x86-64 four-level page-table encoding.
//!
//! The structures here are layout-compatible with the CPU's page tables. The
//! kernel allocates one 4 KiB frame per table and installs the physical
//! address of the level-4 table in CR3. No heap is involved.

use crate::{LARGE_PAGE_SIZE, PAGE_SIZE};

/// Number of entries in every page table.
pub const PAGE_TABLE_ENTRIES: usize = 512;

/// Physical address bits carried by a page-table entry.
pub const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;

/// Page-table entry flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFlags(u64);

impl PageFlags {
    /// The entry is present.
    pub const PRESENT: Self = Self(1 << 0);
    /// Writes are permitted.
    pub const WRITABLE: Self = Self(1 << 1);
    /// User-mode access is permitted.
    pub const USER: Self = Self(1 << 2);
    /// Use write-through caching.
    pub const WRITE_THROUGH: Self = Self(1 << 3);
    /// Disable caching (required for most MMIO).
    pub const CACHE_DISABLE: Self = Self(1 << 4);
    /// The entry has been accessed.
    pub const ACCESSED: Self = Self(1 << 5);
    /// The entry has been written (leaf entries).
    pub const DIRTY: Self = Self(1 << 6);
    /// This entry maps a 2 MiB page (level-2 table only).
    pub const HUGE: Self = Self(1 << 7);
    /// Do not flush this mapping from the TLB on address-space switches.
    pub const GLOBAL: Self = Self(1 << 8);
    /// Software bit (ignored by the CPU): a user page mapped `PROT_NONE`.
    ///
    /// The entry stays present so the page keeps its frame across `fork`,
    /// `mprotect` and `munmap`, but [`PageFlags::USER`] is clear, so every
    /// user-mode access faults and user-pointer validation rejects it.
    pub const GUARD: Self = Self(1 << 9);
    /// Instruction fetches are forbidden.
    pub const NO_EXECUTE: Self = Self(1 << 63);

    /// No flags set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Builds flags from raw bits.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits & !ADDRESS_MASK)
    }

    /// Raw flag bits.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns `true` if every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Combines two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Removes every flag in `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

impl core::ops::BitOr for PageFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

/// One page-table entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    /// An absent entry.
    pub const EMPTY: Self = Self(0);

    /// Encodes a physical address and flags.
    ///
    /// Returns `None` if the address is not page-aligned or contains bits
    /// outside the architectural address field.
    #[must_use]
    pub const fn new(address: u64, flags: PageFlags) -> Option<Self> {
        if address & (PAGE_SIZE - 1) != 0 || address & !ADDRESS_MASK != 0 {
            return None;
        }
        Some(Self((address & ADDRESS_MASK) | flags.bits()))
    }

    /// Encodes a 2 MiB page.
    ///
    /// Returns `None` if the address is not 2 MiB-aligned.
    #[must_use]
    pub const fn new_large(address: u64, flags: PageFlags) -> Option<Self> {
        if address & (LARGE_PAGE_SIZE - 1) != 0 || address & !ADDRESS_MASK != 0 {
            return None;
        }
        Some(Self(
            (address & ADDRESS_MASK) | flags.union(PageFlags::HUGE).bits(),
        ))
    }

    /// Raw entry value.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// Physical address carried by the entry.
    #[must_use]
    pub const fn address(self) -> u64 {
        self.0 & ADDRESS_MASK
    }

    /// Flags carried by the entry.
    #[must_use]
    pub const fn flags(self) -> PageFlags {
        PageFlags::from_bits(self.0)
    }

    /// Returns `true` if the present bit is set.
    #[must_use]
    pub const fn is_present(self) -> bool {
        self.flags().contains(PageFlags::PRESENT)
    }

    /// Returns `true` if the entry maps a large page.
    #[must_use]
    pub const fn is_huge(self) -> bool {
        self.flags().contains(PageFlags::HUGE)
    }
}

/// A 4 KiB page table with 512 entries.
#[derive(Clone, Copy, Debug)]
#[repr(C, align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; PAGE_TABLE_ENTRIES],
}

impl PageTable {
    /// An all-zero page table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: [PageTableEntry::EMPTY; PAGE_TABLE_ENTRIES],
        }
    }

    /// Reads an entry.
    #[must_use]
    pub fn entry(&self, index: usize) -> PageTableEntry {
        self.entries[index]
    }

    /// Writes an entry.
    pub fn set_entry(&mut self, index: usize, entry: PageTableEntry) {
        self.entries[index] = entry;
    }

    /// Returns all entries.
    #[must_use]
    pub fn entries(&self) -> &[PageTableEntry] {
        &self.entries
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the four table indices for a virtual address, from level 4 to
/// level 1.
#[must_use]
pub const fn indices(address: u64) -> [usize; 4] {
    [
        ((address >> 39) & 0x1ff) as usize,
        ((address >> 30) & 0x1ff) as usize,
        ((address >> 21) & 0x1ff) as usize,
        ((address >> 12) & 0x1ff) as usize,
    ]
}

/// Returns the level-2 index and offset within a 2 MiB page.
#[must_use]
pub const fn large_page_parts(address: u64) -> (usize, u64) {
    (
        ((address >> 21) & 0x1ff) as usize,
        address & (LARGE_PAGE_SIZE - 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_table_is_one_page_and_empty() {
        assert_eq!(core::mem::size_of::<PageTable>(), 4096);
        assert_eq!(core::mem::align_of::<PageTable>(), 4096);
        assert!(
            PageTable::new()
                .entries()
                .iter()
                .all(|entry| *entry == PageTableEntry::EMPTY)
        );
    }

    #[test]
    fn encodes_and_decodes_addresses() {
        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
        let entry = PageTableEntry::new(0x2000, flags).expect("aligned");
        assert_eq!(entry.address(), 0x2000);
        assert!(entry.flags().contains(PageFlags::PRESENT));
        assert!(entry.flags().contains(PageFlags::WRITABLE));
        assert!(entry.flags().contains(PageFlags::NO_EXECUTE));
        assert!(!entry.flags().contains(PageFlags::USER));
        assert!(entry.is_present());
        assert!(!entry.is_huge());
    }

    #[test]
    fn rejects_misaligned_or_out_of_range_addresses() {
        assert!(PageTableEntry::new(1, PageFlags::PRESENT).is_none());
        assert!(PageTableEntry::new(1 << 52, PageFlags::PRESENT).is_none());
        assert!(PageTableEntry::new_large(0x1000, PageFlags::PRESENT).is_none());
        assert!(PageTableEntry::new_large(2 * 1024 * 1024, PageFlags::PRESENT).is_some());
    }

    #[test]
    fn large_pages_set_the_huge_flag() {
        let entry = PageTableEntry::new_large(0x20_0000, PageFlags::PRESENT).expect("aligned");
        assert!(entry.is_huge());
        assert_eq!(entry.address(), 0x20_0000);
    }

    #[test]
    fn indices_split_canonical_addresses() {
        assert_eq!(indices(0), [0, 0, 0, 0]);
        assert_eq!(indices(0x1000), [0, 0, 0, 1]);
        assert_eq!(indices(0x0000_7fff_ffff_f000), [255, 511, 511, 511]);
        assert_eq!(indices(0xffff_8000_0000_0000), [256, 0, 0, 0]);
    }

    #[test]
    fn large_page_parts_are_consistent() {
        assert_eq!(large_page_parts(0), (0, 0));
        assert_eq!(large_page_parts(0x20_0000), (1, 0));
        assert_eq!(large_page_parts(0x3f_ffff), (1, 0x1f_ffff));
    }

    #[test]
    fn entry_set_and_get_round_trip() {
        let mut table = PageTable::new();
        let entry = PageTableEntry::new(0x3000, PageFlags::PRESENT).expect("aligned");
        table.set_entry(7, entry);
        assert_eq!(table.entry(7), entry);
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Aether boot ABI.
//!
//! This crate is dependency-free and `no_std` so the same definitions are used
//! by the UEFI loader, the kernel, and host-side tests. It describes only the
//! loader-to-kernel hand-off; kernel-internal structures do not belong here.
//!
//! The loader is trusted, but the kernel still validates the hand-off. A
//! firmware bug, a corrupted image, or a future loader mismatch must fail
//! closed instead of causing undefined behaviour. The kernel also checks the
//! hand-off pointer's alignment before forming a reference.

#![no_std]

pub mod user;

use core::fmt;
use core::mem::{align_of, size_of};

/// Magic value identifying an [`BootInfo`] block. ASCII `AETHEROS`.
pub const BOOT_INFO_MAGIC: u64 = u64::from_le_bytes(*b"AETHEROS");

/// Version of the hand-off structure.
///
/// Bump this for any layout or semantic change. The kernel refuses versions it
/// does not implement.
pub const BOOT_INFO_VERSION: u32 = 1;

/// Maximum number of memory regions passed to the kernel.
///
/// The bound keeps both sides allocation-free. The loader fails closed if
/// firmware reports more regions than this.
pub const MAX_MEMORY_REGIONS: usize = 256;

/// Coarse memory classification used by the kernel.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// RAM the kernel may allocate.
    Usable = 0,
    /// Firmware, device, or otherwise reserved memory.
    Reserved = 1,
    /// ACPI tables that may be reclaimed after parsing.
    AcpiReclaimable = 2,
    /// ACPI non-volatile storage that must be preserved.
    AcpiNvs = 3,
    /// Firmware-reported unusable memory.
    Unusable = 4,
    /// Boot-services memory, free after `ExitBootServices`.
    BootServices = 5,
    /// Runtime-services memory, preserved after `ExitBootServices`.
    RuntimeServices = 6,
    /// Loader-owned memory: loader image, kernel image, and boot metadata.
    Loader = 7,
    /// Memory-mapped I/O or port space.
    Mmio = 8,
    /// Any other firmware-defined region.
    Other = 9,
}

impl MemoryKind {
    /// Whether the region may be given to the page allocator.
    #[must_use]
    pub const fn is_allocatable(self) -> bool {
        matches!(
            self,
            Self::Usable | Self::BootServices | Self::AcpiReclaimable
        )
    }

    /// Converts a raw ABI value, rejecting reserved discriminants.
    ///
    /// The region array is read as raw bytes by both sides, so an invalid
    /// discriminant must be reported rather than materialized as an enum.
    #[must_use]
    pub const fn from_raw(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Usable),
            1 => Some(Self::Reserved),
            2 => Some(Self::AcpiReclaimable),
            3 => Some(Self::AcpiNvs),
            4 => Some(Self::Unusable),
            5 => Some(Self::BootServices),
            6 => Some(Self::RuntimeServices),
            7 => Some(Self::Loader),
            8 => Some(Self::Mmio),
            9 => Some(Self::Other),
            _ => None,
        }
    }
}

/// One physical memory region.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    /// Inclusive physical start address.
    pub start: u64,
    /// Length in bytes.
    pub len: u64,
    /// Classification, stored as a raw value for ABI validation.
    pub kind: u32,
    /// Must be zero.
    pub reserved: u32,
}

impl MemoryRegion {
    /// Creates a region with the reserved field cleared.
    #[must_use]
    pub const fn new(start: u64, len: u64, kind: MemoryKind) -> Self {
        Self {
            start,
            len,
            kind: kind as u32,
            reserved: 0,
        }
    }

    /// Decoded classification, or `None` for an unknown ABI value.
    #[must_use]
    pub const fn kind(&self) -> Option<MemoryKind> {
        MemoryKind::from_raw(self.kind)
    }

    /// Exclusive end address, or `None` on overflow.
    #[must_use]
    pub const fn end(&self) -> Option<u64> {
        self.start.checked_add(self.len)
    }
}

/// Reasons [`BootInfo::validate`] can reject a hand-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootInfoError {
    /// Wrong magic value.
    BadMagic,
    /// Unsupported structure version.
    UnsupportedVersion(u32),
    /// Structure is smaller than this kernel expects.
    Truncated { reported: u32, required: u32 },
    /// Reserved header fields must be zero.
    ReservedNotZero,
    /// Memory map pointer is null.
    NullMemoryMap,
    /// Memory map pointer does not satisfy `MemoryRegion` alignment.
    MemoryMapMisaligned,
    /// Memory map has no regions.
    EmptyMemoryMap,
    /// Region count exceeds [`MAX_MEMORY_REGIONS`].
    TooManyRegions(u64),
    /// Region array size overflows.
    MemoryMapOverflow,
    /// Kernel range is empty or overflows.
    InvalidKernelRange,
    /// Entry point is outside the loaded kernel image.
    EntryOutsideKernel,
    /// A region is empty, overflows, or has an unknown kind.
    InvalidRegion { index: u64 },
    /// Regions are not sorted by ascending physical start address.
    RegionsNotSorted { index: u64 },
    /// Two regions overlap.
    OverlappingRegions { index: u64 },
}

impl fmt::Display for BootInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "bad boot information magic"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported boot information version {v}")
            }
            Self::Truncated { reported, required } => {
                write!(f, "boot information is {reported} bytes, need {required}")
            }
            Self::ReservedNotZero => write!(f, "reserved boot information fields are not zero"),
            Self::NullMemoryMap => write!(f, "null memory map pointer"),
            Self::MemoryMapMisaligned => write!(f, "misaligned memory map pointer"),
            Self::EmptyMemoryMap => write!(f, "empty memory map"),
            Self::TooManyRegions(n) => {
                write!(
                    f,
                    "memory map has {n} regions, limit is {MAX_MEMORY_REGIONS}"
                )
            }
            Self::MemoryMapOverflow => write!(f, "memory map size overflows"),
            Self::InvalidKernelRange => write!(f, "invalid kernel address range"),
            Self::EntryOutsideKernel => write!(f, "kernel entry point outside kernel image"),
            Self::InvalidRegion { index } => write!(f, "invalid memory region at index {index}"),
            Self::RegionsNotSorted { index } => {
                write!(f, "memory regions are not sorted at index {index}")
            }
            Self::OverlappingRegions { index } => {
                write!(f, "memory region at index {index} overlaps its predecessor")
            }
        }
    }
}

/// Loader-to-kernel hand-off.
///
/// # ABI rules
///
/// * `repr(C)`, fixed-width fields.
/// * New fields require a new [`BOOT_INFO_VERSION`].
/// * Pointers are physical addresses. The kernel starts on the identity map
///   established by firmware and does not call UEFI boot services.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootInfo {
    /// Must equal [`BOOT_INFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOT_INFO_VERSION`].
    pub version: u32,
    /// Size of this structure in bytes.
    pub struct_size: u32,
    /// Physical address of the region array.
    pub memory_regions: u64,
    /// Number of entries in the region array.
    pub memory_region_count: u64,
    /// First physical address of the kernel image.
    pub kernel_start: u64,
    /// Exclusive end physical address of the kernel image.
    pub kernel_end: u64,
    /// Physical kernel entry point.
    pub entry_point: u64,
    /// UEFI system table physical address, retained for a future runtime
    /// services design. Zero when not available.
    pub system_table: u64,
    /// Reserved; must be zero.
    pub reserved: [u64; 4],
}

impl BootInfo {
    /// Creates an empty block with the current magic and size.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            magic: BOOT_INFO_MAGIC,
            version: BOOT_INFO_VERSION,
            struct_size: size_of::<Self>() as u32,
            memory_regions: 0,
            memory_region_count: 0,
            kernel_start: 0,
            kernel_end: 0,
            entry_point: 0,
            system_table: 0,
            reserved: [0; 4],
        }
    }

    /// Validates the header and kernel range without dereferencing the map.
    pub fn validate(&self) -> Result<(), BootInfoError> {
        if self.magic != BOOT_INFO_MAGIC {
            return Err(BootInfoError::BadMagic);
        }
        if self.version != BOOT_INFO_VERSION {
            return Err(BootInfoError::UnsupportedVersion(self.version));
        }
        let required = size_of::<Self>() as u32;
        if self.struct_size < required {
            return Err(BootInfoError::Truncated {
                reported: self.struct_size,
                required,
            });
        }
        if self.reserved.iter().any(|value| *value != 0) {
            return Err(BootInfoError::ReservedNotZero);
        }
        if self.memory_regions == 0 {
            return Err(BootInfoError::NullMemoryMap);
        }
        if !self
            .memory_regions
            .is_multiple_of(align_of::<MemoryRegion>() as u64)
        {
            return Err(BootInfoError::MemoryMapMisaligned);
        }
        if self.memory_region_count == 0 {
            return Err(BootInfoError::EmptyMemoryMap);
        }
        if self.memory_region_count > MAX_MEMORY_REGIONS as u64 {
            return Err(BootInfoError::TooManyRegions(self.memory_region_count));
        }
        self.memory_region_count
            .checked_mul(size_of::<MemoryRegion>() as u64)
            .ok_or(BootInfoError::MemoryMapOverflow)?;
        if self.kernel_end <= self.kernel_start {
            return Err(BootInfoError::InvalidKernelRange);
        }
        if self.entry_point < self.kernel_start || self.entry_point >= self.kernel_end {
            return Err(BootInfoError::EntryOutsideKernel);
        }
        Ok(())
    }

    /// Returns the memory map.
    ///
    /// # Safety
    ///
    /// [`Self::validate`] must have returned `Ok`, the block must not have
    /// changed since, and the pointed-to memory must remain mapped and
    /// readable.
    #[must_use]
    pub unsafe fn regions(&self) -> &[MemoryRegion] {
        // SAFETY: Guaranteed by the caller.
        unsafe {
            core::slice::from_raw_parts(
                self.memory_regions as *const MemoryRegion,
                self.memory_region_count as usize,
            )
        }
    }

    /// Checks every region for emptiness, overflow, and unknown kinds.
    ///
    /// # Safety
    ///
    /// Same requirements as [`Self::regions`].
    pub unsafe fn validate_regions(&self) -> Result<(), BootInfoError> {
        let mut previous: Option<(u64, u64)> = None;
        // SAFETY: Guaranteed by the caller.
        for (index, region) in unsafe { self.regions() }.iter().enumerate() {
            let index = index as u64;
            let Some(end) = region.end() else {
                return Err(BootInfoError::InvalidRegion { index });
            };
            if region.len == 0
                || end <= region.start
                || region.reserved != 0
                || region.kind().is_none()
            {
                return Err(BootInfoError::InvalidRegion { index });
            }
            if let Some((previous_start, previous_end)) = previous {
                if region.start < previous_start {
                    return Err(BootInfoError::RegionsNotSorted { index });
                }
                if region.start < previous_end {
                    return Err(BootInfoError::OverlappingRegions { index });
                }
            }
            previous = Some((region.start, end));
        }
        Ok(())
    }

    /// Sums allocatable RAM, saturating on overflow.
    ///
    /// # Safety
    ///
    /// Same requirements as [`Self::regions`].
    #[must_use]
    pub unsafe fn usable_bytes(&self) -> u64 {
        // SAFETY: Guaranteed by the caller.
        unsafe { self.regions() }
            .iter()
            .filter(|region| region.kind().is_some_and(MemoryKind::is_allocatable))
            .fold(0u64, |total, region| total.saturating_add(region.len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> BootInfo {
        let mut info = BootInfo::empty();
        info.memory_regions = 0x1000;
        info.memory_region_count = 1;
        info.kernel_start = 0x40_0000;
        info.kernel_end = 0x50_0000;
        info.entry_point = 0x40_0000;
        info
    }

    #[test]
    fn empty_block_is_rejected() {
        assert_eq!(
            BootInfo::empty().validate(),
            Err(BootInfoError::NullMemoryMap)
        );
    }

    #[test]
    fn accepts_well_formed_header() {
        assert_eq!(header().validate(), Ok(()));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut info = header();
        info.magic = 0;
        assert_eq!(info.validate(), Err(BootInfoError::BadMagic));
    }

    #[test]
    fn rejects_future_version() {
        let mut info = header();
        info.version = BOOT_INFO_VERSION + 1;
        assert_eq!(
            info.validate(),
            Err(BootInfoError::UnsupportedVersion(BOOT_INFO_VERSION + 1))
        );
    }

    #[test]
    fn rejects_nonzero_reserved_fields() {
        let mut info = header();
        info.reserved[2] = 1;
        assert_eq!(info.validate(), Err(BootInfoError::ReservedNotZero));
    }

    #[test]
    fn rejects_misaligned_memory_map_pointer() {
        let mut info = header();
        info.memory_regions = 0x1001;
        assert_eq!(info.validate(), Err(BootInfoError::MemoryMapMisaligned));
    }

    #[test]
    fn rejects_excessive_region_count() {
        let mut info = header();
        info.memory_region_count = MAX_MEMORY_REGIONS as u64 + 1;
        assert_eq!(
            info.validate(),
            Err(BootInfoError::TooManyRegions(MAX_MEMORY_REGIONS as u64 + 1))
        );
    }

    #[test]
    fn rejects_entry_outside_kernel() {
        let mut info = header();
        info.entry_point = info.kernel_end;
        assert_eq!(info.validate(), Err(BootInfoError::EntryOutsideKernel));
    }

    #[test]
    fn rejects_inverted_kernel_range() {
        let mut info = header();
        info.kernel_start = info.kernel_end;
        assert_eq!(info.validate(), Err(BootInfoError::InvalidKernelRange));
    }

    #[test]
    fn region_end_detects_overflow() {
        assert_eq!(
            MemoryRegion::new(u64::MAX - 4, 4, MemoryKind::Usable).end(),
            Some(u64::MAX)
        );
        assert_eq!(
            MemoryRegion::new(u64::MAX, 1, MemoryKind::Usable).end(),
            None
        );
    }

    #[test]
    fn allocatable_kinds_are_narrow() {
        assert!(MemoryKind::Usable.is_allocatable());
        assert!(MemoryKind::BootServices.is_allocatable());
        assert!(MemoryKind::AcpiReclaimable.is_allocatable());
        assert!(!MemoryKind::Reserved.is_allocatable());
        assert!(!MemoryKind::Loader.is_allocatable());
        assert!(!MemoryKind::RuntimeServices.is_allocatable());
        assert!(!MemoryKind::Mmio.is_allocatable());
    }

    #[test]
    fn raw_kinds_round_trip_and_reject_unknown_values() {
        for kind in [
            MemoryKind::Usable,
            MemoryKind::Reserved,
            MemoryKind::AcpiReclaimable,
            MemoryKind::AcpiNvs,
            MemoryKind::Unusable,
            MemoryKind::BootServices,
            MemoryKind::RuntimeServices,
            MemoryKind::Loader,
            MemoryKind::Mmio,
            MemoryKind::Other,
        ] {
            assert_eq!(MemoryKind::from_raw(kind as u32), Some(kind));
            assert_eq!(MemoryRegion::new(0, 4096, kind).kind(), Some(kind));
        }
        assert_eq!(MemoryKind::from_raw(10), None);
        assert_eq!(MemoryKind::from_raw(u32::MAX), None);
    }

    #[test]
    fn layout_is_stable() {
        // These sizes are part of the loader/kernel contract.
        assert_eq!(size_of::<MemoryKind>(), 4);
        assert_eq!(size_of::<MemoryRegion>(), 24);
        assert_eq!(size_of::<BootInfo>(), 96);
    }

    #[test]
    fn accepts_sorted_adjacent_regions() {
        let regions = [
            MemoryRegion::new(0x1000, 0x1000, MemoryKind::Usable),
            MemoryRegion::new(0x2000, 0x1000, MemoryKind::BootServices),
        ];
        let mut info = header();
        info.memory_regions = regions.as_ptr() as u64;
        info.memory_region_count = regions.len() as u64;
        assert_eq!(unsafe { info.validate_regions() }, Ok(()));
    }

    #[test]
    fn rejects_unsorted_regions() {
        let regions = [
            MemoryRegion::new(0x2000, 0x1000, MemoryKind::Usable),
            MemoryRegion::new(0x1000, 0x1000, MemoryKind::Usable),
        ];
        let mut info = header();
        info.memory_regions = regions.as_ptr() as u64;
        info.memory_region_count = regions.len() as u64;
        assert_eq!(
            unsafe { info.validate_regions() },
            Err(BootInfoError::RegionsNotSorted { index: 1 })
        );
    }

    #[test]
    fn rejects_overlapping_regions() {
        let regions = [
            MemoryRegion::new(0x1000, 0x2000, MemoryKind::Usable),
            MemoryRegion::new(0x2000, 0x1000, MemoryKind::Reserved),
        ];
        let mut info = header();
        info.memory_regions = regions.as_ptr() as u64;
        info.memory_region_count = regions.len() as u64;
        assert_eq!(
            unsafe { info.validate_regions() },
            Err(BootInfoError::OverlappingRegions { index: 1 })
        );
    }

    #[test]
    fn rejects_region_with_unknown_kind() {
        let regions = [MemoryRegion {
            start: 0x1000,
            len: 0x1000,
            kind: 42,
            reserved: 0,
        }];
        let mut info = header();
        info.memory_regions = regions.as_ptr() as u64;
        info.memory_region_count = 1;
        assert_eq!(
            unsafe { info.validate_regions() },
            Err(BootInfoError::InvalidRegion { index: 0 })
        );
    }
}

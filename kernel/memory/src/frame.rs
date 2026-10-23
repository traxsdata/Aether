// SPDX-License-Identifier: Apache-2.0
//! Bitmap physical-frame allocator.
//!
//! The allocator is deliberately simple and deterministic: one bit per 4 KiB
//! frame, one meaning "in use". It does not depend on the heap, allocation
//! cannot block, and every operation is bounded by the bitmap size.
//!
//! Only memory the kernel may safely own is freed during initialization:
//! conventional RAM and boot-services memory (which is free after
//! `ExitBootServices`). ACPI reclaimable memory stays reserved until an ACPI
//! parser explicitly releases it. Firmware, loader, kernel, and MMIO ranges
//! are never handed out.
//!
//! The first 1 MiB is always reserved. Firmware data, the real-mode IVT, and
//! legacy device memory live there, and the cost of skipping it is irrelevant
//! on the supported platform.

use crate::{PAGE_SIZE, align_down, align_up};
use aether_abi::{MemoryKind, MemoryRegion};

/// Lowest physical address the allocator will hand out.
pub const FIRST_ALLOCATABLE_ADDRESS: u64 = 0x10_0000;

/// A 4 KiB physical frame, identified by its frame index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysFrame(u64);

impl PhysFrame {
    /// Creates a frame from a frame index.
    #[must_use]
    pub const fn from_index(index: u64) -> Self {
        Self(index)
    }

    /// Creates a frame from a page-aligned physical address.
    #[must_use]
    pub const fn from_address(address: u64) -> Option<Self> {
        if address & (PAGE_SIZE - 1) != 0 {
            None
        } else {
            Some(Self(address / PAGE_SIZE))
        }
    }

    /// Frame index.
    #[must_use]
    pub const fn index(self) -> u64 {
        self.0
    }

    /// Physical start address.
    #[must_use]
    pub const fn start_address(self) -> u64 {
        self.0 * PAGE_SIZE
    }
}

/// Errors from explicit frame operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame index lies outside the allocator's bitmap.
    OutOfRange,
    /// The address or length is not page-aligned where required.
    NotAligned,
    /// The frame was already free.
    DoubleFree,
}

/// Snapshot of allocator capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStats {
    /// Total frames representable by the bitmap.
    pub total_frames: u64,
    /// Frames currently available for allocation.
    pub free_frames: u64,
}

impl FrameStats {
    /// Total bytes representable by the bitmap.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.total_frames * PAGE_SIZE
    }

    /// Bytes currently available for allocation.
    #[must_use]
    pub const fn free_bytes(self) -> u64 {
        self.free_frames * PAGE_SIZE
    }
}

/// Bitmap frame allocator.
pub struct FrameAllocator<'a> {
    bitmap: &'a mut [u8],
    frame_count: u64,
    next_hint: u64,
    free_frames: u64,
}

impl<'a> FrameAllocator<'a> {
    /// Creates an allocator in which every frame is in use.
    ///
    /// Call [`Self::initialize`] with the validated boot memory map to release
    /// usable memory.
    #[must_use]
    pub fn new(bitmap: &'a mut [u8]) -> Self {
        bitmap.fill(0xff);
        let frame_count = (bitmap.len() as u64) * 8;
        Self {
            bitmap,
            frame_count,
            next_hint: FIRST_ALLOCATABLE_ADDRESS / PAGE_SIZE,
            free_frames: 0,
        }
    }

    /// Builds an allocator from a validated boot memory map.
    ///
    /// Only conventional (`Usable`) memory is released here. Boot-services
    /// memory is not freed until the kernel has switched away from the firmware
    /// page tables: firmware may keep its active page tables in boot-services
    /// data, and reusing those frames before the CR3 switch would corrupt the
    /// running address space. The kernel calls [`Self::free_range`] for
    /// boot-services regions after switching.
    ///
    /// `kernel_start..kernel_end` is reserved even though the loader should
    /// already describe it as loader-owned. This is a fail-closed redundancy:
    /// a future loader bug must not let the kernel allocate over itself.
    pub fn initialize(
        &mut self,
        regions: &[MemoryRegion],
        kernel_start: u64,
        kernel_end: u64,
    ) -> FrameStats {
        self.bitmap.fill(0xff);
        self.free_frames = 0;
        self.next_hint = FIRST_ALLOCATABLE_ADDRESS / PAGE_SIZE;

        for region in regions {
            if region.kind() == Some(MemoryKind::Usable) {
                self.free_range(region.start, region.len);
            }
        }

        self.reserve_range(0, FIRST_ALLOCATABLE_ADDRESS);
        if kernel_end > kernel_start {
            self.reserve_range(kernel_start, kernel_end - kernel_start);
        }

        self.stats()
    }

    /// Current allocator capacity.
    #[must_use]
    pub const fn stats(&self) -> FrameStats {
        FrameStats {
            total_frames: self.frame_count,
            free_frames: self.free_frames,
        }
    }

    /// Allocates one frame, returning `None` when memory is exhausted.
    pub fn allocate(&mut self) -> Option<PhysFrame> {
        if self.free_frames == 0 {
            return None;
        }
        let index = self.find_free(self.next_hint)?;
        self.set_used(index);
        self.next_hint = if index + 1 >= self.frame_count {
            0
        } else {
            index + 1
        };
        Some(PhysFrame::from_index(index))
    }

    /// Allocates `count` physically contiguous frames.
    ///
    /// The kernel identity map lets a caller treat the returned run as a
    /// contiguous byte buffer. Returns `None` when no free run is large enough
    /// or `count` is zero.
    pub fn allocate_contiguous(&mut self, count: u64) -> Option<PhysFrame> {
        if count == 0 || count > self.free_frames {
            return None;
        }
        let mut run_start = 0;
        let mut run_len = 0;
        let mut index = 0;
        while index < self.frame_count {
            if self.is_used(index) {
                run_len = 0;
                index += 1;
                continue;
            }
            if run_len == 0 {
                run_start = index;
            }
            run_len += 1;
            if run_len == count {
                for frame in run_start..run_start + count {
                    self.set_used(frame);
                }
                self.next_hint = if run_start + count >= self.frame_count {
                    0
                } else {
                    run_start + count
                };
                return Some(PhysFrame::from_index(run_start));
            }
            index += 1;
        }
        None
    }

    /// Releases a previously allocated frame.
    pub fn deallocate(&mut self, frame: PhysFrame) -> Result<(), FrameError> {
        if frame.index() >= self.frame_count {
            return Err(FrameError::OutOfRange);
        }
        if !self.is_used(frame.index()) {
            return Err(FrameError::DoubleFree);
        }
        self.set_free(frame.index());
        if frame.index() < self.next_hint {
            self.next_hint = frame.index();
        }
        Ok(())
    }

    /// Marks a physical range as in use.
    ///
    /// The range is rounded outward to whole frames, which may reserve a few
    /// bytes on either side. Overflowing or out-of-bitmap parts are ignored.
    pub fn reserve_range(&mut self, start: u64, len: u64) {
        if len == 0 {
            return;
        }
        let Some(end) = start.checked_add(len) else {
            return;
        };
        let start = align_down(start);
        let Some(end) = align_up(end) else {
            return;
        };
        let end = end.min(self.max_address());
        let mut frame = start / PAGE_SIZE;
        let last = end / PAGE_SIZE;
        while frame < last {
            self.set_used(frame);
            frame += 1;
        }
    }

    /// Frees page-aligned, allocatable frames in a physical range.
    ///
    /// Partial pages at either end are trimmed rather than freeing bytes the
    /// allocator cannot represent. Memory below
    /// [`FIRST_ALLOCATABLE_ADDRESS`] and beyond the bitmap is ignored.
    pub fn free_range(&mut self, start: u64, len: u64) {
        if len == 0 {
            return;
        }
        let Some(raw_end) = start.checked_add(len) else {
            return;
        };
        let start = align_up(start.max(FIRST_ALLOCATABLE_ADDRESS)).unwrap_or(u64::MAX);
        let end = align_down(raw_end).min(self.max_address());
        if start >= end {
            return;
        }
        let mut frame = start / PAGE_SIZE;
        let last = end / PAGE_SIZE;
        while frame < last {
            self.set_free(frame);
            frame += 1;
        }
    }

    /// One past the highest frame that is free now, or 0 when none is.
    ///
    /// Frames above this limit are already in use and so are kernel-owned
    /// until freed, which lets a per-frame table cover only the memory that
    /// can still reach an address space.
    #[must_use]
    pub fn free_frame_limit(&self) -> u64 {
        self.bitmap
            .iter()
            .rposition(|byte| *byte != 0xff)
            .map_or(0, |byte| {
                let bits = self.bitmap[byte];
                let bit = 7 - u64::from((!bits).leading_zeros());
                (byte as u64 * 8 + bit + 1).min(self.frame_count)
            })
    }

    fn max_address(&self) -> u64 {
        self.frame_count * PAGE_SIZE
    }

    fn find_free(&self, from: u64) -> Option<u64> {
        if self.frame_count == 0 {
            return None;
        }
        let from = from.min(self.frame_count - 1);
        for index in from..self.frame_count {
            if !self.is_used(index) {
                return Some(index);
            }
        }
        (0..from).find(|index| !self.is_used(*index))
    }

    fn is_used(&self, index: u64) -> bool {
        let byte = (index / 8) as usize;
        let bit = (index % 8) as u8;
        self.bitmap[byte] & (1 << bit) != 0
    }

    fn set_used(&mut self, index: u64) {
        let byte = (index / 8) as usize;
        let bit = (index % 8) as u8;
        let mask = 1 << bit;
        if self.bitmap[byte] & mask == 0 {
            self.bitmap[byte] |= mask;
            self.free_frames -= 1;
        }
    }

    fn set_free(&mut self, index: u64) {
        let byte = (index / 8) as usize;
        let bit = (index % 8) as u8;
        let mask = 1 << bit;
        if self.bitmap[byte] & mask != 0 {
            self.bitmap[byte] &= !mask;
            self.free_frames += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    const MIB: u64 = 1024 * 1024;

    fn region(start: u64, len: u64, kind: MemoryKind) -> MemoryRegion {
        MemoryRegion::new(start, len, kind)
    }

    fn allocator(bitmap_bytes: usize) -> Vec<u8> {
        vec![0u8; bitmap_bytes]
    }

    #[test]
    fn frees_only_conventional_memory_during_initialization() {
        let mut bitmap = allocator(4096); // 32768 frames = 128 MiB
        let regions = [
            region(0, MIB, MemoryKind::Reserved),
            region(MIB, 8 * MIB, MemoryKind::Usable),
            region(9 * MIB, 4 * MIB, MemoryKind::BootServices),
            region(13 * MIB, 4 * MIB, MemoryKind::AcpiReclaimable),
            region(17 * MIB, 4 * MIB, MemoryKind::Loader),
            region(21 * MIB, 4 * MIB, MemoryKind::Mmio),
        ];
        let mut frames = FrameAllocator::new(&mut bitmap);
        let stats = frames.initialize(&regions, MIB, 2 * MIB);

        assert_eq!(stats.free_frames, 8 * 256 - 256);
        for _ in 0..100 {
            let frame = frames.allocate().expect("free memory");
            assert!(frame.start_address() >= FIRST_ALLOCATABLE_ADDRESS);
        }

        // Boot-services memory is released only after the CR3 switch.
        frames.free_range(9 * MIB, 4 * MIB);
        assert_eq!(
            frames.stats().free_frames,
            stats.free_frames - 100 + 4 * 256
        );
    }

    #[test]
    fn reports_one_past_the_highest_free_frame() {
        let mut bitmap = allocator(64); // 512 frames = 2 MiB
        let mut frames = FrameAllocator::new(&mut bitmap);
        assert_eq!(frames.free_frame_limit(), 0);
        frames.free_range(MIB, 4096 * 3);
        assert_eq!(frames.free_frame_limit(), 256 + 3);
        frames.free_range(2 * MIB - 4096, 4096);
        assert_eq!(frames.free_frame_limit(), 512);
    }

    #[test]
    fn never_allocates_below_one_mib() {
        let mut bitmap = allocator(64);
        let regions = [region(0, 4 * MIB, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, MIB, MIB);
        while let Some(frame) = frames.allocate() {
            assert!(frame.start_address() >= FIRST_ALLOCATABLE_ADDRESS);
        }
    }

    #[test]
    fn reserves_the_kernel_range_even_if_the_map_claims_it_is_usable() {
        let mut bitmap = allocator(64);
        let regions = [region(0, 8 * MIB, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, 4 * MIB, 5 * MIB);

        while let Some(frame) = frames.allocate() {
            let address = frame.start_address();
            assert!(!(4 * MIB..5 * MIB).contains(&address));
        }
    }

    #[test]
    fn allocation_is_deterministic_and_reports_empty() {
        let mut bitmap = allocator(8); // 64 frames = 256 KiB
        let regions = [region(0, 256 * 1024, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, 0, 0);
        // Everything is below 1 MiB, so nothing is allocatable.
        assert_eq!(frames.stats().free_frames, 0);
        assert!(frames.allocate().is_none());
    }

    #[test]
    fn allocation_respects_bitmap_capacity() {
        let mut bitmap = allocator(2); // 16 frames = 64 KiB
        let regions = [region(0, 64 * MIB, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        let stats = frames.initialize(&regions, 0, 0);
        assert_eq!(stats.total_frames, 16);
        assert!(stats.free_frames <= 16);
    }

    #[test]
    fn deallocate_rejects_double_free_and_out_of_range() {
        let mut bitmap = allocator(64);
        let regions = [region(0, 8 * MIB, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, 0, 0);

        let frame = frames.allocate().expect("frame");
        frames.deallocate(frame).expect("first free");
        assert_eq!(frames.deallocate(frame), Err(FrameError::DoubleFree));
        assert_eq!(
            frames.deallocate(PhysFrame::from_index(u64::MAX)),
            Err(FrameError::OutOfRange)
        );
    }

    #[test]
    fn allocates_a_contiguous_run_and_rejects_when_too_large() {
        let mut bitmap = allocator(64); // 512 frames = 2 MiB
        let regions = [region(0, 8 * MIB, MemoryKind::Usable)];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, 0, 0);

        // Reserve a frame in the middle of the low free range to force the
        // contiguous search to skip over it.
        let hole = frames.allocate().expect("frame");
        frames.allocate().expect("frame");
        frames.allocate().expect("frame");
        let before = frames.stats().free_frames;
        let run = frames.allocate_contiguous(4).expect("contiguous run");
        assert_eq!(run.start_address() % PAGE_SIZE, 0);
        assert!(
            !(run.start_address()..run.start_address() + 4 * PAGE_SIZE)
                .contains(&hole.start_address())
        );
        assert_eq!(frames.stats().free_frames, before - 4);

        assert!(frames.allocate_contiguous(0).is_none());
        assert!(frames.allocate_contiguous(before).is_none());
    }

    #[test]
    fn reserve_rounds_outward_and_trims_free_ranges() {
        let mut bitmap = allocator(256); // 2048 frames = 8 MiB
        let regions = [
            region(0, 8 * MIB, MemoryKind::Usable),
            region(8 * MIB + 1, 4095, MemoryKind::Usable),
        ];
        let mut frames = FrameAllocator::new(&mut bitmap);
        frames.initialize(&regions, 0, 0);
        let before = frames.stats().free_frames;
        frames.reserve_range(2 * MIB + 1, 4096);
        assert_eq!(frames.stats().free_frames, before - 2);
    }

    #[test]
    fn frame_address_conversion_requires_alignment() {
        assert_eq!(
            PhysFrame::from_address(4096),
            Some(PhysFrame::from_index(1))
        );
        assert_eq!(PhysFrame::from_address(1), None);
        assert_eq!(PhysFrame::from_index(3).start_address(), 3 * PAGE_SIZE);
    }
}

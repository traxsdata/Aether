// SPDX-License-Identifier: Apache-2.0
//! Kernel physical-memory and paging support.

pub mod paging;

use aether_memory::PAGE_SIZE;
use aether_memory::frame::FrameAllocator;

/// Maximum physical address space represented by the frame bitmap.
///
/// The bitmap is a fixed 512 KiB array in the kernel image: 16 GiB / 4 KiB / 8
/// bits. Memory above this limit is left to a later, dynamically sized
/// allocator. This bound is documented as a Phase 1 limitation.
pub const MAX_PHYSICAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Number of bytes in the frame bitmap.
pub const FRAME_BITMAP_BYTES: usize = (MAX_PHYSICAL_BYTES / PAGE_SIZE / 8) as usize;

/// One bit per 4 KiB frame, all initially in use.
static mut FRAME_BITMAP: [u8; FRAME_BITMAP_BYTES] = [0; FRAME_BITMAP_BYTES];

/// Creates the kernel frame allocator.
///
/// The allocator borrows static bitmap storage, so only one instance may exist.
/// Phase 1 is single-threaded with interrupts disabled. A later scheduler must
/// replace this with a lock or per-CPU allocator before enabling concurrency.
#[must_use]
pub fn frame_allocator() -> FrameAllocator<'static> {
    // SAFETY: This is the only place that borrows the bitmap, it is borrowed
    // for the rest of the program, and no other reference to it exists.
    let bitmap = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(FRAME_BITMAP).cast::<u8>(),
            FRAME_BITMAP_BYTES,
        )
    };
    FrameAllocator::new(bitmap)
}

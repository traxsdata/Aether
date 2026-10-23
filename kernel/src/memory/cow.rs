// SPDX-License-Identifier: Apache-2.0
//! Reference counts for private frames shared copy-on-write after `fork`.
//!
//! `fork` maps each private frame of the parent into the child read-only and
//! counts the extra owner here, instead of copying the page. The first write
//! from either side copies the frame (see `paging::resolve_cow`), so a large
//! process forks in time and memory proportional to its page tables, not its
//! data.
//!
//! The table holds one `u16` per physical frame, below the highest frame that
//! was free at boot, in frames taken from the allocator. A count of 0 means
//! one owner, so the table starts zeroed; a shared frame counts every owner.
//! A frame outside the table, or one whose count would overflow, is never
//! shared: `fork` copies it eagerly as before. If the table cannot be
//! allocated, every `fork` copies eagerly.
//!
//! The kernel is single-CPU and these paths run with interrupts masked, like
//! the frame allocator and the shared-frame registry. A later SMP port must add
//! a lock here.

use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use core::sync::atomic::{AtomicU64, Ordering};

/// Physical address of the count table, or 0 before [`init`] succeeds.
static TABLE: AtomicU64 = AtomicU64::new(0);
/// Frames the table covers.
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Allocates the count table. Returns the number of frames it covers, or
/// `None` when memory for it was not available.
pub fn init(allocator: &mut FrameAllocator<'_>) -> Option<u64> {
    let frames = allocator.free_frame_limit();
    if frames == 0 {
        return None;
    }
    let pages = (frames * 2).div_ceil(PAGE_SIZE);
    let start = allocator.allocate_contiguous(pages)?;
    // SAFETY: The run was just allocated, is identity-mapped and owned by this
    // table for the rest of the kernel's life.
    unsafe {
        core::ptr::write_bytes(
            start.start_address() as *mut u8,
            0,
            (pages * PAGE_SIZE) as usize,
        );
    }
    TABLE.store(start.start_address(), Ordering::Release);
    FRAMES.store(frames, Ordering::Release);
    Some(frames)
}

fn count(frame: PhysFrame) -> Option<*mut u16> {
    let table = TABLE.load(Ordering::Acquire);
    if table == 0 || frame.index() >= FRAMES.load(Ordering::Acquire) {
        return None;
    }
    Some((table as *mut u16).wrapping_add(frame.index() as usize))
}

/// Adds an owner to a private frame. Returns `false` when the frame cannot be
/// shared, and the caller must copy it instead.
pub fn share(frame: PhysFrame) -> bool {
    let Some(count) = count(frame) else {
        return false;
    };
    // SAFETY: `count` points into the table, which only these functions touch,
    // serialized by the single-CPU kernel.
    unsafe {
        match *count {
            0 => *count = 2,
            u16::MAX => return false,
            owners => *count = owners + 1,
        }
    }
    true
}

/// Drops one owner of a private frame. Returns `true` when another owner is
/// left, so the caller must not free it.
pub fn release(frame: PhysFrame) -> bool {
    let Some(count) = count(frame) else {
        return false;
    };
    // SAFETY: As in `share`.
    unsafe {
        match *count {
            0 => false,
            2 => {
                *count = 0;
                true
            }
            owners => {
                *count = owners - 1;
                true
            }
        }
    }
}

/// Whether more than one address space owns `frame`.
#[must_use]
pub fn is_shared(frame: PhysFrame) -> bool {
    // SAFETY: As in `share`.
    count(frame).is_some_and(|count| unsafe { *count } != 0)
}

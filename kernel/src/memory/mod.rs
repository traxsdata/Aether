// SPDX-License-Identifier: Apache-2.0
//! Kernel physical-memory and paging support.

mod heap;
pub mod paging;
pub mod shared;

pub use heap::{has_headroom as heap_has_headroom, stats as heap_stats};

/// Reserves the kernel heap arena from the frame allocator.
///
/// Must run after the kernel has switched to its own page tables and released
/// boot-services memory, so the arena comes from ordinary usable RAM rather
/// than a fixed range in the kernel ELF.
pub fn init_heap() -> Option<(usize, usize)> {
    let bytes = heap::requested_bytes();
    let start = with_frame_allocator(|frames| heap::init(frames, bytes))??;
    Some((start, bytes))
}

use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, FrameStats};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Allocatable RAM the boot memory map reported, for `free` and `htop`. The
/// frame bitmap's own size is the addressable range, not installed memory.
pub static USABLE_BYTES: AtomicU64 = AtomicU64::new(0);

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

struct AllocatorCell(UnsafeCell<MaybeUninit<FrameAllocator<'static>>>);

// SAFETY: The kernel is single-CPU. Every access goes through
// `with_frame_allocator`, which lends the allocator only for the duration of a
// closure that cannot re-enter the allocator.
unsafe impl Sync for AllocatorCell {}

static FRAME_ALLOCATOR: AllocatorCell = AllocatorCell(UnsafeCell::new(MaybeUninit::uninit()));
static FRAME_ALLOCATOR_READY: AtomicBool = AtomicBool::new(false);

/// Creates the kernel frame allocator and stores it for runtime use.
///
/// The allocator borrows static bitmap storage, so only one instance may exist.
/// The kernel is single-threaded and interrupts are masked around allocator
/// use, so a lock is not needed. A later SMP port must replace this with a
/// per-CPU allocator or a spin lock.
pub fn init_frame_allocator() {
    // SAFETY: This is the only place that borrows the bitmap, it is borrowed
    // for the rest of the program, and no other reference to it exists.
    let bitmap = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(FRAME_BITMAP).cast::<u8>(),
            FRAME_BITMAP_BYTES,
        )
    };
    let allocator = FrameAllocator::new(bitmap);
    // SAFETY: The cell is written once before any reader can observe readiness.
    unsafe { (*FRAME_ALLOCATOR.0.get()).write(allocator) };
    FRAME_ALLOCATOR_READY.store(true, Ordering::Release);
}

/// Runs `operation` with the kernel frame allocator.
///
/// Returns `None` before [`init_frame_allocator`] has run. The borrow ends when
/// `operation` returns; do not re-enter this function from the closure.
pub fn with_frame_allocator<R>(
    operation: impl FnOnce(&mut FrameAllocator<'static>) -> R,
) -> Option<R> {
    if !FRAME_ALLOCATOR_READY.load(Ordering::Acquire) {
        return None;
    }
    // SAFETY: Readiness was published after initialization. The single-CPU
    // kernel serializes allocator use with interrupt masking, and the closure
    // receives the only live reference for its duration.
    let allocator = unsafe { (*FRAME_ALLOCATOR.0.get()).assume_init_mut() };
    Some(operation(allocator))
}

/// Returns allocator capacity for diagnostics.
#[must_use]
pub fn frame_stats() -> Option<FrameStats> {
    with_frame_allocator(|allocator| allocator.stats())
}

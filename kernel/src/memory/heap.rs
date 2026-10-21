// SPDX-License-Identifier: Apache-2.0
//! Kernel bump heap for import-time allocations.
//!
//! The kernel has no general-purpose allocator. The Linux rootfs importer needs
//! dynamic buffers for decompression and the imported file table, so this
//! module provides a bounded, single-CPU arena for bootstrap work. It never
//! frees: the importer runs once, and the imported root lives for the rest of
//! the boot. A later runtime allocator must replace this for long-lived
//! services.
//!
//! The arena is allocated from the frame allocator after the kernel has
//! switched to its own page tables. It is deliberately not a static array: a
//! 1.5 GiB `.bss` segment forces the UEFI loader to reserve a fixed 1.6 GiB
//! physical range before the OS starts, which fails on firmware that has
//! fragmented that range even when the VM has plenty of RAM.

use aether_memory::PAGE_SIZE;
use aether_memory::frame::FrameAllocator;
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

// The importer never reclaims, so every decompression scratch buffer and every
// imported file accumulates for the rest of the boot. A real 19 MiB AppImage
// carries both GridCraft binaries (~48 MiB uncompressed) plus hundreds of zstd
// scratch buffers; 64 MiB and 256 MiB both exhausted during that import. Keep
// the arena large enough for the largest staged fixture while the importer is
// allocation-only; replacing it with a reclaiming allocator is follow-up work.
//
// The Gate 4 Mesa/lavapipe rootfs carries ~164 MiB of uncompressed libraries
// (LLVM alone is ~139 MiB). The allocation-only importer uses roughly 1 GiB
// for that tree because decompression scratch is never reclaimed, so the
// profile selects a 1.5 GiB arena. A reclaiming allocator is the follow-up that
// removes this multiplier.
#[cfg(feature = "gate4-runtime")]
const HEAP_BYTES: usize = 1536 * 1024 * 1024;
#[cfg(not(feature = "gate4-runtime"))]
const HEAP_BYTES: usize = 512 * 1024 * 1024;

static HEAP_START: AtomicUsize = AtomicUsize::new(0);
static HEAP_CAPACITY: AtomicUsize = AtomicUsize::new(0);
static OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Arena size selected by the build profile.
#[must_use]
pub const fn requested_bytes() -> usize {
    HEAP_BYTES
}

/// Reserves the arena from the frame allocator.
///
/// Returns the physical start address. The kernel identity map makes that
/// address directly usable as a byte buffer.
pub fn init(frames: &mut FrameAllocator<'_>, bytes: usize) -> Option<usize> {
    let count = (bytes as u64 / PAGE_SIZE) as usize;
    if count == 0 {
        return None;
    }
    let start = frames.allocate_contiguous(count as u64)?;
    let base = start.start_address() as usize;
    HEAP_START.store(base, Ordering::Release);
    HEAP_CAPACITY.store(count * PAGE_SIZE as usize, Ordering::Release);
    OFFSET.store(0, Ordering::Release);
    Some(base)
}

/// Bytes reserved by the bump heap since boot.
#[must_use]
pub fn used_bytes() -> usize {
    OFFSET.load(Ordering::Relaxed)
}

/// Bounded bump allocator.
pub struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = HEAP_START.load(Ordering::Acquire);
        let capacity = HEAP_CAPACITY.load(Ordering::Acquire);
        if base == 0 || capacity == 0 {
            return core::ptr::null_mut();
        }
        let mut current = OFFSET.load(Ordering::Relaxed);
        loop {
            let align = layout.align().max(1);
            let Some(aligned) = current
                .checked_add(align - 1)
                .map(|value| value & !(align - 1))
            else {
                return core::ptr::null_mut();
            };
            let Some(end) = aligned.checked_add(layout.size()) else {
                return core::ptr::null_mut();
            };
            if end > capacity {
                crate::serial_line!(
                    "heap: allocation failed size={} align={} used={} capacity={}",
                    layout.size(),
                    align,
                    current,
                    capacity
                );
                return core::ptr::null_mut();
            }
            match OFFSET.compare_exchange(current, end, Ordering::SeqCst, Ordering::Relaxed) {
                Ok(_) => {
                    // SAFETY: `aligned..end` lies inside the frame-allocated
                    // arena and was reserved by the successful compare-exchange.
                    return unsafe { (base as *mut u8).add(aligned) };
                }
                Err(value) => current = value,
            }
        }
    }

    unsafe fn dealloc(&self, _pointer: *mut u8, _layout: Layout) {
        // The bootstrap heap never reclaims. Import runs once.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator;

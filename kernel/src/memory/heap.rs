// SPDX-License-Identifier: Apache-2.0
//! Fixed kernel bump heap for import-time allocations.
//!
//! The kernel has no general-purpose allocator. The Linux rootfs importer needs
//! dynamic buffers for decompression and the imported file table, so this
//! module provides a bounded, single-CPU arena for bootstrap work. It never
//! frees: the importer runs once, and the imported root lives for the rest of
//! the boot. A later runtime allocator must replace this for long-lived
//! services.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_BYTES: usize = 4 * 1024 * 1024;

static mut HEAP: [u8; HEAP_BYTES] = [0; HEAP_BYTES];
static OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Bounded bump allocator.
pub struct BumpAllocator;

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
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
            if end > HEAP_BYTES {
                return core::ptr::null_mut();
            }
            match OFFSET.compare_exchange(current, end, Ordering::SeqCst, Ordering::Relaxed) {
                Ok(_) => {
                    // SAFETY: `aligned..end` lies inside the static heap and was
                    // reserved by the successful compare-exchange.
                    return unsafe { core::ptr::addr_of_mut!(HEAP).cast::<u8>().add(aligned) };
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

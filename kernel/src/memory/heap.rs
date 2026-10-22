// SPDX-License-Identifier: Apache-2.0
//! Kernel heap.
//!
//! The arena is reserved once from the frame allocator after the kernel has
//! switched to its own page tables, and handed to the reclaiming
//! [`aether_memory::heap::Heap`]: small requests come from size-class slabs and
//! large ones from a coalescing free list, so freed import scratch and runtime
//! buffers are reused instead of accumulating for the rest of the boot.
//!
//! The arena is deliberately not a static array: a 1.5 GiB `.bss` segment
//! forces the UEFI loader to reserve a fixed 1.6 GiB physical range before the
//! OS starts, which fails on firmware that has fragmented that range even when
//! the VM has plenty of RAM.
//!
//! The kernel runs on one CPU. Every heap operation masks interrupts for its
//! duration, so an interrupt handler that allocates cannot observe a half-updated
//! free list.

use crate::arch::x86_64::timer::InterruptGuard;
use aether_memory::PAGE_SIZE;
use aether_memory::frame::FrameAllocator;
use aether_memory::heap::{Heap, HeapStats};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

// The Gate 4 Mesa/lavapipe rootfs carries ~164 MiB of uncompressed libraries
// (LLVM alone is ~139 MiB) that stay resident as the imported file table, so
// the profile keeps a larger arena. Decompression scratch is now reclaimed, so
// neither size carries the old never-free multiplier.
#[cfg(feature = "gate4-runtime")]
const HEAP_BYTES: usize = 1536 * 1024 * 1024;
#[cfg(not(feature = "gate4-runtime"))]
const HEAP_BYTES: usize = 512 * 1024 * 1024;

struct KernelHeap(UnsafeCell<Heap>);

// SAFETY: Access goes through `with_heap`, which masks interrupts on the only
// CPU the kernel runs on, so there is never more than one live reference.
unsafe impl Sync for KernelHeap {}

static HEAP: KernelHeap = KernelHeap(UnsafeCell::new(Heap::empty()));

fn with_heap<R>(f: impl FnOnce(&mut Heap) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked on the single kernel CPU, and no heap
    // operation calls back into the heap, so this is the only reference.
    f(unsafe { &mut *HEAP.0.get() })
}

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
    let count = bytes as u64 / PAGE_SIZE;
    if count == 0 {
        return None;
    }
    let start = frames.allocate_contiguous(count)?;
    let base = start.start_address() as usize;
    // SAFETY: The frames were just allocated for the heap alone, are
    // identity-mapped, and are never returned to the frame allocator.
    let installed = with_heap(|heap| unsafe { heap.init(base, (count * PAGE_SIZE) as usize) });
    installed.then_some(base)
}

/// Current heap counters.
#[must_use]
pub fn stats() -> HeapStats {
    with_heap(|heap| heap.stats())
}

/// Free heap a syscall keeps in reserve before it grows a kernel table or
/// queue for user space: two slabs, so one more size class can refill.
const HEADROOM: usize = 2 * aether_memory::heap::SLAB_BYTES;

/// Whether the heap can take a syscall-sized allocation without running
/// dry. Paths whose collections cannot report failure (`BTreeMap`, `Rc`)
/// check this first and return `ENOMEM`, because an exhausted kernel heap
/// panics instead.
#[must_use]
pub fn has_headroom() -> bool {
    stats().free_large >= HEADROOM
}

/// Global allocator backed by the kernel heap.
pub struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let block = with_heap(|heap| heap.allocate(layout));
        if block.is_null() {
            let stats = stats();
            crate::serial_line!(
                "heap: allocation failed size={} align={} used={} free={} capacity={}",
                layout.size(),
                layout.align(),
                stats.used,
                stats.free_large,
                stats.capacity
            );
        }
        block
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: GlobalAlloc's contract: `pointer` came from `alloc` with
        // this layout and is not used again.
        with_heap(|heap| unsafe { heap.deallocate(pointer, layout) });
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

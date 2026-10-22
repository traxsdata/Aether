// SPDX-License-Identifier: Apache-2.0
//! Reclaiming heap allocator over one contiguous region.
//!
//! The kernel hands this allocator a frame-allocated arena once at boot. Two
//! tiers share it:
//!
//! - **Small blocks** (at most [`MAX_SMALL`] bytes and alignment) come from
//!   power-of-two size classes. A class refills by carving a [`SLAB_BYTES`]
//!   slab out of the large tier, and freed blocks go back on their class list.
//!   Slabs are not returned to the large tier, so small-block memory is reused
//!   only by the same class.
//! - **Large blocks** come from an address-ordered free list with first-fit
//!   placement in [`GRANULE`]-byte units. A free coalesces with both
//!   neighbours, so freeing every large block restores one free extent.
//!
//! Neither tier stores a header: Rust's allocator API passes the same layout
//! to `deallocate` as to `allocate`, and both sides round it identically.
//! Free-list nodes live inside the free memory they describe.
//!
//! The type is not synchronized. The kernel wraps it in a critical section
//! that masks interrupts; host tests use it directly.

use core::alloc::Layout;
use core::ptr;

/// Allocation unit and minimum alignment of the large tier.
pub const GRANULE: usize = 64;
/// Smallest size class.
pub const MIN_SMALL: usize = 16;
/// Largest size class; bigger or more strictly aligned requests are large.
pub const MAX_SMALL: usize = 2048;
/// Bytes carved from the large tier when a size class runs empty.
pub const SLAB_BYTES: usize = 64 * 1024;

const CLASSES: usize = (MAX_SMALL / MIN_SMALL).trailing_zeros() as usize + 1;

/// Free small block; the link is stored in the block itself.
struct SmallFree {
    next: *mut SmallFree,
}

/// Free large extent; the header is stored at the start of the extent.
struct LargeFree {
    size: usize,
    next: *mut LargeFree,
}

/// Point-in-time allocator counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Arena size in bytes.
    pub capacity: usize,
    /// Bytes currently handed out, after rounding to the class or granule.
    pub used: usize,
    /// Largest value `used` has reached.
    pub peak: usize,
    /// Bytes held by small-class slabs, used or free.
    pub slab: usize,
    /// Bytes on the large free list.
    pub free_large: usize,
    /// Allocation requests refused because no extent fit.
    pub failures: usize,
}

/// Size-class and coalescing free-list allocator.
pub struct Heap {
    base: usize,
    end: usize,
    small: [*mut SmallFree; CLASSES],
    large: *mut LargeFree,
    stats: HeapStats,
}

// SAFETY: The heap only holds pointers into the arena it was given; moving the
// bookkeeping between threads does not move the arena. Callers synchronize.
unsafe impl Send for Heap {}

impl Heap {
    /// Returns a heap with no arena; every allocation fails until [`Heap::init`].
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            base: 0,
            end: 0,
            small: [ptr::null_mut(); CLASSES],
            large: ptr::null_mut(),
            stats: HeapStats {
                capacity: 0,
                used: 0,
                peak: 0,
                slab: 0,
                free_large: 0,
                failures: 0,
            },
        }
    }

    /// Takes ownership of `bytes` bytes at `base`.
    ///
    /// The start is rounded up and the length down to [`GRANULE`]. Returns
    /// `false` if the heap already has an arena or the region is too small.
    ///
    /// # Safety
    ///
    /// The region must be valid for reads and writes, unused by anything else,
    /// and stay so for as long as the heap or any block from it is live.
    pub unsafe fn init(&mut self, base: usize, bytes: usize) -> bool {
        if self.end != 0 {
            return false;
        }
        let Some(start) = align_up(base, GRANULE) else {
            return false;
        };
        let Some(limit) = base.checked_add(bytes) else {
            return false;
        };
        let end = limit & !(GRANULE - 1);
        if end <= start {
            return false;
        }
        self.base = start;
        self.end = end;
        let size = end - start;
        let node = start as *mut LargeFree;
        // SAFETY: The caller handed over the region; `start` is GRANULE-aligned
        // and at least one granule long, which holds a LargeFree.
        unsafe {
            node.write(LargeFree {
                size,
                next: ptr::null_mut(),
            })
        };
        self.large = node;
        self.stats.capacity = size;
        self.stats.free_large = size;
        true
    }

    /// Current counters.
    #[must_use]
    pub const fn stats(&self) -> HeapStats {
        self.stats
    }

    /// Allocates a block for `layout`, or returns null.
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let block = match small_class(layout) {
            Some(class) => self.allocate_small(class),
            None => match large_size(layout) {
                Some(size) => self
                    .allocate_large(size, layout.align().max(GRANULE))
                    .map_or(ptr::null_mut(), |(block, rounded)| {
                        self.charge(rounded);
                        block
                    }),
                None => ptr::null_mut(),
            },
        };
        if block.is_null() {
            self.stats.failures = self.stats.failures.saturating_add(1);
        }
        block
    }

    /// Returns a block to the heap.
    ///
    /// # Safety
    ///
    /// `block` must have come from [`Heap::allocate`] on this heap with the
    /// same `layout`, and must not be used or freed again afterwards.
    pub unsafe fn deallocate(&mut self, block: *mut u8, layout: Layout) {
        if block.is_null() {
            return;
        }
        if let Some(class) = small_class(layout) {
            let node = block.cast::<SmallFree>();
            // SAFETY: The block is a class-sized, class-aligned slab block
            // the caller no longer uses; every class holds a SmallFree.
            unsafe {
                node.write(SmallFree {
                    next: self.small[class],
                })
            };
            self.small[class] = node;
            self.stats.used -= class_size(class);
            return;
        }
        let Some(size) = large_size(layout) else {
            return;
        };
        // SAFETY: Forwarded from the caller's contract.
        unsafe { self.free_large(block as usize, size) };
        self.stats.used -= size;
    }

    fn charge(&mut self, bytes: usize) {
        self.stats.used += bytes;
        self.stats.peak = self.stats.peak.max(self.stats.used);
    }

    fn allocate_small(&mut self, class: usize) -> *mut u8 {
        if self.small[class].is_null() && !self.refill(class) {
            return ptr::null_mut();
        }
        let node = self.small[class];
        // SAFETY: Nodes on a class list are free blocks this heap wrote.
        self.small[class] = unsafe { (*node).next };
        self.charge(class_size(class));
        node.cast()
    }

    /// Carves a slab for `class` from the large tier.
    fn refill(&mut self, class: usize) -> bool {
        let size = class_size(class);
        let Some((slab, _)) = self.allocate_large(SLAB_BYTES, size.max(GRANULE)) else {
            return false;
        };
        self.stats.slab += SLAB_BYTES;
        let mut head = self.small[class];
        let mut offset = SLAB_BYTES;
        while offset >= size {
            offset -= size;
            // SAFETY: The slab is a fresh large extent of SLAB_BYTES; each
            // `size`-byte block inside it is aligned to `size`.
            let node = unsafe { slab.add(offset) }.cast::<SmallFree>();
            // SAFETY: As above; the block is unused and holds a SmallFree.
            unsafe { node.write(SmallFree { next: head }) };
            head = node;
        }
        self.small[class] = head;
        true
    }

    /// First-fit allocation of `size` bytes (a granule multiple) at `align`.
    /// Returns the block and the bytes removed from the free list.
    fn allocate_large(&mut self, size: usize, align: usize) -> Option<(*mut u8, usize)> {
        let mut previous: *mut LargeFree = ptr::null_mut();
        let mut current = self.large;
        while !current.is_null() {
            let start = current as usize;
            // SAFETY: Every node on the large list is a free extent this heap
            // wrote and still owns.
            let (extent, next) = unsafe { ((*current).size, (*current).next) };
            let end = start + extent;
            if let Some(aligned) = align_up(start, align)
                && let Some(block_end) = aligned.checked_add(size)
                && block_end <= end
            {
                let leading = aligned - start;
                let trailing = end - block_end;
                let mut after = next;
                if trailing != 0 {
                    let tail = block_end as *mut LargeFree;
                    // SAFETY: The tail is a granule-aligned, non-empty part of
                    // the free extent being split.
                    unsafe {
                        tail.write(LargeFree {
                            size: trailing,
                            next,
                        })
                    };
                    after = tail;
                }
                if leading != 0 {
                    // SAFETY: `current` stays on the list, shortened to the
                    // granule-aligned leading part.
                    unsafe {
                        (*current).size = leading;
                        (*current).next = after;
                    }
                } else {
                    self.link(previous, after);
                }
                self.stats.free_large -= size;
                return Some((aligned as *mut u8, size));
            }
            previous = current;
            current = next;
        }
        None
    }

    fn link(&mut self, previous: *mut LargeFree, next: *mut LargeFree) {
        if previous.is_null() {
            self.large = next;
        } else {
            // SAFETY: `previous` is a live node on the large list.
            unsafe { (*previous).next = next };
        }
    }

    /// Inserts `[address, address + size)` in address order and coalesces.
    ///
    /// # Safety
    ///
    /// The extent must be a block this heap handed out and no longer in use.
    unsafe fn free_large(&mut self, address: usize, size: usize) {
        debug_assert!(address >= self.base && address + size <= self.end);
        let mut previous: *mut LargeFree = ptr::null_mut();
        let mut current = self.large;
        while !current.is_null() && (current as usize) < address {
            previous = current;
            // SAFETY: Live node on the large list.
            current = unsafe { (*current).next };
        }
        self.stats.free_large += size;
        let node = address as *mut LargeFree;
        let mut node_size = size;
        if !current.is_null() && address + size == current as usize {
            // SAFETY: `current` is the free extent directly after the block.
            unsafe {
                node_size += (*current).size;
                current = (*current).next;
            }
        }
        if !previous.is_null() {
            // SAFETY: `previous` is a live node before the block.
            let previous_end = previous as usize + unsafe { (*previous).size };
            if previous_end == address {
                // SAFETY: As above; the block extends `previous`.
                unsafe {
                    (*previous).size += node_size;
                    (*previous).next = current;
                }
                return;
            }
        }
        // SAFETY: The block is granule-aligned, at least one granule long, and
        // unused, so it can hold the list node.
        unsafe {
            node.write(LargeFree {
                size: node_size,
                next: current,
            })
        };
        self.link(previous, node);
    }

    /// Size of the largest free large extent.
    #[must_use]
    pub fn largest_free(&self) -> usize {
        let mut largest = 0;
        let mut current = self.large;
        while !current.is_null() {
            // SAFETY: Live node on the large list.
            unsafe {
                largest = largest.max((*current).size);
                current = (*current).next;
            }
        }
        largest
    }
}

const fn class_size(class: usize) -> usize {
    MIN_SMALL << class
}

/// Size class for a small request, or `None` for the large tier.
fn small_class(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(MIN_SMALL);
    if need > MAX_SMALL {
        return None;
    }
    let rounded = need.next_power_of_two();
    Some((rounded / MIN_SMALL).trailing_zeros() as usize)
}

/// Rounded size for a large request.
fn large_size(layout: Layout) -> Option<usize> {
    align_up(layout.size().max(1), GRANULE)
}

const fn align_up(value: usize, align: usize) -> Option<usize> {
    match value.checked_add(align - 1) {
        Some(value) => Some(value & !(align - 1)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    const ARENA: usize = 4 * 1024 * 1024;

    struct Arena {
        memory: *mut u8,
        layout: Layout,
    }

    impl Arena {
        fn new(bytes: usize) -> Self {
            let layout = Layout::from_size_align(bytes, 4096).unwrap();
            // SAFETY: Non-zero layout.
            let memory = unsafe { std::alloc::alloc(layout) };
            assert!(!memory.is_null());
            Self { memory, layout }
        }

        fn heap(&self) -> Heap {
            let mut heap = Heap::empty();
            // SAFETY: The arena is owned by the test and outlives the heap.
            assert!(unsafe { heap.init(self.memory as usize, self.layout.size()) });
            heap
        }
    }

    impl Drop for Arena {
        fn drop(&mut self) {
            // SAFETY: Allocated in `new` with this layout.
            unsafe { std::alloc::dealloc(self.memory, self.layout) };
        }
    }

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn empty_heap_refuses() {
        let mut heap = Heap::empty();
        assert!(heap.allocate(layout(8, 8)).is_null());
        assert_eq!(heap.stats().failures, 1);
    }

    #[test]
    fn init_rejects_tiny_and_repeated_regions() {
        let arena = Arena::new(4096);
        let mut heap = Heap::empty();
        // SAFETY: The arena outlives the heap.
        assert!(!unsafe { heap.init(arena.memory as usize + 1, GRANULE) });
        // SAFETY: As above.
        assert!(unsafe { heap.init(arena.memory as usize, 4096) });
        // SAFETY: As above.
        assert!(!unsafe { heap.init(arena.memory as usize, 4096) });
    }

    #[test]
    fn small_blocks_are_aligned_reused_and_distinct() {
        let arena = Arena::new(ARENA);
        let mut heap = arena.heap();
        let first = heap.allocate(layout(24, 8));
        let second = heap.allocate(layout(24, 8));
        assert!(!first.is_null() && !second.is_null());
        assert_ne!(first, second);
        assert_eq!(first as usize % 32, 0);
        assert_eq!(heap.stats().used, 64);
        assert_eq!(heap.stats().slab, SLAB_BYTES);
        // SAFETY: Allocated above with the same layout.
        unsafe { heap.deallocate(first, layout(24, 8)) };
        assert_eq!(heap.allocate(layout(20, 4)), first);
        for align in [16, 64, 256, 2048] {
            let block = heap.allocate(layout(1, align));
            assert_eq!(block as usize % align, 0);
        }
    }

    #[test]
    fn large_blocks_honour_alignment() {
        let arena = Arena::new(ARENA);
        let mut heap = arena.heap();
        let _offset = heap.allocate(layout(4096 + 64, 64));
        for align in [64, 4096, 65536, 1 << 20] {
            let block = heap.allocate(layout(5000, align));
            assert!(!block.is_null());
            assert_eq!(block as usize % align, 0);
        }
    }

    #[test]
    fn freeing_everything_coalesces_back_to_one_extent() {
        let arena = Arena::new(ARENA);
        let mut heap = arena.heap();
        let capacity = heap.stats().capacity;
        let mut blocks = Vec::new();
        for index in 0..200 {
            let item = layout(3000 + index * 97, if index % 7 == 0 { 4096 } else { 64 });
            let block = heap.allocate(item);
            assert!(!block.is_null());
            blocks.push((block, item));
        }
        // Free in an interleaved order so both neighbours coalesce.
        for pass in 0..3 {
            for (index, (block, item)) in blocks.iter().enumerate() {
                if index % 3 == pass {
                    // SAFETY: Each block is freed exactly once with its layout.
                    unsafe { heap.deallocate(*block, *item) };
                }
            }
        }
        assert_eq!(heap.stats().used, 0);
        assert_eq!(heap.stats().free_large, capacity);
        assert_eq!(heap.largest_free(), capacity);
        assert!(heap.stats().peak > 0);
    }

    #[test]
    fn exhaustion_fails_closed_and_recovers_after_free() {
        let arena = Arena::new(256 * 1024);
        let mut heap = arena.heap();
        let big = layout(200 * 1024, 64);
        let first = heap.allocate(big);
        assert!(!first.is_null());
        assert!(heap.allocate(big).is_null());
        assert_eq!(heap.stats().failures, 1);
        // SAFETY: Allocated above with the same layout.
        unsafe { heap.deallocate(first, big) };
        assert_eq!(heap.allocate(big), first);
    }

    #[test]
    fn repeated_import_sized_churn_does_not_grow() {
        // The bump allocator leaked every decompression buffer. A reclaiming
        // heap must serve the same churn from a bounded footprint.
        let arena = Arena::new(ARENA);
        let mut heap = arena.heap();
        let scratch = layout(128 * 1024, 8);
        for _ in 0..1000 {
            let block = heap.allocate(scratch);
            assert!(!block.is_null());
            // SAFETY: Freed once with its layout.
            unsafe { heap.deallocate(block, scratch) };
        }
        assert_eq!(heap.stats().peak, 128 * 1024);
        assert_eq!(heap.stats().used, 0);
    }

    #[test]
    fn randomized_blocks_never_overlap() {
        let arena = Arena::new(ARENA);
        let mut heap = arena.heap();
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut live: Vec<(usize, Layout, u8)> = Vec::new();
        for step in 0..20_000u32 {
            if live.is_empty() || next() % 3 != 0 {
                let size = match next() % 4 {
                    0 => (next() % 64 + 1) as usize,
                    1 => (next() % 2048 + 1) as usize,
                    2 => (next() % 16384 + 1) as usize,
                    _ => (next() % 131_072 + 1) as usize,
                };
                let align = 1usize << (next() % 13);
                let item = layout(size, align);
                let block = heap.allocate(item);
                if block.is_null() {
                    continue;
                }
                assert_eq!(block as usize % align, 0);
                let tag = step as u8;
                // SAFETY: The block is `size` bytes inside the arena.
                unsafe { ptr::write_bytes(block, tag, size) };
                live.push((block as usize, item, tag));
            } else {
                let index = (next() as usize) % live.len();
                let (block, item, tag) = live.swap_remove(index);
                // SAFETY: Reads the block this test filled.
                let bytes = unsafe { core::slice::from_raw_parts(block as *const u8, item.size()) };
                assert!(
                    bytes.iter().all(|byte| *byte == tag),
                    "block was overwritten"
                );
                // SAFETY: Freed once with its layout.
                unsafe { heap.deallocate(block as *mut u8, item) };
            }
        }
        for (block, item, tag) in live.drain(..) {
            // SAFETY: Reads the block this test filled.
            let bytes = unsafe { core::slice::from_raw_parts(block as *const u8, item.size()) };
            assert!(
                bytes.iter().all(|byte| *byte == tag),
                "block was overwritten"
            );
            // SAFETY: Freed once with its layout.
            unsafe { heap.deallocate(block as *mut u8, item) };
        }
        let stats = heap.stats();
        assert_eq!(stats.used, 0);
        assert_eq!(stats.free_large + stats.slab, stats.capacity);
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Bounded anonymous shared-memory objects (`memfd_create`).
//!
//! A memfd is a kernel-owned object with a byte length and a bounded set of
//! physical pages. Descriptors are ordinary Linux file descriptors; `fork` and
//! `dup` take another reference, and the object is dissolved when the last
//! descriptor closes or its task exits. `mmap(MAP_SHARED)` aliases the object's
//! pages through the existing shared-frame registry, so threads, forked
//! processes, and repeated mappings observe one copy.
//!
//! Limits are explicit: 16 MiB per object, four objects per task, and a global
//! 64 MiB page budget. Memfd pages are not charged to the anonymous
//! shared-page quota (they have their own per-object budget), but they are
//! removed from the registry like any other shared frame when the last mapping
//! and the object reference are gone.
//!
//! All state is serialized by the single-CPU interrupt guard, matching the
//! scheduler's file table and the frame allocator.

use crate::arch::x86_64::timer::InterruptGuard;
use crate::memory::shared;
use aether_memory::PAGE_SIZE;
use aether_memory::frame::PhysFrame;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

pub use aether_memory::memfd::{MAX_MEMFDS, MAX_PAGES, MAX_TOTAL_PAGES, pages_for, valid_length};
/// File-key tag for memfd page-cache identities. Root and overlay identities
/// use the low 32 bits; the 1<<48 tag leaves those untouched.
const MEMFD_KEY_TAG: u64 = 1 << 48;

const ENOMEM: i64 = -12;
const EINVAL: i64 = -22;
const ENOSPC: i64 = -28;
const EBADF: i64 = -9;

#[derive(Clone, Copy)]
struct MemFdObject {
    used: bool,
    /// Logical length in bytes.
    len: usize,
    /// Allocated pages, `pages_len` entries valid.
    pages: [Option<PhysFrame>; MAX_PAGES],
    pages_len: usize,
    /// Live descriptors referring to this object.
    references: u32,
}

impl MemFdObject {
    const EMPTY: Self = Self {
        used: false,
        len: 0,
        pages: [None; MAX_PAGES],
        pages_len: 0,
        references: 0,
    };
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: Every access below takes an `InterruptGuard`; the kernel is single
// CPU. No reference to the table escapes a guarded call.
unsafe impl<T> Sync for Global<T> {}

static OBJECTS: Global<[MemFdObject; MAX_MEMFDS]> =
    Global(UnsafeCell::new([MemFdObject::EMPTY; MAX_MEMFDS]));
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(0);

fn state() -> &'static mut [MemFdObject; MAX_MEMFDS] {
    // SAFETY: Callers hold an `InterruptGuard`, which serializes this table.
    unsafe { &mut *OBJECTS.0.get() }
}

/// Returns the stable page-cache key for an object index.
#[must_use]
pub const fn file_key(index: usize) -> u64 {
    MEMFD_KEY_TAG | index as u64
}

/// Allocates a new empty object and returns its index.
pub fn create() -> Result<usize, i64> {
    let _guard = InterruptGuard::enter();
    let objects = state();
    let Some(index) = objects.iter().position(|object| !object.used) else {
        return Err(ENOSPC);
    };
    objects[index] = MemFdObject {
        used: true,
        references: 1,
        ..MemFdObject::EMPTY
    };
    Ok(index)
}

/// Adds one descriptor reference. Returns `false` for a stale index.
pub fn retain(index: usize) -> bool {
    let _guard = InterruptGuard::enter();
    let Some(object) = state().get_mut(index).filter(|object| object.used) else {
        return false;
    };
    object.references = object.references.saturating_add(1);
    true
}

/// Drops one descriptor reference, releasing every page with the last one.
pub fn release(index: usize) -> bool {
    let _guard = InterruptGuard::enter();
    let objects = state();
    let Some(object) = objects.get_mut(index).filter(|object| object.used) else {
        return false;
    };
    if object.references > 1 {
        object.references -= 1;
        return true;
    }
    for slot in object.pages.iter_mut().take(object.pages_len) {
        if let Some(frame) = slot.take() {
            release_page(frame);
        }
    }
    let released = object.pages_len;
    TOTAL_PAGES.fetch_sub(released, Ordering::Release);
    *object = MemFdObject::EMPTY;
    true
}

fn release_page(frame: PhysFrame) {
    let _ = crate::memory::with_frame_allocator(|allocator| {
        if !shared::release(allocator, frame) {
            let _ = allocator.deallocate(frame);
        }
    });
}

/// Returns the logical length of a live object.
#[must_use]
pub fn len(index: usize) -> Option<usize> {
    let _guard = InterruptGuard::enter();
    state()
        .get(index)
        .filter(|object| object.used)
        .map(|object| object.len)
}

/// Resizes an object. Growing allocates and zero-fills pages on demand; a
/// failure leaves the previous length and pages intact.
pub fn ftruncate(index: usize, length: usize) -> Result<(), i64> {
    if !valid_length(length as i64) {
        return Err(EINVAL);
    }
    let _guard = InterruptGuard::enter();
    let old_len = {
        let objects = state();
        let Some(object) = objects.get(index).filter(|object| object.used) else {
            return Err(EBADF);
        };
        object.len
    };
    let needed = pages_for(length).ok_or(EINVAL)?;
    let domain = crate::scheduler::linux_shared_domain();
    let key = file_key(index);
    let objects = state();
    let Some(object) = objects.get_mut(index).filter(|object| object.used) else {
        return Err(EBADF);
    };
    if needed > object.pages_len {
        let additional = needed - object.pages_len;
        if TOTAL_PAGES
            .load(Ordering::Acquire)
            .saturating_add(additional)
            > MAX_TOTAL_PAGES
        {
            return Err(ENOMEM);
        }
        let old_pages = object.pages_len;
        let mut installed = 0usize;
        let result = crate::memory::with_frame_allocator(|allocator| {
            for page in old_pages..needed {
                let Some(frame) = allocator.allocate() else {
                    return Err(ENOMEM);
                };
                // SAFETY: The frame was just allocated, is identity-mapped,
                // and is not reachable from any address space yet.
                unsafe {
                    core::ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize)
                };
                let offset = (page as u64) * PAGE_SIZE;
                if shared::register_memfd(frame, domain, (key, offset)).is_err() {
                    let _ = allocator.deallocate(frame);
                    return Err(ENOMEM);
                }
                object.pages[page] = Some(frame);
                object.pages_len = page + 1;
                installed += 1;
            }
            Ok(())
        });
        match result {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                rollback_pages(object, old_pages);
                TOTAL_PAGES.fetch_add(installed, Ordering::Release);
                return Err(error);
            }
            None => {
                rollback_pages(object, old_pages);
                TOTAL_PAGES.fetch_add(installed, Ordering::Release);
                return Err(ENOMEM);
            }
        }
        TOTAL_PAGES.fetch_add(installed, Ordering::Release);
    } else if needed < object.pages_len {
        for slot in object.pages.iter_mut().take(object.pages_len).skip(needed) {
            if let Some(frame) = slot.take() {
                release_page(frame);
            }
        }
        TOTAL_PAGES.fetch_sub(object.pages_len - needed, Ordering::Release);
        object.pages_len = needed;
    }
    // Extending inside the last existing page must zero the newly visible
    // tail, like a sparse file extension.
    if length > old_len
        && let Some(Some(frame)) = object.pages.get(old_len / PAGE_SIZE as usize)
    {
        let start = old_len % PAGE_SIZE as usize;
        let end = length.min((old_len / PAGE_SIZE as usize + 1) * PAGE_SIZE as usize);
        if end > start {
            // SAFETY: The frame is owned by this object and identity-mapped.
            unsafe {
                core::ptr::write_bytes(
                    (frame.start_address() as *mut u8).add(start),
                    0,
                    end - start,
                );
            }
        }
    }
    object.len = length;
    Ok(())
}

fn rollback_pages(object: &mut MemFdObject, old_pages: usize) {
    for slot in object
        .pages
        .iter_mut()
        .take(object.pages_len)
        .skip(old_pages)
    {
        if let Some(frame) = slot.take() {
            release_page(frame);
        }
    }
    object.pages_len = old_pages;
}

/// Reads up to `out.len()` bytes at `offset`, returning the byte count.
pub fn read(index: usize, offset: usize, out: &mut [u8]) -> Result<usize, i64> {
    let _guard = InterruptGuard::enter();
    let objects = state();
    let Some(object) = objects.get(index).filter(|object| object.used) else {
        return Err(EBADF);
    };
    if offset >= object.len {
        return Ok(0);
    }
    let length = (object.len - offset).min(out.len());
    let mut copied = 0usize;
    while copied < length {
        let position = offset + copied;
        let page = position / PAGE_SIZE as usize;
        let page_offset = position % PAGE_SIZE as usize;
        let chunk = (PAGE_SIZE as usize - page_offset).min(length - copied);
        let Some(frame) = object.pages.get(page).copied().flatten() else {
            out[copied..copied + chunk].fill(0);
            copied += chunk;
            continue;
        };
        // SAFETY: The frame belongs to this object and is identity-mapped.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (frame.start_address() as *const u8).add(page_offset),
                out[copied..copied + chunk].as_mut_ptr(),
                chunk,
            );
        }
        copied += chunk;
    }
    Ok(length)
}

/// Writes up to `data.len()` bytes at `offset`, bounded by the current length.
pub fn write(index: usize, offset: usize, data: &[u8]) -> Result<usize, i64> {
    let _guard = InterruptGuard::enter();
    let objects = state();
    let Some(object) = objects.get(index).filter(|object| object.used) else {
        return Err(EBADF);
    };
    if offset >= object.len {
        return Err(ENOSPC);
    }
    let length = (object.len - offset).min(data.len());
    let mut copied = 0usize;
    while copied < length {
        let position = offset + copied;
        let page = position / PAGE_SIZE as usize;
        let page_offset = position % PAGE_SIZE as usize;
        let chunk = (PAGE_SIZE as usize - page_offset).min(length - copied);
        let Some(frame) = object.pages.get(page).copied().flatten() else {
            return Err(ENOSPC);
        };
        // SAFETY: The frame belongs to this object and is identity-mapped.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data[copied..copied + chunk].as_ptr(),
                (frame.start_address() as *mut u8).add(page_offset),
                chunk,
            );
        }
        copied += chunk;
    }
    Ok(length)
}

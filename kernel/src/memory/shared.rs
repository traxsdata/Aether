// SPDX-License-Identifier: Apache-2.0
//! Reference-counted physical frames for Linux shared mappings.
//!
//! `MAP_SHARED` mappings are owned by more than one address space after
//! `fork`, and file-backed shared mappings may be owned by unrelated
//! processes. The page tables store only a physical frame, so ownership lives
//! in this bounded registry. A frame is registered once, retained by every
//! mapping that aliases it, and returned to the frame allocator only when the
//! last reference is dropped. That avoids the double-free and use-after-free
//! that plain eager `fork` would cause for an aliased frame.
//!
//! File-backed entries also carry a `(file key, page offset)` tuple so two
//! mappings of the same read-only rootfs page share one physical frame. The
//! cache entry is evicted with its last reference; this is not persistent
//! writeback.
//!
//! The kernel is single-CPU and syscall/exception paths run with interrupts
//! masked, matching the rest of the allocator and task tables. A later SMP
//! port must add a lock here.

use aether_memory::frame::{FrameAllocator, PhysFrame};
use core::cell::UnsafeCell;

/// Maximum number of simultaneously registered shared frames.
///
/// The anonymous/file pool is small, but a Gate 4 memfd may own up to 16 MiB
/// (4096 pages) and four such objects may coexist. Memfd pages keep their own
/// global budget (see `memfd.rs`) and do not consume the per-domain anonymous
/// page quota, so this table must cover both pools.
#[cfg(feature = "gate4-gui-test")]
pub const MAX_SHARED_FRAMES: usize = 512 + 131_072;
#[cfg(not(feature = "gate4-gui-test"))]
pub const MAX_SHARED_FRAMES: usize = 512 + 16_384;
/// Linux application domains are identified by the bounded overlay app id.
pub const MAX_DOMAINS: usize = 256;
/// Shared physical pages charged to one application domain.
pub const MAX_DOMAIN_SHARED_PAGES: u32 = 256;

/// One shared physical frame.
#[derive(Clone, Copy)]
struct SharedFrame {
    frame: PhysFrame,
    references: u32,
    domain: u8,
    /// `u64::MAX` for anonymous mappings, otherwise the scheduler file key.
    file_key: u64,
    file_offset: u64,
    /// Whether this frame was charged to the anonymous domain quota.
    charged: bool,
}

const ANONYMOUS_KEY: u64 = u64::MAX;

/// Failure while registering a shared frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedFrameError {
    /// The global registry has no free slot.
    Exhausted,
    /// The owning application domain exceeded its shared-page quota.
    Quota,
    /// The frame was already registered.
    Duplicate,
}

struct Registry {
    entries: [Option<SharedFrame>; MAX_SHARED_FRAMES],
    domain_pages: [u32; MAX_DOMAINS],
}

struct RegistryCell(UnsafeCell<Registry>);

// SAFETY: Access is serialized by the single-CPU kernel model: syscall,
// exception, and scheduler paths run with interrupts masked. No reference
// escapes a call below.
unsafe impl Sync for RegistryCell {}

static REGISTRY: RegistryCell = RegistryCell(UnsafeCell::new(Registry {
    entries: [None; MAX_SHARED_FRAMES],
    domain_pages: [0; MAX_DOMAINS],
}));

/// Registers a freshly allocated frame as shared.
///
/// The caller must own the only mapping of `frame`. On error the caller still
/// owns it and must release it through the normal allocator path.
pub fn register(
    frame: PhysFrame,
    domain: u8,
    file: Option<(u64, u64)>,
) -> Result<(), SharedFrameError> {
    register_inner(frame, domain, file, true)
}

/// Registers a memfd-owned frame.
///
/// Memfd pages have their own global budget and are bounded per object, so
/// they are not charged to the anonymous shared-page domain quota. This keeps
/// the existing 256-page anonymous quota test meaningful while allowing a
/// 16 MiB `wl_shm`-style pool.
pub fn register_memfd(
    frame: PhysFrame,
    domain: u8,
    file: (u64, u64),
) -> Result<(), SharedFrameError> {
    register_inner(frame, domain, Some(file), false)
}

fn register_inner(
    frame: PhysFrame,
    domain: u8,
    file: Option<(u64, u64)>,
    charge_domain: bool,
) -> Result<(), SharedFrameError> {
    // SAFETY: Single-CPU serialization as described on `RegistryCell`.
    let registry = unsafe { &mut *REGISTRY.0.get() };
    if registry
        .entries
        .iter()
        .flatten()
        .any(|entry| entry.frame == frame)
    {
        return Err(SharedFrameError::Duplicate);
    }
    let domain_index = usize::from(domain);
    let Some(pages) = registry.domain_pages.get_mut(domain_index) else {
        return Err(SharedFrameError::Quota);
    };
    if charge_domain && *pages >= MAX_DOMAIN_SHARED_PAGES {
        return Err(SharedFrameError::Quota);
    }
    let Some(slot) = registry.entries.iter_mut().find(|slot| slot.is_none()) else {
        return Err(SharedFrameError::Exhausted);
    };
    let (file_key, file_offset) = file.unwrap_or((ANONYMOUS_KEY, 0));
    *slot = Some(SharedFrame {
        frame,
        references: 1,
        domain,
        file_key,
        file_offset,
        charged: charge_domain,
    });
    if charge_domain {
        *pages += 1;
    }
    Ok(())
}

/// Finds a cached file page and adds a reference to it.
#[must_use]
pub fn find_file(file_key: u64, file_offset: u64) -> Option<PhysFrame> {
    // SAFETY: Single-CPU serialization as described on `RegistryCell`.
    let registry = unsafe { &mut *REGISTRY.0.get() };
    let entry = registry
        .entries
        .iter_mut()
        .flatten()
        .find(|entry| entry.file_key == file_key && entry.file_offset == file_offset)?;
    entry.references = entry.references.saturating_add(1);
    Some(entry.frame)
}

/// Adds a reference to a shared frame.
///
/// Returns `false` when the frame is not registered, which means it is a
/// private frame and must be copied instead.
pub fn retain(frame: PhysFrame) -> bool {
    // SAFETY: Single-CPU serialization as described on `RegistryCell`.
    let registry = unsafe { &mut *REGISTRY.0.get() };
    let Some(entry) = registry
        .entries
        .iter_mut()
        .flatten()
        .find(|entry| entry.frame == frame)
    else {
        return false;
    };
    entry.references = entry.references.saturating_add(1);
    true
}

/// Drops one reference to a shared frame.
///
/// Returns `true` when the frame was shared. The last reference returns the
/// frame to `allocator` and releases its domain quota. A `false` result means
/// the caller owns an ordinary private frame and must free it itself.
pub fn release(allocator: &mut FrameAllocator<'_>, frame: PhysFrame) -> bool {
    // SAFETY: Single-CPU serialization as described on `RegistryCell`.
    let registry = unsafe { &mut *REGISTRY.0.get() };
    let Some(index) = registry
        .entries
        .iter()
        .position(|entry| entry.as_ref().is_some_and(|entry| entry.frame == frame))
    else {
        return false;
    };
    let Some(entry) = registry.entries.get_mut(index).and_then(Option::as_mut) else {
        return false;
    };
    if entry.references > 1 {
        entry.references -= 1;
        return true;
    }
    let owned = entry.frame;
    let domain = usize::from(entry.domain);
    let charged = entry.charged;
    registry.entries[index] = None;
    if charged
        && let Some(pages) = registry.domain_pages.get_mut(domain)
        && *pages > 0
    {
        *pages -= 1;
    }
    let _ = allocator.deallocate(owned);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_page_sized_and_bounded() {
        assert!(MAX_SHARED_FRAMES >= MAX_DOMAIN_SHARED_PAGES as usize);
        assert!(MAX_DOMAINS >= 256);
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Kernel-owned x86-64 page tables.
//!
//! Phase 1 installs a new four-level identity map instead of trusting the
//! firmware page tables. The low 2 MiB is mapped with 4 KiB pages so the kernel
//! image can be given per-section permissions (text execute-only-for-write,
//! rodata read-only, data/bss non-executable). The rest of RAM is mapped with
//! 2 MiB pages for simplicity.
//!
//! This is not yet a user/kernel split and there is no higher-half direct map.
//! Address-space isolation arrives with the task model in Phase 2.

use aether_abi::user::{USER_BASE, USER_LIMIT, USER_STACK_PAGES, USER_STACK_TOP};
use aether_abi::{BootInfo, FramebufferInfo, MemoryKind, MemoryRegion};
use aether_boot_common::elf::{Image, PF_W, PF_X};
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_memory::paging::{PAGE_TABLE_ENTRIES, PageFlags, PageTable, PageTableEntry};
use aether_memory::{LARGE_PAGE_SIZE, PAGE_SIZE};
use core::arch::asm;
use core::ptr;

use super::shared;

/// Low region mapped with 4 KiB pages.
const LOW_REGION_BYTES: u64 = LARGE_PAGE_SIZE;

/// Result of installing kernel page tables.
#[derive(Clone, Copy, Debug)]
pub struct PagingStats {
    /// Bytes of physical address space covered by the new identity map.
    pub mapped_bytes: u64,
    /// Page-table frames allocated from the frame allocator.
    pub page_table_frames: u64,
    /// Physical address installed in CR3.
    pub pml4_address: u64,
    /// Whether the linear framebuffer was mapped for the graphics service.
    pub framebuffer_mapped: bool,
}

/// Failure while building or activating the identity map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagingError {
    /// The frame allocator ran out of memory for a page table.
    OutOfMemory,
    /// A physical address could not be represented in a page-table entry.
    UnrepresentableAddress(u64),
    /// The boot memory map contains no mappable RAM.
    EmptyMap,
    /// The memory-map pointer or size cannot be represented.
    InvalidMemoryMap,
    /// Boot metadata lies above the identity-map limit.
    MetadataOutsideMap,
    /// A page-table walk found a huge-page entry where a table was expected.
    UnexpectedHugePage,
    /// The active kernel PML4 entry 0 is missing.
    MissingKernelMapping,
    /// A user image segment lies outside the user address window.
    UserImageOutsideWindow,
    /// A user image segment is empty or has inconsistent bounds.
    InvalidUserSegment,
    /// A user page table contains an unexpected huge-page entry.
    UnexpectedUserHugePage,
    /// The framebuffer overlaps an existing huge-page mapping.
    FramebufferHugePage,
}

unsafe extern "C" {
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
}

fn kernel_ranges() -> (u64, u64, u64, u64, u64, u64) {
    // SAFETY: These are linker-defined absolute addresses, not memory reads.
    (
        core::ptr::addr_of!(__text_start) as u64,
        core::ptr::addr_of!(__text_end) as u64,
        core::ptr::addr_of!(__rodata_start) as u64,
        core::ptr::addr_of!(__rodata_end) as u64,
        core::ptr::addr_of!(__data_start) as u64,
        core::ptr::addr_of!(__kernel_end) as u64,
    )
}

/// Virtual base of the per-address-space private window.
///
/// This is PML4 slot 1 (512 GiB), which the kernel identity map does not use.
/// Every task maps the same virtual address to a different physical frame,
/// which makes address-space isolation observable from the scheduler test.
pub const PRIVATE_BASE: u64 = 0x0000_0080_0000_0000;

/// Reads the active PML4 physical address from CR3.
#[must_use]
pub fn current_pml4() -> Option<PhysFrame> {
    let value: u64;
    // SAFETY: Reading CR3 is valid at CPL 0 and has no side effects.
    unsafe {
        asm!(
            "mov {value}, cr3",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    PhysFrame::from_address(value & aether_memory::paging::ADDRESS_MASK)
}

/// Switches to `pml4`.
pub fn activate(pml4: PhysFrame) {
    // SAFETY: The address space shares the kernel identity mapping, so the
    // running code, stack, and data remain mapped. Interrupts are disabled by
    // the caller while switching scheduler state.
    unsafe {
        asm!(
            "mov cr3, {pml4}",
            pml4 = in(reg) pml4.start_address(),
            options(nostack, preserves_flags)
        );
    }
}

/// Creates a new address space that shares the kernel identity map and maps
/// `private_frame` at [`PRIVATE_BASE`].
///
/// The private mapping is kernel-only for now. User-accessible mappings and
/// per-task kernel stacks arrive with user mode.
pub fn create_address_space(
    allocator: &mut FrameAllocator<'_>,
    private_frame: PhysFrame,
) -> Result<PhysFrame, PagingError> {
    let kernel_pml4 = current_pml4().ok_or(PagingError::MissingKernelMapping)?;
    let kernel_entry = unsafe { (*table_ptr(kernel_pml4)).entry(0) };
    if !kernel_entry.is_present() {
        return Err(PagingError::MissingKernelMapping);
    }

    let pml4 = allocate_table(allocator)?;
    let pdpt = allocate_table(allocator)?;
    let pd = allocate_table(allocator)?;
    let pt = allocate_table(allocator)?;

    let [pml4_index, pdpt_index, pd_index, pt_index] = aether_memory::paging::indices(PRIVATE_BASE);

    // SAFETY: All tables were just allocated and zeroed, so every entry is
    // absent. The kernel PML4 entry is copied so kernel text, data, stacks, and
    // the identity map remain reachable in the new address space.
    unsafe {
        let pml4_table = table_ptr(pml4);
        (*pml4_table).set_entry(0, kernel_entry);
        (*pml4_table).set_entry(pml4_index, link_entry(pdpt)?);
        (*table_ptr(pdpt)).set_entry(pdpt_index, link_entry(pd)?);
        (*table_ptr(pd)).set_entry(pd_index, link_entry(pt)?);
        let private = PageTableEntry::new(
            private_frame.start_address(),
            PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE,
        )
        .ok_or(PagingError::UnrepresentableAddress(
            private_frame.start_address(),
        ))?;
        (*table_ptr(pt)).set_entry(pt_index, private);
    }

    Ok(pml4)
}

fn link_entry(child: PhysFrame) -> Result<PageTableEntry, PagingError> {
    PageTableEntry::new(
        child.start_address(),
        PageFlags::PRESENT | PageFlags::WRITABLE,
    )
    .ok_or(PagingError::UnrepresentableAddress(child.start_address()))
}

fn link_user_entry(child: PhysFrame) -> Result<PageTableEntry, PagingError> {
    PageTableEntry::new(
        child.start_address(),
        PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER,
    )
    .ok_or(PagingError::UnrepresentableAddress(child.start_address()))
}

/// Creates an address space for a ring-3 process.
///
/// The new PML4 shares the supervisor-only kernel identity map but adds user
/// mappings only in [`USER_BASE`]..[`USER_LIMIT`]. Each `PT_LOAD` page gets its
/// own frame; the final permissions follow the ELF segment flags. A private
/// stack is mapped below [`USER_STACK_TOP`].
///
/// The kernel copies the file bytes through the physical frame before the page
/// is made read-only, so W^X does not prevent loading.
pub fn create_user_address_space(
    allocator: &mut FrameAllocator<'_>,
    image: &Image,
    bytes: &[u8],
) -> Result<PhysFrame, PagingError> {
    let pml4 = create_empty_user_address_space(allocator)?;

    for segment in image.segments() {
        let physical_end = segment
            .physical_address
            .checked_add(segment.memory_size)
            .ok_or(PagingError::InvalidUserSegment)?;
        if segment.memory_size == 0
            || physical_end <= segment.physical_address
            || segment.physical_address < USER_BASE
            || physical_end > USER_LIMIT
        {
            return Err(PagingError::UserImageOutsideWindow);
        }

        let first_page = segment.physical_address & !(PAGE_SIZE - 1);
        let last_page = align_up_page(physical_end)?;
        let mut virtual_address = first_page;
        while virtual_address < last_page {
            let frame = allocate_table(allocator)?;
            // SAFETY: The frame is exclusively owned and identity-mapped.
            unsafe { ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize) };

            // Copy the intersection of this page and the segment's file data.
            let page_end = virtual_address.saturating_add(PAGE_SIZE);
            let copy_start = segment.physical_address.max(virtual_address);
            let copy_end = physical_end
                .min(page_end)
                .min(segment.physical_address.saturating_add(segment.file_size));
            if copy_start < copy_end {
                let source_offset = segment
                    .file_offset
                    .checked_add(copy_start - segment.physical_address)
                    .ok_or(PagingError::InvalidUserSegment)?
                    as usize;
                let length = (copy_end - copy_start) as usize;
                let source = bytes
                    .get(source_offset..source_offset.saturating_add(length))
                    .ok_or(PagingError::InvalidUserSegment)?;
                // SAFETY: The destination is inside the freshly zeroed frame,
                // which is identity-mapped and exclusively owned.
                unsafe {
                    ptr::copy_nonoverlapping(
                        source.as_ptr(),
                        (frame.start_address() + (copy_start - virtual_address)) as *mut u8,
                        length,
                    );
                }
            }

            map_user_page(
                pml4,
                allocator,
                virtual_address,
                frame,
                user_segment_flags(segment.flags),
            )?;
            virtual_address = page_end;
        }
    }

    map_user_stack(allocator, pml4)?;

    Ok(pml4)
}

/// Creates an empty ring-3 address space sharing the kernel identity map.
pub fn create_empty_user_address_space(
    allocator: &mut FrameAllocator<'_>,
) -> Result<PhysFrame, PagingError> {
    let kernel_pml4 = current_pml4().ok_or(PagingError::MissingKernelMapping)?;
    let kernel_entry = unsafe { (*table_ptr(kernel_pml4)).entry(0) };
    if !kernel_entry.is_present() {
        return Err(PagingError::MissingKernelMapping);
    }

    let pml4 = allocate_table(allocator)?;
    // SAFETY: The table was just allocated and zeroed. The kernel entry is
    // copied unchanged so kernel text, data, stacks, and the identity map stay
    // supervisor-only in the new address space.
    unsafe { (*table_ptr(pml4)).set_entry(0, kernel_entry) };
    Ok(pml4)
}

/// Maps the fixed user stack window into an address space.
pub fn map_user_stack(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
) -> Result<(), PagingError> {
    for page in 0..USER_STACK_PAGES {
        let address = USER_STACK_TOP - (page + 1) * PAGE_SIZE;
        let frame = allocate_table(allocator)?;
        // SAFETY: The frame is exclusively owned and identity-mapped.
        unsafe { ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize) };
        map_user_page(
            pml4,
            allocator,
            address,
            frame,
            PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::USER | PageFlags::NO_EXECUTE,
        )?;
    }
    Ok(())
}

fn user_segment_flags(flags: u32) -> PageFlags {
    let mut page = PageFlags::PRESENT | PageFlags::USER;
    if flags & PF_W != 0 {
        page = page | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
    }
    if flags & PF_X == 0 {
        page = page | PageFlags::NO_EXECUTE;
    }
    page
}

fn align_up_page(value: u64) -> Result<u64, PagingError> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(PagingError::InvalidUserSegment)
}

/// Returns the leaf flags for a user page.
///
/// Ordinary pages are user-accessible. A [`PageFlags::GUARD`] (`PROT_NONE`)
/// page stays present but supervisor-only, read-only and non-executable, so
/// user code faults on it while the frame stays owned by the mapping.
fn user_leaf_flags(flags: PageFlags) -> PageFlags {
    if flags.contains(PageFlags::GUARD) {
        flags.without(PageFlags::USER | PageFlags::WRITABLE)
            | PageFlags::PRESENT
            | PageFlags::NO_EXECUTE
    } else {
        flags | PageFlags::USER | PageFlags::PRESENT
    }
}

/// Returns `true` for a leaf that belongs to the user mapping: an ordinary
/// user page or a `PROT_NONE` guard page. Walkers that own frames (fork,
/// unmap, teardown) use this; access checks require [`PageFlags::USER`].
fn is_user_leaf(entry: PageTableEntry) -> bool {
    entry.is_present()
        && (entry.flags().contains(PageFlags::USER) || entry.flags().contains(PageFlags::GUARD))
}

/// Maps one 4 KiB page into a user address space, allocating intermediate
/// tables and marking every level user-accessible.
pub fn map_user_page(
    pml4: PhysFrame,
    allocator: &mut FrameAllocator<'_>,
    virtual_address: u64,
    physical: PhysFrame,
    flags: PageFlags,
) -> Result<(), PagingError> {
    if !(USER_BASE..USER_LIMIT).contains(&virtual_address)
        || !virtual_address.is_multiple_of(PAGE_SIZE)
    {
        return Err(PagingError::UserImageOutsideWindow);
    }
    let [pml4_index, pdpt_index, pd_index, pt_index] =
        aether_memory::paging::indices(virtual_address);

    let pdpt = ensure_user_table(pml4, pml4_index, allocator)?;
    let pd = ensure_user_table(pdpt, pdpt_index, allocator)?;
    let pt = ensure_user_table(pd, pd_index, allocator)?;
    let entry = PageTableEntry::new(physical.start_address(), user_leaf_flags(flags)).ok_or(
        PagingError::UnrepresentableAddress(physical.start_address()),
    )?;
    // SAFETY: `pt` was validated or allocated above and is identity-mapped.
    unsafe { (*table_ptr(pt)).set_entry(pt_index, entry) };
    flush_page(virtual_address);
    Ok(())
}

/// Allocates, zeroes, and maps one user page with the supplied permissions.
pub fn map_allocated_user_page(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    virtual_address: u64,
    flags: PageFlags,
) -> Result<(), PagingError> {
    let frame = allocate_table(allocator)?;
    if let Err(error) = map_user_page(pml4, allocator, virtual_address, frame, flags) {
        let _ = allocator.deallocate(frame);
        return Err(error);
    }
    Ok(())
}

/// Copies every present user page into a fresh address space.
///
/// Private frames are eagerly copied. Frames registered as shared are retained
/// and mapped into the child, preserving `MAP_SHARED` aliasing across `fork`.
/// The caller owns the returned PML4 and is responsible for releasing its
/// frames when the process exits.
pub fn copy_user_address_space(
    allocator: &mut FrameAllocator<'_>,
    source: PhysFrame,
) -> Result<PhysFrame, PagingError> {
    let target = create_empty_user_address_space(allocator)?;
    let mut address = USER_BASE;
    while address < USER_LIMIT {
        if let Some(entry) = user_leaf_entry(source, address)
            && is_user_leaf(entry)
        {
            let Some(shared_frame) = PhysFrame::from_address(entry.address()) else {
                return Err(PagingError::UnrepresentableAddress(entry.address()));
            };
            if shared::retain(shared_frame) {
                if let Err(error) =
                    map_user_page(target, allocator, address, shared_frame, entry.flags())
                {
                    let _ = shared::release(allocator, shared_frame);
                    return Err(error);
                }
            } else {
                let frame = allocator.allocate().ok_or(PagingError::OutOfMemory)?;
                // SAFETY: Both frames are identity-mapped and exclusively owned
                // by their respective address spaces.
                unsafe {
                    ptr::copy_nonoverlapping(
                        entry.address() as *const u8,
                        frame.start_address() as *mut u8,
                        PAGE_SIZE as usize,
                    );
                }
                map_user_page(target, allocator, address, frame, entry.flags())?;
            }
        }
        address += PAGE_SIZE;
    }
    Ok(target)
}

/// Allocates and maps a user range eagerly.
///
/// Every page gets its own zeroed frame. On failure the pages mapped so far are
/// unmapped and their frames are returned to the allocator.
pub fn map_allocated_user_range(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    start: u64,
    length: u64,
    flags: PageFlags,
) -> Result<(), PagingError> {
    let end = validate_user_range(start, length)?;
    let mut address = start;
    while address < end {
        let frame = allocator.allocate().ok_or(PagingError::OutOfMemory)?;
        // SAFETY: The frame is exclusively owned and identity-mapped.
        unsafe { ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize) };
        if let Err(error) = map_user_page(pml4, allocator, address, frame, flags) {
            let _ = allocator.deallocate(frame);
            let _ = unmap_user_range(allocator, pml4, start, address - start);
            return Err(error);
        }
        address += PAGE_SIZE;
    }
    Ok(())
}

/// Allocates and maps an anonymous `MAP_SHARED` range.
///
/// Every page is zero-filled, registered in the shared-frame registry, and
/// mapped with `flags`. The first reference is owned by the new mapping.
/// Partial failure unmaps the pages already installed, which either releases
/// shared references or returns unregistered frames to the allocator.
pub fn map_shared_anonymous_range(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    start: u64,
    length: u64,
    flags: PageFlags,
    domain: u8,
) -> Result<(), PagingError> {
    let end = validate_user_range(start, length)?;
    let mut address = start;
    while address < end {
        let Some(frame) = allocator.allocate() else {
            let _ = unmap_user_range(allocator, pml4, start, address - start);
            return Err(PagingError::OutOfMemory);
        };
        // SAFETY: The frame was just allocated, is identity-mapped, and is not
        // reachable from any address space until `map_user_page` runs.
        unsafe { ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize) };
        if let Err(error) = map_user_page(pml4, allocator, address, frame, flags) {
            let _ = allocator.deallocate(frame);
            let _ = unmap_user_range(allocator, pml4, start, address - start);
            return Err(error);
        }
        if shared::register(frame, domain, None).is_err() {
            // The current page is mapped but unregistered; unmapping the range
            // also releases the earlier registered pages.
            let _ = unmap_user_range(
                allocator,
                pml4,
                start,
                address.saturating_add(PAGE_SIZE) - start,
            );
            return Err(PagingError::OutOfMemory);
        }
        address += PAGE_SIZE;
    }
    Ok(())
}

/// Unmaps a user range and returns its frames to the allocator.
///
/// Registered shared frames lose one reference and are freed only with their
/// last owner. Absent pages are skipped so a caller can release a partially
/// populated range. The number of released frames is returned.
pub fn unmap_user_range(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    start: u64,
    length: u64,
) -> Result<usize, PagingError> {
    let end = validate_user_range(start, length)?;
    let mut released = 0usize;
    let mut address = start;
    while address < end {
        if let Some(entry) = user_leaf_entry(pml4, address)
            && is_user_leaf(entry)
        {
            if let Some(frame) = PhysFrame::from_address(entry.address())
                && !shared::release(allocator, frame)
            {
                // Not a registered shared frame: this address space owns it.
                let _ = allocator.deallocate(frame);
            }
            if clear_user_leaf(pml4, address) {
                flush_page(address);
                released += 1;
            }
        }
        address += PAGE_SIZE;
    }
    Ok(released)
}

/// Releases every user frame and page-table frame of an address space,
/// including the PML4 itself.
///
/// Shared frames lose one reference; private frames return to the allocator.
/// Only the PML4 slots that cover [`USER_BASE`]..[`USER_LIMIT`] are walked, so
/// the shared kernel entries are never touched.
///
/// # Safety
///
/// `pml4` must be a user address space created by this module that is not
/// active in CR3 and is not referenced by any runnable task.
pub unsafe fn destroy_user_address_space(allocator: &mut FrameAllocator<'_>, pml4: PhysFrame) {
    let first = aether_memory::paging::indices(USER_BASE)[0];
    let last = aether_memory::paging::indices(USER_LIMIT - 1)[0];
    for pml4_index in first..=last {
        // SAFETY: The caller guarantees the tables are owned by this address
        // space and identity-mapped; nothing else walks them any more.
        unsafe {
            let pml4_entry = (*table_ptr(pml4)).entry(pml4_index);
            if !pml4_entry.is_present() || pml4_entry.is_huge() {
                continue;
            }
            let Some(pdpt) = PhysFrame::from_address(pml4_entry.address()) else {
                continue;
            };
            for pdpt_index in 0..512 {
                let pdpt_entry = (*table_ptr(pdpt)).entry(pdpt_index);
                if !pdpt_entry.is_present() || pdpt_entry.is_huge() {
                    continue;
                }
                let Some(pd) = PhysFrame::from_address(pdpt_entry.address()) else {
                    continue;
                };
                for pd_index in 0..512 {
                    let pd_entry = (*table_ptr(pd)).entry(pd_index);
                    if !pd_entry.is_present() || pd_entry.is_huge() {
                        continue;
                    }
                    let Some(pt) = PhysFrame::from_address(pd_entry.address()) else {
                        continue;
                    };
                    for pt_index in 0..512 {
                        let leaf = (*table_ptr(pt)).entry(pt_index);
                        if !is_user_leaf(leaf) {
                            continue;
                        }
                        if let Some(frame) = PhysFrame::from_address(leaf.address())
                            && !shared::release(allocator, frame)
                        {
                            let _ = allocator.deallocate(frame);
                        }
                    }
                    let _ = allocator.deallocate(pt);
                }
                let _ = allocator.deallocate(pd);
            }
            let _ = allocator.deallocate(pdpt);
            (*table_ptr(pml4)).set_entry(pml4_index, PageTableEntry::EMPTY);
        }
    }
    let _ = allocator.deallocate(pml4);
}

fn validate_user_range(start: u64, length: u64) -> Result<u64, PagingError> {
    if length == 0
        || !start.is_multiple_of(PAGE_SIZE)
        || !length.is_multiple_of(PAGE_SIZE)
        || start < USER_BASE
    {
        return Err(PagingError::UserImageOutsideWindow);
    }
    let end = start
        .checked_add(length)
        .ok_or(PagingError::UserImageOutsideWindow)?;
    if end > USER_LIMIT {
        return Err(PagingError::UserImageOutsideWindow);
    }
    Ok(end)
}

fn clear_user_leaf(pml4: PhysFrame, address: u64) -> bool {
    let [pml4_index, pdpt_index, pd_index, pt_index] = aether_memory::paging::indices(address);
    // SAFETY: The PML4 and child tables are task-owned and identity-mapped.
    unsafe {
        let pml4_entry = (*table_ptr(pml4)).entry(pml4_index);
        if !pml4_entry.is_present() || pml4_entry.is_huge() {
            return false;
        }
        let Some(pdpt) = PhysFrame::from_address(pml4_entry.address()) else {
            return false;
        };
        let pdpt_entry = (*table_ptr(pdpt)).entry(pdpt_index);
        if !pdpt_entry.is_present() || pdpt_entry.is_huge() {
            return false;
        }
        let Some(pd) = PhysFrame::from_address(pdpt_entry.address()) else {
            return false;
        };
        let pd_entry = (*table_ptr(pd)).entry(pd_index);
        if !pd_entry.is_present() || pd_entry.is_huge() {
            return false;
        }
        let Some(pt) = PhysFrame::from_address(pd_entry.address()) else {
            return false;
        };
        (*table_ptr(pt)).set_entry(pt_index, PageTableEntry::EMPTY);
    }
    true
}

fn flush_page(address: u64) {
    // SAFETY: INVLPG is valid at CPL0 for a mapped or unmapped user page. It
    // only invalidates a TLB entry and has no memory side effects.
    unsafe {
        asm!(
            "invlpg [{address}]",
            address = in(reg) address,
            options(nostack, preserves_flags)
        );
    }
}

fn ensure_user_table(
    parent: PhysFrame,
    index: usize,
    allocator: &mut FrameAllocator<'_>,
) -> Result<PhysFrame, PagingError> {
    let entry = unsafe { (*table_ptr(parent)).entry(index) };
    if entry.is_present() {
        if entry.is_huge() {
            return Err(PagingError::UnexpectedUserHugePage);
        }
        return PhysFrame::from_address(entry.address())
            .ok_or(PagingError::UnrepresentableAddress(entry.address()));
    }

    let child = allocate_table(allocator)?;
    let link = link_user_entry(child)?;
    // SAFETY: The parent table is identity-mapped and exclusively owned by
    // this address space.
    unsafe { (*table_ptr(parent)).set_entry(index, link) };
    Ok(child)
}

/// Base of the Linux virtual-memory window.
///
/// The window is reserved address space, not pre-mapped memory. `mmap` and
/// `brk` allocate frames on demand; `munmap` returns them to the allocator.
pub const LINUX_ARENA_BASE: u64 = USER_BASE + 256 * 1024 * 1024;
/// Number of pages in the Linux arena (32 MiB).
pub const LINUX_ARENA_PAGES: u64 = 262_144;
/// Pages reserved for the Linux `brk` heap at the start of the arena (1 MiB).
pub const LINUX_HEAP_PAGES: u64 = 4096;
/// First address available to Linux `mmap` after the heap window.
pub const LINUX_MMAP_BASE: u64 = LINUX_ARENA_BASE + LINUX_HEAP_PAGES * PAGE_SIZE;

/// Changes permissions on an existing user range.
pub fn protect_user_range(
    pml4: PhysFrame,
    start: u64,
    length: u64,
    flags: PageFlags,
) -> Result<(), PagingError> {
    if length == 0
        || !start.is_multiple_of(PAGE_SIZE)
        || !length.is_multiple_of(PAGE_SIZE)
        || start < USER_BASE
        || start.saturating_add(length) > USER_LIMIT
    {
        return Err(PagingError::UserImageOutsideWindow);
    }
    let mut address = start;
    while address < start + length {
        let [pml4_index, pdpt_index, pd_index, pt_index] = aether_memory::paging::indices(address);
        let pml4e = unsafe { (*table_ptr(pml4)).entry(pml4_index) };
        if !pml4e.is_present() || pml4e.is_huge() {
            return Err(PagingError::MissingKernelMapping);
        }
        let pdpt = PhysFrame::from_address(pml4e.address())
            .ok_or(PagingError::UnrepresentableAddress(pml4e.address()))?;
        let pdpte = unsafe { (*table_ptr(pdpt)).entry(pdpt_index) };
        if !pdpte.is_present() || pdpte.is_huge() {
            return Err(PagingError::UnexpectedUserHugePage);
        }
        let pd = PhysFrame::from_address(pdpte.address())
            .ok_or(PagingError::UnrepresentableAddress(pdpte.address()))?;
        let pde = unsafe { (*table_ptr(pd)).entry(pd_index) };
        if !pde.is_present() || pde.is_huge() {
            return Err(PagingError::UnexpectedUserHugePage);
        }
        let pt = PhysFrame::from_address(pde.address())
            .ok_or(PagingError::UnrepresentableAddress(pde.address()))?;
        let entry = unsafe { (*table_ptr(pt)).entry(pt_index) };
        if !entry.is_present() {
            return Err(PagingError::MissingKernelMapping);
        }
        let address_phys = entry.address();
        let updated = PageTableEntry::new(address_phys, user_leaf_flags(flags))
            .ok_or(PagingError::UnrepresentableAddress(address_phys))?;
        unsafe { (*table_ptr(pt)).set_entry(pt_index, updated) };
        flush_page(address);
        address += PAGE_SIZE;
    }
    Ok(())
}

/// Changes the protections of the present pages in a user range.
///
/// Anonymous VMAs are demand-paged, so `mprotect` may legally run before any
/// page has been faulted in. Missing leaf entries are skipped; the VMA table
/// carries the new protection for the fault path to apply later. This mirrors
/// `protect_user_range`, which remains strict for callers that require every
/// page to be mapped.
pub fn protect_user_range_if_present(
    pml4: PhysFrame,
    start: u64,
    length: u64,
    flags: PageFlags,
) -> Result<(), PagingError> {
    if length == 0
        || !start.is_multiple_of(PAGE_SIZE)
        || !length.is_multiple_of(PAGE_SIZE)
        || start < USER_BASE
        || start.saturating_add(length) > USER_LIMIT
    {
        return Err(PagingError::UserImageOutsideWindow);
    }
    let mut address = start;
    while address < start + length {
        let [pml4_index, pdpt_index, pd_index, pt_index] = aether_memory::paging::indices(address);
        let pml4e = unsafe { (*table_ptr(pml4)).entry(pml4_index) };
        if !pml4e.is_present() {
            // A demand-paged range may not have allocated its intermediate
            // tables yet. That is not an error: the VMA already records the
            // protection and the fault path installs the page later.
            address += PAGE_SIZE;
            continue;
        }
        if pml4e.is_huge() {
            return Err(PagingError::MissingKernelMapping);
        }
        let pdpt = PhysFrame::from_address(pml4e.address())
            .ok_or(PagingError::UnrepresentableAddress(pml4e.address()))?;
        let pdpte = unsafe { (*table_ptr(pdpt)).entry(pdpt_index) };
        if !pdpte.is_present() {
            address += PAGE_SIZE;
            continue;
        }
        if pdpte.is_huge() {
            return Err(PagingError::UnexpectedUserHugePage);
        }
        let pd = PhysFrame::from_address(pdpte.address())
            .ok_or(PagingError::UnrepresentableAddress(pdpte.address()))?;
        let pde = unsafe { (*table_ptr(pd)).entry(pd_index) };
        if !pde.is_present() {
            address += PAGE_SIZE;
            continue;
        }
        if pde.is_huge() {
            return Err(PagingError::UnexpectedUserHugePage);
        }
        let pt = PhysFrame::from_address(pde.address())
            .ok_or(PagingError::UnrepresentableAddress(pde.address()))?;
        let entry = unsafe { (*table_ptr(pt)).entry(pt_index) };
        if entry.is_present() {
            let address_phys = entry.address();
            let updated = PageTableEntry::new(address_phys, user_leaf_flags(flags))
                .ok_or(PagingError::UnrepresentableAddress(address_phys))?;
            unsafe { (*table_ptr(pt)).set_entry(pt_index, updated) };
            flush_page(address);
        }
        address += PAGE_SIZE;
    }
    Ok(())
}

/// Validates that `address..address+len` lies in the user window and is mapped
/// with user permissions.
///
/// System calls use this before dereferencing a user pointer. A write requires
/// every page to be writable; a read only requires presence. The range may be
/// empty, which is valid and copies nothing.
#[must_use]
pub fn user_range_valid(pml4: PhysFrame, address: u64, len: u64, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = address.checked_add(len) else {
        return false;
    };
    if address < USER_BASE || end > USER_LIMIT {
        return false;
    }

    let mut page = address & !(PAGE_SIZE - 1);
    while page < end {
        let Some(entry) = user_leaf_entry(pml4, page) else {
            return false;
        };
        if !entry.is_present() || !entry.flags().contains(PageFlags::USER) {
            return false;
        }
        if write && !entry.flags().contains(PageFlags::WRITABLE) {
            return false;
        }
        page = page.saturating_add(PAGE_SIZE);
    }
    true
}

/// Writes a short kernel-owned byte slice into a task's validated user mapping
/// without changing CR3. This is used when completing a blocked syscall for a
/// process other than the currently scheduled one.
pub fn write_user_bytes(pml4: PhysFrame, address: u64, bytes: &[u8]) -> bool {
    if !user_range_valid(pml4, address, bytes.len() as u64, true) {
        return false;
    }
    let mut copied = 0usize;
    while copied < bytes.len() {
        let virtual_address = address + copied as u64;
        let Some(entry) = user_leaf_entry(pml4, virtual_address) else {
            return false;
        };
        let page_offset = (virtual_address & (PAGE_SIZE - 1)) as usize;
        let length = (PAGE_SIZE as usize - page_offset).min(bytes.len() - copied);
        let physical = entry.address() + page_offset as u64;
        // SAFETY: The page-table walk validated this task's user writable
        // mapping, and the identity map covers allocated physical frames.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes[copied..copied + length].as_ptr(),
                physical as *mut u8,
                length,
            );
        }
        copied += length;
    }
    true
}

/// Copies a user byte range into a kernel buffer while validating each page in
/// the same pass.
///
/// This is the bulk form for large, already-bounded transfers such as a
/// presented surface. Unlike [`read_user_bytes`], it does not pre-walk the
/// whole range and then walk it again to copy. A failure may have copied a
/// prefix into `bytes`; callers must discard the buffer on `false`.
#[must_use]
pub fn read_user_bytes_checked(pml4: PhysFrame, address: u64, bytes: &mut [u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    let Some(end) = address.checked_add(bytes.len() as u64) else {
        return false;
    };
    if address < USER_BASE || end > USER_LIMIT {
        return false;
    }
    let mut copied = 0usize;
    while copied < bytes.len() {
        let virtual_address = address + copied as u64;
        let Some(entry) = user_leaf_entry(pml4, virtual_address) else {
            return false;
        };
        if !entry.is_present() || !entry.flags().contains(PageFlags::USER) {
            return false;
        }
        let page_offset = (virtual_address & (PAGE_SIZE - 1)) as usize;
        let length = (PAGE_SIZE as usize - page_offset).min(bytes.len() - copied);
        let physical = entry.address() + page_offset as u64;
        // SAFETY: The leaf entry was checked present and user-readable above,
        // and the kernel identity map covers allocated physical frames.
        unsafe {
            core::ptr::copy_nonoverlapping(
                physical as *const u8,
                bytes[copied..copied + length].as_mut_ptr(),
                length,
            );
        }
        copied += length;
    }
    true
}

/// Copies a short user byte range into a kernel buffer without changing CR3.
/// The range must be present and readable in the supplied address space.
pub fn read_user_bytes(pml4: PhysFrame, address: u64, bytes: &mut [u8]) -> bool {
    if !user_range_valid(pml4, address, bytes.len() as u64, false) {
        return false;
    }
    let mut copied = 0usize;
    while copied < bytes.len() {
        let virtual_address = address + copied as u64;
        let Some(entry) = user_leaf_entry(pml4, virtual_address) else {
            return false;
        };
        let page_offset = (virtual_address & (PAGE_SIZE - 1)) as usize;
        let length = (PAGE_SIZE as usize - page_offset).min(bytes.len() - copied);
        let physical = entry.address() + page_offset as u64;
        // SAFETY: The page-table walk validated this task's user readable
        // mapping, and the identity map covers allocated physical frames.
        unsafe {
            core::ptr::copy_nonoverlapping(
                physical as *const u8,
                bytes[copied..copied + length].as_mut_ptr(),
                length,
            );
        }
        copied += length;
    }
    true
}

/// Returns `true` if the PML4 entry covering `address` is present.
#[cfg(feature = "usercopy-fault-selftest")]
#[must_use]
pub fn pml4_slot_present(pml4: PhysFrame, address: u64) -> bool {
    let [pml4_index, ..] = aether_memory::paging::indices(address);
    // SAFETY: The PML4 is the active table and the kernel identity map covers
    // every allocated page-table frame.
    unsafe { (*table_ptr(pml4)).entry(pml4_index).is_present() }
}

/// Returns `true` if `address` has a leaf of the user mapping, including a
/// `PROT_NONE` guard page.
#[must_use]
pub fn user_leaf_present(pml4: PhysFrame, address: u64) -> bool {
    (USER_BASE..USER_LIMIT).contains(&address)
        && user_leaf_entry(pml4, address).is_some_and(is_user_leaf)
}

/// Returns the physical address behind a present user mapping.
#[must_use]
pub fn user_physical_address(pml4: PhysFrame, address: u64) -> Option<u64> {
    if !(USER_BASE..USER_LIMIT).contains(&address) {
        return None;
    }
    let entry = user_leaf_entry(pml4, address)?;
    if !entry.is_present() || !entry.flags().contains(PageFlags::USER) {
        return None;
    }
    Some(entry.address() + (address & (PAGE_SIZE - 1)))
}

fn user_leaf_entry(pml4: PhysFrame, address: u64) -> Option<PageTableEntry> {
    let [pml4_index, pdpt_index, pd_index, pt_index] = aether_memory::paging::indices(address);
    // SAFETY: The PML4 is the active or a task-owned table and the kernel
    // identity map covers every allocated page-table frame.
    unsafe {
        let pml4_entry = (*table_ptr(pml4)).entry(pml4_index);
        if !pml4_entry.is_present() || pml4_entry.is_huge() {
            return None;
        }
        let pdpt = PhysFrame::from_address(pml4_entry.address())?;
        let pdpt_entry = (*table_ptr(pdpt)).entry(pdpt_index);
        if !pdpt_entry.is_present() || pdpt_entry.is_huge() {
            return None;
        }
        let pd = PhysFrame::from_address(pdpt_entry.address())?;
        let pd_entry = (*table_ptr(pd)).entry(pd_index);
        if !pd_entry.is_present() || pd_entry.is_huge() {
            return None;
        }
        let pt = PhysFrame::from_address(pd_entry.address())?;
        Some((*table_ptr(pt)).entry(pt_index))
    }
}

/// Installs kernel page tables and switches CR3.
pub fn init(
    allocator: &mut FrameAllocator<'_>,
    info: &BootInfo,
    regions: &[MemoryRegion],
) -> Result<PagingStats, PagingError> {
    let end = identity_end(info, regions)?;
    let mut tables = Tables::new(allocator)?;

    tables.map_low_memory()?;
    tables.map_kernel()?;
    tables.map_large_pages(end, regions)?;
    let framebuffer_mapped = tables.map_framebuffer(&info.framebuffer)?;

    let pml4_address = tables.pml4.start_address();
    let stats = PagingStats {
        mapped_bytes: end,
        page_table_frames: tables.allocated,
        pml4_address,
        framebuffer_mapped,
    };
    tables.activate();
    Ok(stats)
}

/// Returns `true` if a memory region may be identity-mapped by the kernel.
///
/// The kernel only accesses conventional RAM, loader-owned data, ACPI tables,
/// and runtime services. Device MMIO is mapped explicitly when a driver needs
/// it; reserved firmware holes and unusable memory are never mapped.
fn is_identity_mappable(region: &MemoryRegion) -> bool {
    matches!(
        region.kind(),
        Some(
            MemoryKind::Usable
                | MemoryKind::BootServices
                | MemoryKind::Loader
                | MemoryKind::AcpiReclaimable
                | MemoryKind::AcpiNvs
                | MemoryKind::RuntimeServices
        )
    )
}

/// Computes the exclusive end of the identity map.
///
/// Only RAM, loader, ACPI, and runtime-service regions are considered. MMIO,
/// reserved firmware holes, and unusable memory are not mapped; the kernel does
/// not access them yet. This keeps the identity map proportional to actual RAM
/// rather than to the PCI address hole. The map is capped at the frame bitmap
/// capacity (16 GiB); the loader keeps boot metadata in the low 4 GiB so it is
/// always inside the map.
fn identity_end(info: &BootInfo, regions: &[MemoryRegion]) -> Result<u64, PagingError> {
    let mut end = 0u64;
    for region in regions {
        if !is_identity_mappable(region) {
            continue;
        }
        let Some(region_end) = region.end() else {
            continue;
        };
        if region.start >= crate::memory::MAX_PHYSICAL_BYTES {
            continue;
        }
        // A region may extend past the allocator's bitmap. Truncate it instead
        // of refusing to boot on machines with more than 16 GiB of RAM.
        end = end.max(region_end.min(crate::memory::MAX_PHYSICAL_BYTES));
    }

    end = end.max(info.kernel_end);
    let map_bytes = info
        .memory_region_count
        .checked_mul(core::mem::size_of::<MemoryRegion>() as u64)
        .ok_or(PagingError::InvalidMemoryMap)?;
    let map_end = info
        .memory_regions
        .checked_add(map_bytes)
        .ok_or(PagingError::InvalidMemoryMap)?;
    let info_end = (info as *const BootInfo as u64)
        .checked_add(core::mem::size_of::<BootInfo>() as u64)
        .ok_or(PagingError::InvalidMemoryMap)?;
    end = end.max(map_end).max(info_end);

    // The hand-off and kernel must remain mapped after the CR3 switch.
    if end > crate::memory::MAX_PHYSICAL_BYTES {
        return Err(PagingError::MetadataOutsideMap);
    }
    if end == 0 {
        return Err(PagingError::EmptyMap);
    }
    Ok(align_up_large(end))
}

fn align_up_large(value: u64) -> u64 {
    let mask = LARGE_PAGE_SIZE - 1;
    value.saturating_add(mask) & !mask
}

struct Tables<'a, 'b> {
    allocator: &'a mut FrameAllocator<'b>,
    pml4: PhysFrame,
    allocated: u64,
}

impl<'a, 'b> Tables<'a, 'b> {
    fn new(allocator: &'a mut FrameAllocator<'b>) -> Result<Self, PagingError> {
        let pml4 = allocate_table(allocator)?;
        Ok(Self {
            allocator,
            pml4,
            allocated: 1,
        })
    }

    fn map_low_memory(&mut self) -> Result<(), PagingError> {
        let pdpt = self.ensure(self.pml4, 0)?;
        let pd = self.ensure(pdpt, 0)?;
        let pt = self.ensure(pd, 0)?;
        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
        for index in 0..PAGE_TABLE_ENTRIES {
            let address = index as u64 * PAGE_SIZE;
            let entry = PageTableEntry::new(address, flags)
                .ok_or(PagingError::UnrepresentableAddress(address))?;
            unsafe { (*table_ptr(pt)).set_entry(index, entry) };
        }
        Ok(())
    }

    /// Maps the 2 MiB windows containing the kernel with 4 KiB pages so each
    /// section gets its own permissions.
    fn map_kernel(&mut self) -> Result<(), PagingError> {
        let (text_start, text_end, rodata_start, rodata_end, _data_start, kernel_end) =
            kernel_ranges();

        let mut window = text_start & !(LARGE_PAGE_SIZE - 1);
        let end = align_up_large(kernel_end);
        while window < end {
            let pml4_index = ((window >> 39) & 0x1ff) as usize;
            let pdpt_index = ((window >> 30) & 0x1ff) as usize;
            let pd_index = ((window >> 21) & 0x1ff) as usize;

            let pdpt = self.ensure(self.pml4, pml4_index)?;
            let pd = self.ensure(pdpt, pdpt_index)?;
            let pt = self.ensure(pd, pd_index)?;

            for index in 0..PAGE_TABLE_ENTRIES {
                let address = window + index as u64 * PAGE_SIZE;
                let flags = if address >= text_start && address < text_end {
                    // Kernel text: read + execute, no write.
                    PageFlags::PRESENT
                } else if address >= rodata_start && address < rodata_end {
                    PageFlags::PRESENT | PageFlags::NO_EXECUTE
                } else {
                    // Data, BSS, and gaps in the kernel window.
                    PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE
                };
                // `_data_start` is reserved for future per-section handling;
                // every non-text, non-rodata page in the window is writable.
                let entry = PageTableEntry::new(address, flags)
                    .ok_or(PagingError::UnrepresentableAddress(address))?;
                unsafe { (*table_ptr(pt)).set_entry(index, entry) };
            }
            window = window.saturating_add(LARGE_PAGE_SIZE);
        }
        Ok(())
    }

    fn map_large_pages(&mut self, end: u64, regions: &[MemoryRegion]) -> Result<(), PagingError> {
        let (text_start, _text_end, _rodata_start, _rodata_end, _data_start, kernel_end) =
            kernel_ranges();
        let kernel_window_start = text_start & !(LARGE_PAGE_SIZE - 1);
        let kernel_window_end = align_up_large(kernel_end);
        let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;

        let mut base = LOW_REGION_BYTES;
        while base < end {
            let next = base.saturating_add(LARGE_PAGE_SIZE);
            // The low region and the kernel windows are already mapped with
            // 4 KiB page tables.
            if base >= kernel_window_start && base < kernel_window_end {
                base = next;
                continue;
            }

            // Build a bitmap of the 4 KiB pages covered by mappable regions in
            // this 2 MiB window. A fully covered window can use a huge page; a
            // partial window uses 4 KiB pages so reserved holes stay unmapped.
            let mut covered = [0u64; PAGE_TABLE_ENTRIES / 64];
            let mut any = false;
            for region in regions {
                if !is_identity_mappable(region) {
                    continue;
                }
                let Some(region_end) = region.end() else {
                    continue;
                };
                let start = region.start.max(base);
                let region_end = region_end.min(next);
                if start >= region_end {
                    continue;
                }
                any = true;
                let first = ((start - base) / PAGE_SIZE) as usize;
                let last = ((region_end - 1 - base) / PAGE_SIZE) as usize;
                for page in first..=last {
                    covered[page / 64] |= 1u64 << (page % 64);
                }
            }
            if !any {
                base = next;
                continue;
            }

            let pml4_index = ((base >> 39) & 0x1ff) as usize;
            let pdpt_index = ((base >> 30) & 0x1ff) as usize;
            let pd_index = ((base >> 21) & 0x1ff) as usize;
            let pdpt = self.ensure(self.pml4, pml4_index)?;
            let pd = self.ensure(pdpt, pdpt_index)?;

            if covered.iter().all(|bits| *bits == u64::MAX) {
                let entry = PageTableEntry::new_large(base, flags)
                    .ok_or(PagingError::UnrepresentableAddress(base))?;
                unsafe { (*table_ptr(pd)).set_entry(pd_index, entry) };
            } else {
                let pt = self.ensure(pd, pd_index)?;
                for page in 0..PAGE_TABLE_ENTRIES {
                    if covered[page / 64] & (1u64 << (page % 64)) == 0 {
                        continue;
                    }
                    let address = base + page as u64 * PAGE_SIZE;
                    let entry = PageTableEntry::new(address, flags)
                        .ok_or(PagingError::UnrepresentableAddress(address))?;
                    unsafe { (*table_ptr(pt)).set_entry(page, entry) };
                }
            }
            base = next;
        }
        Ok(())
    }

    /// Maps the linear framebuffer identity with write-combining disabled.
    ///
    /// Firmware memory maps classify the framebuffer as MMIO, which the
    /// general identity map deliberately skips. The graphics service needs it,
    /// so it is mapped explicitly, non-executable and cache-disabled.
    fn map_framebuffer(&mut self, framebuffer: &FramebufferInfo) -> Result<bool, PagingError> {
        if !framebuffer.is_present() {
            return Ok(false);
        }
        let end = framebuffer
            .base
            .checked_add(framebuffer.size)
            .ok_or(PagingError::InvalidMemoryMap)?;
        if end > crate::memory::MAX_PHYSICAL_BYTES {
            return Ok(false);
        }
        let start = framebuffer.base & !(PAGE_SIZE - 1);
        let end = align_up_page(end)?;
        let flags = PageFlags::PRESENT
            | PageFlags::WRITABLE
            | PageFlags::CACHE_DISABLE
            | PageFlags::NO_EXECUTE;

        let mut address = start;
        while address < end {
            let [pml4_index, pdpt_index, pd_index, pt_index] =
                aether_memory::paging::indices(address);
            let pdpt = self.ensure(self.pml4, pml4_index)?;
            let pd = self.ensure(pdpt, pdpt_index)?;
            let pd_entry = unsafe { (*table_ptr(pd)).entry(pd_index) };
            if pd_entry.is_present() && pd_entry.is_huge() {
                return Err(PagingError::FramebufferHugePage);
            }
            let pt = self.ensure(pd, pd_index)?;
            let entry = PageTableEntry::new(address, flags)
                .ok_or(PagingError::UnrepresentableAddress(address))?;
            // SAFETY: The leaf table is identity-mapped and owned by the
            // kernel.
            unsafe { (*table_ptr(pt)).set_entry(pt_index, entry) };
            address = address.saturating_add(PAGE_SIZE);
        }
        Ok(true)
    }

    /// Returns the child table at `index`, allocating and linking it if absent.
    fn ensure(&mut self, parent: PhysFrame, index: usize) -> Result<PhysFrame, PagingError> {
        let entry = unsafe { (*table_ptr(parent)).entry(index) };
        if entry.is_present() {
            if entry.is_huge() {
                return Err(PagingError::UnexpectedHugePage);
            }
            return PhysFrame::from_address(entry.address())
                .ok_or(PagingError::UnrepresentableAddress(entry.address()));
        }

        let child = allocate_table(self.allocator)?;
        self.allocated += 1;
        let link = PageTableEntry::new(
            child.start_address(),
            PageFlags::PRESENT | PageFlags::WRITABLE,
        )
        .ok_or(PagingError::UnrepresentableAddress(child.start_address()))?;
        unsafe { (*table_ptr(parent)).set_entry(index, link) };
        Ok(child)
    }

    fn activate(&self) {
        // SAFETY: The new PML4 identity-maps the running code, stack, and all
        // data used by this function. Interrupts are disabled. Writing CR3
        // also flushes non-global TLB entries.
        unsafe {
            asm!(
                "mov cr3, {pml4}",
                pml4 = in(reg) self.pml4.start_address(),
                options(nostack, preserves_flags)
            );
        }
    }
}

fn allocate_table<'a>(allocator: &mut FrameAllocator<'a>) -> Result<PhysFrame, PagingError> {
    let frame = allocator.allocate().ok_or(PagingError::OutOfMemory)?;
    // SAFETY: The allocator returned an exclusive, page-aligned frame that is
    // identity-mapped both before and after the CR3 switch.
    unsafe {
        ptr::write_bytes(frame.start_address() as *mut u8, 0, PAGE_SIZE as usize);
    }
    Ok(frame)
}

fn table_ptr(frame: PhysFrame) -> *mut PageTable {
    frame.start_address() as *mut PageTable
}

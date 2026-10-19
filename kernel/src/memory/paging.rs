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

use aether_abi::{BootInfo, MemoryKind, MemoryRegion};
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_memory::paging::{PAGE_TABLE_ENTRIES, PageFlags, PageTable, PageTableEntry};
use aether_memory::{LARGE_PAGE_SIZE, PAGE_SIZE};
use core::arch::asm;
use core::ptr;

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
    /// The active kernel PML4 entry 0 is missing.
    MissingKernelMapping,
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

/// Installs kernel page tables and switches CR3.
pub fn init(
    allocator: &mut FrameAllocator<'_>,
    info: &BootInfo,
    regions: &[MemoryRegion],
) -> Result<PagingStats, PagingError> {
    let end = identity_end(info, regions).ok_or(PagingError::EmptyMap)?;
    let mut tables = Tables::new(allocator)?;

    tables.map_low_memory()?;
    tables.map_kernel()?;
    tables.map_large_pages(end)?;

    let pml4_address = tables.pml4.start_address();
    let stats = PagingStats {
        mapped_bytes: end,
        page_table_frames: tables.allocated,
        pml4_address,
    };
    tables.activate();
    Ok(stats)
}

/// Computes the exclusive end of the identity map.
///
/// Only RAM, loader, ACPI, and runtime-service regions are considered. MMIO,
/// reserved firmware holes, and unusable memory are not mapped; the kernel does
/// not access them yet. This keeps the identity map proportional to actual RAM
/// rather than to the PCI address hole. The result is capped by the frame
/// bitmap capacity.
fn identity_end(info: &BootInfo, regions: &[MemoryRegion]) -> Option<u64> {
    let mut end = 0u64;
    for region in regions {
        let kinds = matches!(
            region.kind(),
            Some(
                MemoryKind::Usable
                    | MemoryKind::BootServices
                    | MemoryKind::Loader
                    | MemoryKind::AcpiReclaimable
                    | MemoryKind::AcpiNvs
                    | MemoryKind::RuntimeServices
            )
        );
        if !kinds {
            continue;
        }
        let Some(region_end) = region.end() else {
            continue;
        };
        if region.start >= crate::memory::MAX_PHYSICAL_BYTES {
            continue;
        }
        end = end.max(region_end);
    }

    end = end.max(info.kernel_end);
    let map_bytes = info
        .memory_region_count
        .checked_mul(core::mem::size_of::<MemoryRegion>() as u64)?;
    let map_end = info.memory_regions.checked_add(map_bytes)?;
    let info_end =
        (info as *const BootInfo as u64).checked_add(core::mem::size_of::<BootInfo>() as u64)?;
    end = end.max(map_end).max(info_end);

    // The hand-off and kernel must remain mapped after the CR3 switch.
    if end > crate::memory::MAX_PHYSICAL_BYTES {
        return None;
    }
    if end == 0 {
        return None;
    }
    Some(align_up_large(end))
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
                // `data_start` is checked for documentation and future use;
                // every non-text, non-rodata page in the window is writable.
                let entry = PageTableEntry::new(address, flags)
                    .ok_or(PagingError::UnrepresentableAddress(address))?;
                unsafe { (*table_ptr(pt)).set_entry(index, entry) };
            }
            window = window.saturating_add(LARGE_PAGE_SIZE);
        }
        Ok(())
    }

    fn map_large_pages(&mut self, end: u64) -> Result<(), PagingError> {
        let (text_start, _text_end, _rodata_start, _rodata_end, _data_start, kernel_end) =
            kernel_ranges();
        let kernel_window_start = text_start & !(LARGE_PAGE_SIZE - 1);
        let kernel_window_end = align_up_large(kernel_end);

        let mut base = LOW_REGION_BYTES;
        while base < end {
            // The low region and the kernel windows are already mapped with
            // 4 KiB page tables.
            if base >= kernel_window_start && base < kernel_window_end {
                base = base.saturating_add(LARGE_PAGE_SIZE);
                continue;
            }

            let pml4_index = ((base >> 39) & 0x1ff) as usize;
            let pdpt_index = ((base >> 30) & 0x1ff) as usize;
            let pd_index = ((base >> 21) & 0x1ff) as usize;

            let pdpt = self.ensure(self.pml4, pml4_index)?;
            let pd = self.ensure(pdpt, pdpt_index)?;

            let flags = PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::NO_EXECUTE;
            let entry = PageTableEntry::new_large(base, flags)
                .ok_or(PagingError::UnrepresentableAddress(base))?;
            unsafe { (*table_ptr(pd)).set_entry(pd_index, entry) };
            base = base.saturating_add(LARGE_PAGE_SIZE);
        }
        Ok(())
    }

    /// Returns the child table at `index`, allocating and linking it if absent.
    fn ensure(&mut self, parent: PhysFrame, index: usize) -> Result<PhysFrame, PagingError> {
        let entry = unsafe { (*table_ptr(parent)).entry(index) };
        if entry.is_present() {
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

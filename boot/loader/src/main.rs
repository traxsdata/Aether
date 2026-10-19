// SPDX-License-Identifier: Apache-2.0
//! Aether UEFI loader.
//!
//! The loader validates the embedded kernel image, loads its `PT_LOAD`
//! segments, builds a bounded physical memory map, exits UEFI boot services,
//! and jumps to the kernel. All policy that can run on the host lives in
//! `aether-boot-common`.

#![no_std]
#![no_main]

mod serial;
mod uefi;

use aether_abi::{BootInfo, MAX_MEMORY_REGIONS, MemoryKind, MemoryRegion};
use aether_boot_common::elf;
use aether_boot_common::{PAGE_SIZE, pages_for};
use core::ffi::c_void;
use core::fmt;
use core::panic::PanicInfo;
use core::ptr;
use uefi::{
    ALLOCATE_ADDRESS, ALLOCATE_ANY_PAGES, BUFFER_TOO_SMALL, BootServices, Handle,
    MEMORY_TYPE_LOADER_DATA, MemoryDescriptor, SUCCESS, Status, SystemTable,
};

/// Kernel image embedded by the build. The path is supplied by `build.rs`, so
/// the image on disk and the image in the loader cannot drift apart.
static KERNEL_ELF: &[u8] = include_bytes!(env!("AETHER_KERNEL_ELF"));

/// Fixed buffer for the firmware memory map. OVMF's map is a few kilobytes;
/// 64 KiB leaves ample room for unusual firmware.
const MEMORY_MAP_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
enum LoadError {
    NoBootServices,
    Image(elf::ParseError),
    BootInfo(aether_abi::BootInfoError),
    Allocation { status: Status },
    AddressMismatch { wanted: u64, got: u64 },
    BadDescriptorSize(usize),
    MemoryMap { status: Status },
    MemoryMapTooLarge,
    TooManyRegions(usize),
    ExitBootServices(Status),
    EntryPoint(u64),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBootServices => write!(f, "firmware did not provide boot services"),
            Self::Image(error) => write!(f, "kernel image rejected: {error:?}"),
            Self::BootInfo(error) => write!(f, "boot information rejected: {error}"),
            Self::Allocation { status } => write!(f, "page allocation failed (status {status:#x})"),
            Self::AddressMismatch { wanted, got } => {
                write!(f, "loaded at {got:#x}, kernel requires {wanted:#x}")
            }
            Self::BadDescriptorSize(size) => write!(f, "unsupported memory descriptor size {size}"),
            Self::MemoryMap { status } => write!(f, "GetMemoryMap failed (status {status:#x})"),
            Self::MemoryMapTooLarge => write!(f, "firmware memory map exceeds 64 KiB"),
            Self::TooManyRegions(count) => {
                write!(
                    f,
                    "memory map has {count} regions after merging, limit is {MAX_MEMORY_REGIONS}"
                )
            }
            Self::ExitBootServices(status) => {
                write!(f, "ExitBootServices failed (status {status:#x})")
            }
            Self::EntryPoint(address) => write!(f, "invalid kernel entry point {address:#x}"),
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    serial::write_str("AETHER_LOADER_PANIC\n");
    serial_line!("loader panic: {info}");
    halt()
}

/// UEFI application entry point.
///
/// # Safety
///
/// Firmware calls this function with valid handles and a system table. The
/// loader validates the embedded kernel before writing to memory.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(image: Handle, system_table: *mut SystemTable) -> Status {
    serial::init();
    serial_line!(
        "AETHER_LOADER_OK stage=entry version={}",
        env!("CARGO_PKG_VERSION")
    );

    match unsafe { boot(image, system_table) } {
        Ok(()) => {
            // `boot` only returns after a successful hand-off when the kernel
            // entry point unexpectedly returns.
            serial_line!("AETHER_LOADER_FAILED kernel returned to loader");
            halt()
        }
        Err(error) => {
            serial_line!("AETHER_LOADER_FAILED error={error}");
            halt()
        }
    }
}

unsafe fn boot(handle: Handle, system_table: *mut SystemTable) -> Result<(), LoadError> {
    // SAFETY: Firmware supplied this table.
    let services =
        unsafe { (*system_table).boot_services.as_ref() }.ok_or(LoadError::NoBootServices)?;

    let kernel = elf::parse(KERNEL_ELF).map_err(LoadError::Image)?;
    serial_line!(
        "loader: kernel elf start={:#x} end={:#x} entry={:#x} segments={}",
        kernel.start(),
        kernel.end(),
        kernel.entry(),
        kernel.segments().len()
    );

    let length = kernel
        .end()
        .checked_sub(kernel.start())
        .ok_or(LoadError::EntryPoint(kernel.entry()))?;
    let pages = pages_for(length).ok_or(LoadError::EntryPoint(kernel.entry()))?;
    unsafe { allocate_at(services, kernel.start(), pages)? };

    // SAFETY: The destination range was just allocated for the kernel and the
    // parser bounded every source range by the embedded image.
    unsafe {
        for segment in kernel.segments() {
            let source = KERNEL_ELF.as_ptr().add(segment.file_offset as usize);
            let destination = segment.physical_address as *mut u8;
            ptr::copy_nonoverlapping(source, destination, segment.file_size as usize);
            if segment.memory_size > segment.file_size {
                ptr::write_bytes(
                    destination.add(segment.file_size as usize),
                    0,
                    (segment.memory_size - segment.file_size) as usize,
                );
            }
        }
    }

    let boot_info_page = unsafe { allocate_any(services, 1)? };
    let map_page = unsafe { allocate_any(services, pages_for(MEMORY_MAP_BYTES as u64).unwrap())? };
    let region_bytes = (MAX_MEMORY_REGIONS * core::mem::size_of::<MemoryRegion>()) as u64;
    let region_page = unsafe { allocate_any(services, pages_for(region_bytes).unwrap())? };

    let boot_info = boot_info_page as *mut BootInfo;
    let regions = region_page as *mut MemoryRegion;
    let count = unsafe { read_memory_map(services, map_page, regions)? };

    // SAFETY: Both pages were allocated above and are large enough.
    unsafe {
        ptr::write(
            boot_info,
            BootInfo {
                magic: aether_abi::BOOT_INFO_MAGIC,
                version: aether_abi::BOOT_INFO_VERSION,
                struct_size: core::mem::size_of::<BootInfo>() as u32,
                memory_regions: regions as u64,
                memory_region_count: count as u64,
                kernel_start: kernel.start(),
                kernel_end: kernel.end(),
                entry_point: kernel.entry(),
                system_table: system_table as u64,
                reserved: [0; 4],
            },
        );
    }

    let info = unsafe { &*boot_info };
    info.validate().map_err(LoadError::BootInfo)?;
    // SAFETY: `read_memory_map` filled `count` regions inside the allocation.
    let usable = unsafe { info.usable_bytes() };
    serial_line!(
        "loader: memory regions={} usable={} MiB",
        info.memory_region_count,
        usable / (1024 * 1024)
    );

    let mut map_key = unsafe { current_map_key(services, map_page)? };
    for attempt in 0..8 {
        // SAFETY: Firmware contract; boot services are still active.
        let status = unsafe { (services.exit_boot_services)(handle, map_key) };
        if status == SUCCESS {
            serial_line!(
                "loader: boot services exited after {} attempt(s)",
                attempt + 1
            );
            serial_line!("AETHER_LOADER_HANDOFF entry={:#x}", kernel.entry());
            // SAFETY: The kernel image is loaded at its linked physical
            // address, the boot information page remains allocated, and the
            // kernel ABI is `extern "sysv64"` with one pointer argument.
            unsafe {
                let entry: unsafe extern "sysv64" fn(*const BootInfo) -> ! =
                    core::mem::transmute(kernel.entry() as usize);
                entry(boot_info);
            }
        }

        // The map key changes if another agent modifies the memory map. Re-read
        // the map and retry a bounded number of times.
        serial_line!("loader: ExitBootServices retry {attempt}: status={status:#x}");
        map_key = unsafe { current_map_key(services, map_page)? };
        let new_count = unsafe { read_memory_map(services, map_page, regions)? };
        // SAFETY: The boot information page is still owned by the loader.
        unsafe {
            (*boot_info).memory_region_count = new_count as u64;
        }
    }

    Err(LoadError::ExitBootServices(1))
}

unsafe fn current_map_key(services: &BootServices, map_page: u64) -> Result<usize, LoadError> {
    let mut size = MEMORY_MAP_BYTES;
    let mut key = 0usize;
    let mut descriptor_size = 0usize;
    let mut descriptor_version = 0u32;
    // SAFETY: The buffer is an allocated page range of `MEMORY_MAP_BYTES`.
    let status = unsafe {
        (services.get_memory_map)(
            &mut size,
            map_page as *mut c_void,
            &mut key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };
    check_map_status(status, size)?;
    Ok(key)
}

unsafe fn read_memory_map(
    services: &BootServices,
    map_page: u64,
    regions: *mut MemoryRegion,
) -> Result<usize, LoadError> {
    let mut size = MEMORY_MAP_BYTES;
    let mut key = 0usize;
    let mut descriptor_size = 0usize;
    let mut descriptor_version = 0u32;
    // SAFETY: Same contract as `current_map_key`.
    let status = unsafe {
        (services.get_memory_map)(
            &mut size,
            map_page as *mut c_void,
            &mut key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };
    check_map_status(status, size)?;
    if descriptor_size < core::mem::size_of::<MemoryDescriptor>() {
        return Err(LoadError::BadDescriptorSize(descriptor_size));
    }

    let entries = size / descriptor_size;
    let mut count = 0usize;
    for index in 0..entries {
        // SAFETY: GetMemoryMap reported `size` bytes of valid descriptors at
        // `map_page`, and this index is within that range.
        let descriptor =
            unsafe { &*((map_page as usize + index * descriptor_size) as *const MemoryDescriptor) };
        let Some(len) = descriptor.len() else {
            continue;
        };
        if len == 0 {
            continue;
        }
        let kind = memory_kind(descriptor.memory_type);
        if count > 0 {
            // SAFETY: `regions` has room for MAX_MEMORY_REGIONS entries and
            // `count` is always below that.
            let previous = unsafe { &mut *regions.add(count - 1) };
            if previous.kind == kind as u32
                && let Some(end) = previous.end()
                && end == descriptor.physical_start
            {
                let Some(total) = previous.len.checked_add(len) else {
                    return Err(LoadError::TooManyRegions(entries));
                };
                previous.len = total;
                continue;
            }
        }
        if count == MAX_MEMORY_REGIONS {
            return Err(LoadError::TooManyRegions(entries));
        }
        // SAFETY: Bounds checked immediately above.
        unsafe {
            ptr::write(
                regions.add(count),
                MemoryRegion::new(descriptor.physical_start, len, kind),
            );
        }
        count += 1;
    }
    Ok(count)
}

fn check_map_status(status: Status, size: usize) -> Result<(), LoadError> {
    match status {
        SUCCESS => Ok(()),
        BUFFER_TOO_SMALL => Err(if size > MEMORY_MAP_BYTES {
            LoadError::MemoryMapTooLarge
        } else {
            LoadError::MemoryMap { status }
        }),
        other => Err(LoadError::MemoryMap { status: other }),
    }
}

fn memory_kind(memory_type: u32) -> MemoryKind {
    match memory_type {
        0 => MemoryKind::Reserved,
        1 | 2 => MemoryKind::Loader,
        3 | 4 => MemoryKind::BootServices,
        5 | 6 => MemoryKind::RuntimeServices,
        7 => MemoryKind::Usable,
        8 => MemoryKind::Unusable,
        9 => MemoryKind::AcpiReclaimable,
        10 => MemoryKind::AcpiNvs,
        11 | 12 => MemoryKind::Mmio,
        13 | 14 => MemoryKind::Other,
        _ => MemoryKind::Reserved,
    }
}

unsafe fn allocate_at(services: &BootServices, address: u64, pages: u64) -> Result<(), LoadError> {
    let mut target = address;
    // SAFETY: Firmware contract. The loader checks that the returned address
    // matches the kernel's linked address.
    let status = unsafe {
        (services.allocate_pages)(
            ALLOCATE_ADDRESS,
            MEMORY_TYPE_LOADER_DATA,
            pages as usize,
            &mut target,
        )
    };
    if status != SUCCESS {
        return Err(LoadError::Allocation { status });
    }
    if target != address {
        return Err(LoadError::AddressMismatch {
            wanted: address,
            got: target,
        });
    }
    Ok(())
}

unsafe fn allocate_any(services: &BootServices, pages: u64) -> Result<u64, LoadError> {
    let mut address = 0u64;
    // SAFETY: Firmware contract.
    let status = unsafe {
        (services.allocate_pages)(
            ALLOCATE_ANY_PAGES,
            MEMORY_TYPE_LOADER_DATA,
            pages as usize,
            &mut address,
        )
    };
    if status != SUCCESS {
        return Err(LoadError::Allocation { status });
    }
    if address == 0 {
        return Err(LoadError::Allocation { status });
    }
    Ok(address)
}

#[inline]
fn halt() -> ! {
    loop {
        // SAFETY: Halting the CPU is always safe here; there is no kernel to
        // return to.
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}

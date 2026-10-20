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

use aether_abi::{
    BootArtifact, BootInfo, FramebufferInfo, MAX_MEMORY_REGIONS, MAX_STAGED_APPIMAGE_BYTES,
    MAX_STAGED_RECORD_BYTES, MemoryKind, MemoryRegion,
};
use aether_boot_common::elf;
use aether_boot_common::{PAGE_SIZE, memory, pages_for};
use core::ffi::c_void;
use core::fmt;
use core::panic::PanicInfo;
use core::ptr;
use uefi::{
    ALLOCATE_ADDRESS, ALLOCATE_MAX_ADDRESS, BUFFER_TOO_SMALL, BootServices, FILE_INFO_GUID,
    FILE_MODE_READ, FileProtocol, GRAPHICS_OUTPUT_GUID, GraphicsOutput, Handle, LOADED_IMAGE_GUID,
    LoadedImage, MEMORY_TYPE_LOADER_DATA, MemoryDescriptor, SIMPLE_FILE_SYSTEM_GUID, SUCCESS,
    SimpleFileSystem, Status, SystemTable,
};

/// Kernel image embedded by the build. The path is supplied by `build.rs`, so
/// the image on disk and the image in the loader cannot drift apart.
static KERNEL_ELF: &[u8] = include_bytes!(env!("AETHER_KERNEL_ELF"));

/// Boot-volume paths of the staged application artifacts.
///
/// The loader reads these files, keeps their pages allocated across
/// `ExitBootServices`, and publishes the physical extents in `BootInfo`. It
/// never executes the AppImage runtime.
const APPIMAGE_PATH: &[u16] = &[
    0x005c, 0x006c, 0x0069, 0x006e, 0x0075, 0x0078, 0x005c, 0x0061, 0x0070, 0x0070, 0x002e, 0x0041,
    0x0070, 0x0070, 0x0049, 0x006d, 0x0061, 0x0067, 0x0065, 0x0000,
];
const APPIMAGE_PATH_NAME: &str = "\\linux\\app.AppImage";
const APPIMAGE_RECORD_PATH: &[u16] = &[
    0x005c, 0x006c, 0x0069, 0x006e, 0x0075, 0x0078, 0x005c, 0x0061, 0x0070, 0x0070, 0x002e, 0x0069,
    0x006d, 0x0070, 0x006f, 0x0072, 0x0074, 0x0000,
];
const APPIMAGE_RECORD_PATH_NAME: &str = "\\linux\\app.import";

/// Fixed buffer for the firmware file-info record. File names on the boot
/// volume are short, so this comfortably exceeds the variable-length header.
const FILE_INFO_BYTES: usize = 512;

/// Fixed buffer for the firmware memory map. OVMF's map is a few kilobytes;
/// 64 KiB leaves ample room for unusual firmware.
const MEMORY_MAP_BYTES: usize = 64 * 1024;

/// Highest address for loader-owned boot metadata.
///
/// The kernel identity-maps at most the first 16 GiB, so the hand-off block
/// and its memory map must be allocated in the low 4 GiB where they are
/// guaranteed to remain mapped after the CR3 switch.
const MAX_BOOT_METADATA_ADDRESS: u64 = 0xffff_ffff;

#[derive(Clone, Copy, Debug)]
enum LoadError {
    NoBootServices,
    Image(elf::ParseError),
    BootInfo(aether_abi::BootInfoError),
    Allocation {
        status: Status,
    },
    AddressMismatch {
        wanted: u64,
        got: u64,
    },
    BadDescriptorSize(usize),
    MemoryMap {
        status: Status,
    },
    MemoryMapTooLarge,
    TooManyRegions(usize),
    ExitBootServices(Status),
    EntryPoint(u64),
    Protocol {
        name: &'static str,
        status: Status,
    },
    BootFileMissing {
        path: &'static str,
        status: Status,
    },
    BootFileInfo {
        path: &'static str,
        status: Status,
    },
    BootFileEmpty {
        path: &'static str,
    },
    BootFileTooLarge {
        path: &'static str,
        size: u64,
        limit: u64,
    },
    BootFileRead {
        path: &'static str,
        status: Status,
    },
    BootFileShort {
        path: &'static str,
        expected: u64,
        actual: u64,
    },
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
                    "firmware memory map has {count} descriptors, limit is {MAX_MEMORY_REGIONS}"
                )
            }
            Self::ExitBootServices(status) => {
                write!(f, "ExitBootServices failed (status {status:#x})")
            }
            Self::EntryPoint(address) => write!(f, "invalid kernel entry point {address:#x}"),
            Self::Protocol { name, status } => {
                write!(f, "{name} protocol unavailable (status {status:#x})")
            }
            Self::BootFileMissing { path, status } => write!(
                f,
                "staged boot file {path} is missing or cannot be opened (status {status:#x})"
            ),
            Self::BootFileInfo { path, status } => {
                write!(
                    f,
                    "cannot query staged boot file {path} (status {status:#x})"
                )
            }
            Self::BootFileEmpty { path } => write!(f, "staged boot file {path} is empty"),
            Self::BootFileTooLarge { path, size, limit } => {
                write!(
                    f,
                    "staged boot file {path} is {size} bytes, limit is {limit}"
                )
            }
            Self::BootFileRead { path, status } => {
                write!(
                    f,
                    "cannot read staged boot file {path} (status {status:#x})"
                )
            }
            Self::BootFileShort {
                path,
                expected,
                actual,
            } => write!(
                f,
                "staged boot file {path} read {actual} bytes, expected {expected}"
            ),
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

    // Stage the application artifacts from the boot volume. These pages stay
    // allocated across ExitBootServices; the kernel receives their physical
    // extents and must verify the signed record before parsing the container.
    // SAFETY: Boot services are active and `handle` is this image's handle.
    let linux_appimage = unsafe {
        load_staged_file(
            services,
            handle,
            APPIMAGE_PATH,
            APPIMAGE_PATH_NAME,
            MAX_STAGED_APPIMAGE_BYTES,
        )?
    };
    // SAFETY: Same contract as the AppImage read.
    let linux_appimage_record = unsafe {
        load_staged_file(
            services,
            handle,
            APPIMAGE_RECORD_PATH,
            APPIMAGE_RECORD_PATH_NAME,
            MAX_STAGED_RECORD_BYTES,
        )?
    };
    serial_line!(
        "loader: staged appimage address={:#x} length={} record_address={:#x} record_length={}",
        linux_appimage.address,
        linux_appimage.length,
        linux_appimage_record.address,
        linux_appimage_record.length
    );

    let boot_info_page = unsafe { allocate_below_4g(services, 1)? };
    let map_page =
        unsafe { allocate_below_4g(services, pages_for(MEMORY_MAP_BYTES as u64).unwrap())? };
    let region_bytes = (MAX_MEMORY_REGIONS * core::mem::size_of::<MemoryRegion>()) as u64;
    let region_page = unsafe { allocate_below_4g(services, pages_for(region_bytes).unwrap())? };

    let boot_info = boot_info_page as *mut BootInfo;
    let regions = region_page as *mut MemoryRegion;
    let count = unsafe { read_memory_map(services, map_page, regions)? };
    let framebuffer = unsafe { query_framebuffer(services) };
    if framebuffer.is_present() {
        serial_line!(
            "loader: framebuffer base={:#x} size={} mode={}x{} stride={} format={}",
            framebuffer.base,
            framebuffer.size,
            framebuffer.width,
            framebuffer.height,
            framebuffer.stride,
            framebuffer.format
        );
    } else {
        serial_line!("loader: no linear framebuffer reported by firmware");
    }

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
                framebuffer,
                linux_appimage,
                linux_appimage_record,
                reserved: [0; 4],
            },
        );
    }

    // SAFETY: `boot_info` points to the initialized hand-off block and
    // `read_memory_map` filled `count` regions inside the bounded allocation.
    unsafe {
        (*boot_info).validate().map_err(LoadError::BootInfo)?;
        (*boot_info)
            .validate_regions()
            .map_err(LoadError::BootInfo)?;
    }
    // SAFETY: Validation succeeded and the region array is readable.
    let usable = unsafe { (*boot_info).usable_bytes() };
    serial_line!(
        "loader: memory regions={} usable={} MiB",
        unsafe { (*boot_info).memory_region_count },
        usable / (1024 * 1024)
    );

    let mut map_key = unsafe { current_map_key(services, map_page)? };
    let mut last_status = SUCCESS;
    for attempt in 0..8 {
        // SAFETY: Firmware contract; boot services are still active.
        let status = unsafe { (services.exit_boot_services)(handle, map_key) };
        last_status = status;
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
        // SAFETY: The boot information page is still owned by the loader and
        // `read_memory_map` bounded the new count by MAX_MEMORY_REGIONS.
        unsafe {
            (*boot_info).memory_region_count = new_count as u64;
            (*boot_info).validate().map_err(LoadError::BootInfo)?;
            (*boot_info)
                .validate_regions()
                .map_err(LoadError::BootInfo)?;
        }
    }

    Err(LoadError::ExitBootServices(last_status))
}

/// Reads a staged boot file into loader-owned pages.
///
/// The file size is queried first and bounded before allocation. The returned
/// pages are never freed: their physical extent is published in `BootInfo` and
/// the kernel treats them as loader-owned memory. Firmware strings, sizes, and
/// the returned read length are all treated as untrusted.
unsafe fn load_staged_file(
    services: &BootServices,
    image: Handle,
    path: &[u16],
    display_path: &'static str,
    limit: u64,
) -> Result<BootArtifact, LoadError> {
    let mut loaded: *mut c_void = ptr::null_mut();
    // SAFETY: Firmware contract. `image` is the loader's own image handle and
    // the GUID is architectural.
    let status = unsafe { (services.handle_protocol)(image, &LOADED_IMAGE_GUID, &mut loaded) };
    if status != SUCCESS || loaded.is_null() {
        return Err(LoadError::Protocol {
            name: "loaded-image",
            status,
        });
    }
    // SAFETY: The loaded-image protocol is valid until boot services exit.
    let device_handle = unsafe { (*(loaded as *const LoadedImage)).device_handle };

    let mut filesystem: *mut c_void = ptr::null_mut();
    // SAFETY: The device handle came from the loaded-image protocol and the
    // simple-file-system GUID is architectural.
    let status = unsafe {
        (services.handle_protocol)(device_handle, &SIMPLE_FILE_SYSTEM_GUID, &mut filesystem)
    };
    if status != SUCCESS || filesystem.is_null() {
        return Err(LoadError::Protocol {
            name: "simple-file-system",
            status,
        });
    }

    let mut root: *mut FileProtocol = ptr::null_mut();
    // SAFETY: `filesystem` points to a live simple-file-system interface.
    let status = unsafe {
        ((*(filesystem as *const SimpleFileSystem)).open_volume)(
            filesystem as *mut SimpleFileSystem,
            &mut root,
        )
    };
    if status != SUCCESS || root.is_null() {
        return Err(LoadError::Protocol {
            name: "file-system-volume",
            status,
        });
    }

    let mut file: *mut FileProtocol = ptr::null_mut();
    // SAFETY: `root` is a live file handle and `path` is NUL-terminated UTF-16.
    let status = unsafe { ((*root).open)(root, &mut file, path.as_ptr(), FILE_MODE_READ, 0) };
    // The root handle is no longer needed once the file is open.
    // SAFETY: `root` is live and was opened by this function.
    unsafe {
        let _ = ((*root).close)(root);
    }
    if status != SUCCESS || file.is_null() {
        return Err(LoadError::BootFileMissing {
            path: display_path,
            status,
        });
    }

    let mut info = [0u8; FILE_INFO_BYTES];
    let mut info_size = FILE_INFO_BYTES;
    // SAFETY: `file` is live, `info` is writable for `info_size` bytes, and the
    // file-info GUID is architectural.
    let status = unsafe {
        ((*file).get_info)(
            file,
            &FILE_INFO_GUID,
            &mut info_size,
            info.as_mut_ptr() as *mut c_void,
        )
    };
    if status != SUCCESS || info_size < core::mem::size_of::<u64>() * 2 {
        // SAFETY: `file` is live and opened by this function.
        unsafe {
            let _ = ((*file).close)(file);
        }
        return Err(LoadError::BootFileInfo {
            path: display_path,
            status,
        });
    }
    // SAFETY: The firmware reported at least the fixed header. `file_size`
    // follows `size`, and the read is unaligned because firmware buffers carry
    // no Rust alignment guarantee.
    let file_size = unsafe { ptr::read_unaligned(info.as_ptr().add(8) as *const u64) };
    if file_size == 0 {
        // SAFETY: `file` is live and opened by this function.
        unsafe {
            let _ = ((*file).close)(file);
        }
        return Err(LoadError::BootFileEmpty { path: display_path });
    }
    if file_size > limit {
        // SAFETY: `file` is live and opened by this function.
        unsafe {
            let _ = ((*file).close)(file);
        }
        return Err(LoadError::BootFileTooLarge {
            path: display_path,
            size: file_size,
            limit,
        });
    }

    let pages = pages_for(file_size).ok_or(LoadError::BootFileTooLarge {
        path: display_path,
        size: file_size,
        limit,
    })?;
    // SAFETY: Firmware contract; the allocation is retained for the hand-off.
    let address = unsafe { allocate_below_4g(services, pages)? };
    let mut read_size = file_size as usize;
    // SAFETY: `address` is a live allocation of `pages` pages, which is at
    // least `file_size` bytes, and `file` is live.
    let status = unsafe { ((*file).read)(file, &mut read_size, address as *mut c_void) };
    // SAFETY: `file` is live and opened by this function.
    unsafe {
        let _ = ((*file).close)(file);
    }
    if status != SUCCESS {
        return Err(LoadError::BootFileRead {
            path: display_path,
            status,
        });
    }
    if read_size as u64 != file_size {
        return Err(LoadError::BootFileShort {
            path: display_path,
            expected: file_size,
            actual: read_size as u64,
        });
    }

    Ok(BootArtifact {
        address,
        length: file_size,
    })
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
        // `map_page`, and this index is within that range. The descriptor is
        // copied with an unaligned read because firmware is not trusted to
        // return a descriptor stride that satisfies Rust's alignment rule.
        let descriptor = unsafe {
            ptr::read_unaligned(
                (map_page as usize + index * descriptor_size) as *const MemoryDescriptor,
            )
        };
        let Some(len) = descriptor.len() else {
            continue;
        };
        if len == 0 {
            continue;
        }
        if count == MAX_MEMORY_REGIONS {
            return Err(LoadError::TooManyRegions(entries));
        }
        // SAFETY: Bounds checked immediately above.
        unsafe {
            ptr::write(
                regions.add(count),
                MemoryRegion::new(
                    descriptor.physical_start,
                    len,
                    memory_kind(descriptor.memory_type),
                ),
            );
        }
        count += 1;
    }
    // Some UEFI implementations return an unsorted map even though the
    // specification requires ascending addresses. Sort before merging so the
    // hand-off contract is met without trusting firmware ordering.
    // SAFETY: `regions` points to an allocation of MAX_MEMORY_REGIONS entries
    // and `count` is at most that limit.
    let regions = unsafe { core::slice::from_raw_parts_mut(regions, count) };
    Ok(memory::normalize_regions(regions))
}

fn check_map_status(status: Status, size: usize) -> Result<(), LoadError> {
    match status {
        SUCCESS if size > MEMORY_MAP_BYTES => Err(LoadError::MemoryMapTooLarge),
        SUCCESS => Ok(()),
        BUFFER_TOO_SMALL => Err(if size > MEMORY_MAP_BYTES {
            LoadError::MemoryMapTooLarge
        } else {
            LoadError::MemoryMap { status }
        }),
        other => Err(LoadError::MemoryMap { status: other }),
    }
}

/// Queries the firmware's linear framebuffer.
///
/// Returns [`FramebufferInfo::NONE`] when GOP is absent or only supports
/// blitting. The loader does not change the mode: the kernel accepts whatever
/// mode the firmware selected, which keeps the boot path robust on real
/// hardware and on QEMU.
unsafe fn query_framebuffer(services: &BootServices) -> FramebufferInfo {
    let mut interface: *mut c_void = ptr::null_mut();
    // SAFETY: Firmware supplies the boot-services table and the GOP GUID is
    // architectural. A null interface is treated as an absent framebuffer.
    let status = unsafe {
        (services.locate_protocol)(&GRAPHICS_OUTPUT_GUID, ptr::null_mut(), &mut interface)
    };
    if status != SUCCESS || interface.is_null() {
        serial_line!(
            "loader: GOP lookup status={status:#x} interface={:#x}",
            interface as usize
        );
        return FramebufferInfo::NONE;
    }

    // SAFETY: LocateProtocol returned a live GOP interface for this boot. Its
    // mode and mode-information pointers are valid until ExitBootServices.
    unsafe {
        let gop = &*(interface as *const GraphicsOutput);
        if gop.mode.is_null() {
            serial_line!("loader: GOP mode pointer is null");
            return FramebufferInfo::NONE;
        }
        let mode = &*gop.mode;
        if mode.info.is_null()
            || mode.size_of_info < core::mem::size_of::<uefi::GraphicsOutputModeInformation>()
            || !(mode.info as usize)
                .is_multiple_of(core::mem::align_of::<uefi::GraphicsOutputModeInformation>())
        {
            serial_line!("loader: GOP mode information is absent, truncated, or misaligned");
            return FramebufferInfo::NONE;
        }
        let info = &*mode.info;
        let Some(framebuffer) =
            info.framebuffer(mode.frame_buffer_base, mode.frame_buffer_size as u64)
        else {
            serial_line!(
                "loader: GOP mode unsupported {}x{} format={}",
                info.horizontal_resolution,
                info.vertical_resolution,
                info.pixel_format
            );
            return FramebufferInfo::NONE;
        };
        serial_line!(
            "loader: GOP {}x{} stride={} format={}",
            info.horizontal_resolution,
            info.vertical_resolution,
            framebuffer.stride,
            info.pixel_format
        );

        framebuffer
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

unsafe fn allocate_below_4g(services: &BootServices, pages: u64) -> Result<u64, LoadError> {
    let mut address = MAX_BOOT_METADATA_ADDRESS;
    // SAFETY: Firmware contract. AllocateMaxAddress may return any page range
    // that ends at or below the supplied maximum.
    let status = unsafe {
        (services.allocate_pages)(
            ALLOCATE_MAX_ADDRESS,
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

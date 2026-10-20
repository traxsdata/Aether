// SPDX-License-Identifier: Apache-2.0
//! Minimal UEFI bindings used by the loader.
//!
//! Only the table entries the loader actually calls are typed. Unused entries
//! are represented as pointer-sized slots, which preserves the table layout
//! without pretending every firmware prototype is implemented here.

#![allow(dead_code)]

pub use aether_boot_common::graphics::GraphicsOutputModeInformation;
use core::ffi::c_void;

/// Opaque firmware handle.
pub type Handle = *mut c_void;
/// UEFI status code. Zero means success.
pub type Status = usize;

/// Success status.
pub const SUCCESS: Status = 0;
/// The caller-provided buffer was too small.
pub const BUFFER_TOO_SMALL: Status = 0x8000_0000_0000_0005;

/// Allocate at the address supplied in the final argument.
pub const ALLOCATE_ADDRESS: u32 = 2;
/// Allocate at or below the maximum address supplied in the final argument.
pub const ALLOCATE_MAX_ADDRESS: u32 = 1;

/// `EfiLoaderData`.
pub const MEMORY_TYPE_LOADER_DATA: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TableHeader {
    pub signature: u64,
    pub revision: u32,
    pub header_size: u32,
    pub crc32: u32,
    pub reserved: u32,
}

/// UEFI GUID.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID`.
pub const GRAPHICS_OUTPUT_GUID: Guid = Guid {
    data1: 0x9042_a9de,
    data2: 0x23dc,
    data3: 0x4a38,
    data4: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};

/// `EFI_LOADED_IMAGE_PROTOCOL_GUID`.
pub const LOADED_IMAGE_GUID: Guid = Guid {
    data1: 0x5b1b_31a1,
    data2: 0x9562,
    data3: 0x11d2,
    data4: [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
};

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID`.
pub const SIMPLE_FILE_SYSTEM_GUID: Guid = Guid {
    data1: 0x964e_5b22,
    data2: 0x6459,
    data3: 0x11d2,
    data4: [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
};

/// `EFI_FILE_INFO_ID`.
pub const FILE_INFO_GUID: Guid = Guid {
    data1: 0x0957_6e92,
    data2: 0x6d3f,
    data3: 0x11d2,
    data4: [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
};

/// `EFI_FILE_MODE_READ`.
pub const FILE_MODE_READ: u64 = 1;

/// `EFI_LOADED_IMAGE_PROTOCOL`.
///
/// Only `device_handle` is consumed; the remaining fields preserve the
/// firmware-defined layout.
#[repr(C)]
pub struct LoadedImage {
    pub revision: u32,
    pub parent_handle: Handle,
    pub system_table: *mut SystemTable,
    pub device_handle: Handle,
    pub file_path: *mut c_void,
    pub reserved: *mut c_void,
    pub load_options_size: u32,
    pub load_options: *mut c_void,
    pub image_base: *mut c_void,
    pub image_size: u64,
    pub image_code_type: u32,
    pub image_data_type: u32,
    pub unload: usize,
}

/// `EFI_FILE_PROTOCOL`.
///
/// Read, open, close, get-info, and the ABI slots around them are typed; write
/// and extension entries are preserved as pointer-sized slots.
#[repr(C)]
pub struct FileProtocol {
    pub revision: u64,
    pub open: unsafe extern "efiapi" fn(
        *mut FileProtocol,
        *mut *mut FileProtocol,
        *const u16,
        u64,
        u64,
    ) -> Status,
    pub close: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
    pub delete: usize,
    pub read: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut c_void) -> Status,
    pub write: usize,
    pub get_position: usize,
    pub set_position: usize,
    pub get_info: unsafe extern "efiapi" fn(
        *mut FileProtocol,
        *const Guid,
        *mut usize,
        *mut c_void,
    ) -> Status,
    pub set_info: usize,
    pub flush: usize,
    pub open_ex: usize,
    pub read_ex: usize,
    pub write_ex: usize,
    pub flush_ex: usize,
}

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`.
#[repr(C)]
pub struct SimpleFileSystem {
    pub revision: u64,
    pub open_volume:
        unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

/// `EFI_TIME`, the timestamp embedded in `EFI_FILE_INFO`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EfiTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub pad1: u8,
    pub nanosecond: u32,
    pub timezone: i16,
    pub daylight: u8,
    pub pad2: u8,
}

/// `EFI_FILE_INFO` header.
///
/// Firmware returns a variable-length structure whose filename follows
/// `attribute`. The loader reads only the fixed header with unaligned reads, so
/// the trailing name is represented by one placeholder element.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FileInfo {
    pub size: u64,
    pub file_size: u64,
    pub physical_size: u64,
    pub create_time: EfiTime,
    pub last_access_time: EfiTime,
    pub modification_time: EfiTime,
    pub attribute: u64,
    pub file_name: [u16; 1],
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE`.
#[repr(C)]
pub struct GraphicsOutputMode {
    pub max_mode: u32,
    pub mode: u32,
    pub info: *mut GraphicsOutputModeInformation,
    pub size_of_info: usize,
    pub frame_buffer_base: u64,
    pub frame_buffer_size: usize,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL`.
///
/// Only the mode field is consumed; query/set/blt are kept in their ABI slots.
#[repr(C)]
pub struct GraphicsOutput {
    pub query_mode: usize,
    pub set_mode: usize,
    pub blt: usize,
    pub mode: *mut GraphicsOutputMode,
}

/// UEFI boot services table. Fields before `exit_boot_services` are ordered
/// exactly as in the UEFI specification.
#[repr(C)]
pub struct BootServices {
    pub header: TableHeader,
    pub raise_tpl: usize,
    pub restore_tpl: usize,
    pub allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    pub free_pages: usize,
    pub get_memory_map: unsafe extern "efiapi" fn(
        *mut usize,
        *mut c_void,
        *mut usize,
        *mut usize,
        *mut u32,
    ) -> Status,
    pub allocate_pool: usize,
    pub free_pool: usize,
    pub create_event: usize,
    pub set_timer: usize,
    pub wait_for_event: usize,
    pub signal_event: usize,
    pub close_event: usize,
    pub check_event: usize,
    pub install_protocol_interface: usize,
    pub reinstall_protocol_interface: usize,
    pub uninstall_protocol_interface: usize,
    pub handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    pub reserved: usize,
    pub register_protocol_notify: usize,
    pub locate_handle: usize,
    pub locate_device_path: usize,
    pub install_configuration_table: usize,
    pub load_image: usize,
    pub start_image: usize,
    pub exit: usize,
    pub unload_image: usize,
    pub exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    pub get_next_monotonic_count: usize,
    pub stall: usize,
    pub set_watchdog_timer: usize,
    pub connect_controller: usize,
    pub disconnect_controller: usize,
    pub open_protocol: usize,
    pub close_protocol: usize,
    pub open_protocol_information: usize,
    pub protocols_per_handle: usize,
    pub locate_handle_buffer: usize,
    pub locate_protocol:
        unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
    pub install_multiple_protocol_interfaces: usize,
    pub uninstall_multiple_protocol_interfaces: usize,
    pub calculate_crc32: usize,
}

/// UEFI system table. Only the boot-services pointer is consumed.
#[repr(C)]
pub struct SystemTable {
    pub header: TableHeader,
    pub firmware_vendor: *const u16,
    pub firmware_revision: u32,
    pub console_in_handle: Handle,
    pub console_in: *mut c_void,
    pub console_out_handle: Handle,
    pub console_out: *mut c_void,
    pub standard_error_handle: Handle,
    pub standard_error: *mut c_void,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut c_void,
}

/// UEFI memory descriptor as returned by `GetMemoryMap`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryDescriptor {
    pub memory_type: u32,
    pub pad: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub number_of_pages: u64,
    pub attributes: u64,
}

impl MemoryDescriptor {
    /// Length in bytes, or `None` on overflow.
    #[must_use]
    pub const fn len(&self) -> Option<u64> {
        self.number_of_pages.checked_mul(crate::PAGE_SIZE)
    }
}

// SPDX-License-Identifier: Apache-2.0
//! Minimal UEFI bindings used by the loader.
//!
//! Only the table entries the loader actually calls are typed. Unused entries
//! are represented as pointer-sized slots, which preserves the table layout
//! without pretending every firmware prototype is implemented here.

#![allow(dead_code)]

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
    pub handle_protocol: usize,
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
    pub locate_protocol: usize,
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

// SPDX-License-Identifier: Apache-2.0
//! Aether kernel entry point.
//!
//! This is the first-sprint skeleton: it validates the loader hand-off and
//! reports boot state on the serial console. Memory management, scheduling,
//! capabilities, and IPC are deliberately not present yet.

#![no_std]
#![no_main]

mod arch;

use aether_abi::BootInfo;
use core::panic::PanicInfo;

core::arch::global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
_start:
    cli
    lea rsp, [rip + __stack_top]
    xor rbp, rbp
    call kernel_main
    ud2

    .section .bss, "aw", @nobits
    .balign 16
__stack_bottom:
    .skip 65536
__stack_top:
    "#
);

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    arch::x86_64::serial::write_str("AETHER_KERNEL_PANIC\n");
    serial_line!("kernel panic: {info}");
    arch::x86_64::halt()
}

/// Kernel entry called by the loader.
///
/// # Safety
///
/// `boot_info` must point to a readable `BootInfo` written by the loader. The
/// pointer is validated before any field other than the magic is used.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    arch::x86_64::serial::init();
    serial_line!(
        "AETHER_KERNEL_OK stage=entry version={} commit={}",
        env!("CARGO_PKG_VERSION"),
        option_env!("AETHER_GIT_COMMIT").unwrap_or("unknown")
    );

    if boot_info.is_null() {
        boot_failure("null boot information pointer");
    }

    // SAFETY: The caller guarantees a readable structure. `validate` checks
    // the header before the memory map is dereferenced.
    let info = unsafe { &*boot_info };
    if let Err(error) = info.validate() {
        serial_line!("AETHER_BOOT_FAILED reason={error}");
        arch::x86_64::halt();
    }

    // SAFETY: `validate` checked the pointer, count, and structure size.
    if let Err(error) = unsafe { info.validate_regions() } {
        serial_line!("AETHER_BOOT_FAILED reason={error}");
        arch::x86_64::halt();
    }
    // SAFETY: Same invariant as above.
    let usable = unsafe { info.usable_bytes() };

    serial_line!(
        "kernel: handoff magic={:#x} version={} regions={} usable={} MiB",
        info.magic,
        info.version,
        info.memory_region_count,
        usable / (1024 * 1024)
    );
    serial_line!(
        "kernel: image={:#x}..{:#x} entry={:#x}",
        info.kernel_start,
        info.kernel_end,
        info.entry_point
    );
    serial_line!("kernel: long mode, ring 0, interrupts disabled");
    serial_line!(
        "AETHER_BOOT_OK stage=kernel version={}",
        env!("CARGO_PKG_VERSION")
    );
    arch::x86_64::halt()
}

fn boot_failure(reason: &str) -> ! {
    serial_line!("AETHER_BOOT_FAILED reason={reason}");
    arch::x86_64::halt()
}

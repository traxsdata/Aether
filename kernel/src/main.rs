// SPDX-License-Identifier: Apache-2.0
//! Aether kernel entry point.
//!
//! Phase 1: validate the loader hand-off, take ownership of the CPU
//! descriptor/interrupt tables, initialize physical memory, install kernel page
//! tables, and report the result on the serial console. There is no scheduler,
//! capability system, or user space yet.

#![no_std]
#![no_main]

mod arch;
mod memory;
mod scheduler;

use aether_abi::{BootInfo, MemoryKind, MemoryRegion};
use core::panic::PanicInfo;

/// Size of the initial kernel stack in bytes.
const KERNEL_STACK_SIZE: usize = 64 * 1024;

/// Kernel boot stack. Firmware boot-services stacks are not reused.
#[unsafe(link_section = ".bss.stack")]
static mut KERNEL_STACK: [u8; KERNEL_STACK_SIZE] = [0; KERNEL_STACK_SIZE];

core::arch::global_asm!(
    r#"
    .section .text.entry, "ax"
    .global _start
    .type _start, @function
_start:
    cli
    lea rax, [rip + {stack}]
    add rax, {stack_size}
    mov rsp, rax
    xor ebp, ebp

    /* The loader passes the BootInfo physical address in RDI. */
    call {kmain}

2:
    hlt
    jmp 2b
    .size _start, . - _start
    "#,
    stack = sym KERNEL_STACK,
    stack_size = const KERNEL_STACK_SIZE,
    kmain = sym kernel_main,
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
/// `boot_info` must be null or point to a readable [`BootInfo`] block written
/// by the Aether loader. The kernel runs identity-mapped, so the physical
/// address is directly dereferenceable.
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

    // SAFETY: The loader guarantees a readable structure; validation below
    // checks its contents before the memory map is trusted.
    let info = unsafe { &*boot_info };
    if let Err(error) = info.validate() {
        serial_line!("AETHER_BOOT_FAILED reason=boot-info: {error}");
        arch::x86_64::halt();
    }
    serial_line!(
        "boot: hand-off validated (version {}, {} bytes)",
        info.version,
        info.struct_size
    );

    // SAFETY: `validate` succeeded and loader-owned memory remains mapped.
    if let Err(error) = unsafe { info.validate_regions() } {
        serial_line!("AETHER_BOOT_FAILED reason=memory-map: {error}");
        arch::x86_64::halt();
    }
    // SAFETY: Same invariant as above.
    let usable = unsafe { info.usable_bytes() };
    // SAFETY: Same invariant as above.
    let regions = unsafe { info.regions() };
    serial_line!(
        "boot: memory map: {} regions, {} MiB allocatable",
        regions.len(),
        usable / (1024 * 1024)
    );
    serial_line!(
        "boot: kernel image [{:#018x}, {:#018x}), entry {:#018x}",
        info.kernel_start,
        info.kernel_end,
        info.entry_point
    );

    arch::x86_64::init();

    let mut frames = memory::frame_allocator();
    let _initial_stats = frames.initialize(regions, info.kernel_start, info.kernel_end);

    // The loader already reports these as loader-owned. Reserve them again so
    // a loader regression cannot hand the hand-off block to the allocator.
    frames.reserve_range(boot_info as u64, core::mem::size_of::<BootInfo>() as u64);
    let map_bytes = info
        .memory_region_count
        .saturating_mul(core::mem::size_of::<MemoryRegion>() as u64);
    frames.reserve_range(info.memory_regions, map_bytes);

    match memory::paging::init(&mut frames, info, regions) {
        Ok(paging) => {
            serial_line!(
                "boot: paging: identity map to {} MiB, {} page-table frames, CR3={:#018x}",
                paging.mapped_bytes / (1024 * 1024),
                paging.page_table_frames,
                paging.pml4_address
            );
        }
        Err(error) => {
            serial_line!("AETHER_BOOT_FAILED reason=paging: {error:?}");
            arch::x86_64::halt();
        }
    }

    // Boot-services memory is safe to reuse only after CR3 points at our own
    // tables. This includes any firmware page tables left behind by UEFI.
    for region in regions {
        if region.kind() == Some(MemoryKind::BootServices) {
            frames.free_range(region.start, region.len);
        }
    }
    let frame_stats = frames.stats();
    serial_line!(
        "boot: frame allocator: {} MiB free ({} frames), bitmap capacity {} MiB",
        frame_stats.free_bytes() / (1024 * 1024),
        frame_stats.free_frames,
        frame_stats.total_bytes() / (1024 * 1024)
    );
    serial_line!("AETHER_MM_OK");

    #[cfg(feature = "fault-selftest")]
    {
        serial_line!("boot: fault self-test: executing breakpoint");
        // SAFETY: `int3` is a deliberate CPU exception. The IDT is installed
        // and the handler reports the vector, then halts.
        unsafe { core::arch::asm!("int3", options(nomem, nostack, preserves_flags)) };
    }

    serial_line!("boot: x86-64 execution environment ready");
    scheduler::run(&mut frames)
}

fn boot_failure(reason: &str) -> ! {
    serial_line!("AETHER_BOOT_FAILED reason={reason}");
    arch::x86_64::halt()
}

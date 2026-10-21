// SPDX-License-Identifier: Apache-2.0
//! Aether kernel entry point.
//!
//! Validate the loader hand-off, take ownership of the CPU descriptor/interrupt
//! tables, configure the protections required by the page tables, initialize
//! physical memory, install kernel page tables, and hand control to the
//! scheduler. The scheduler starts ring-0 service tests and the first ring-3
//! process created from an embedded native ELF image.

#![no_std]
#![no_main]

mod apps;
mod arch;
mod desktop;
mod display;
mod fs;
mod graphics;
mod icons;
mod input;
mod ipc;
mod linux;
mod linux_loader;
mod linux_policy;
mod linuxd;
mod memfd;
mod memory;
mod packaging;
mod portal;
mod recovery;
mod runtime;
mod scheduler;
mod shell;
mod supervisor;
mod syscall;
mod updates;

extern crate alloc;

use aether_abi::{BootInfo, MemoryKind, MemoryRegion};
use core::panic::PanicInfo;

/// Size of the initial kernel stack in bytes.
///
/// The Linux import path walks SquashFS metadata and decompression frames on
/// the boot stack, and the feature builds add recovery and verification
/// frames. The `.bss.stack` section is placed next to the IDT, so an overflow
/// corrupts interrupt state instead of faulting cleanly; keep enough headroom
/// for the largest runtime rootfs.
const KERNEL_STACK_SIZE: usize = 512 * 1024;

/// Kernel boot stack. Firmware boot-services stacks are not reused.
///
/// The wrapper guarantees 16-byte alignment for the initial RSP. Without it
/// the linker may place this byte array directly after a smaller static,
/// producing a misaligned stack and corrupting formatted output.
#[repr(align(16))]
#[allow(dead_code)]
struct KernelStack([u8; KERNEL_STACK_SIZE]);

#[unsafe(link_section = ".bss.stack")]
static mut KERNEL_STACK: KernelStack = KernelStack([0; KERNEL_STACK_SIZE]);

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
    if !(boot_info as usize).is_multiple_of(core::mem::align_of::<BootInfo>()) {
        boot_failure("misaligned boot information pointer");
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
    // The loader stages the AppImage and record as EfiLoaderData. Refuse a
    // present descriptor that does not fall inside a loader-owned region
    // before the kernel ever forms a slice over it.
    if !artifact_in_loader_memory(info.linux_appimage, regions)
        || !artifact_in_loader_memory(info.linux_appimage_record, regions)
        || !artifact_in_loader_memory(info.linux_runtime_rootfs, regions)
        || !artifact_in_loader_memory(info.linux_runtime_rootfs_record, regions)
    {
        boot_failure("staged artifact outside loader memory");
    }

    arch::x86_64::init();
    // SAFETY: Boot is single-threaded, interrupts are disabled, and this runs
    // before the kernel loads page tables containing NO_EXECUTE entries.
    if let Err(error) = unsafe { arch::x86_64::cpu::init() } {
        serial_line!("AETHER_BOOT_FAILED reason=cpu: {error:?}");
        arch::x86_64::halt();
    }
    serial_line!("boot: cpu: NX and write-protect enabled");

    memory::init_frame_allocator();
    memory::with_frame_allocator(|frames| {
        frames.initialize(regions, info.kernel_start, info.kernel_end)
    });

    // The loader already reports these as loader-owned. Reserve them again so
    // a loader regression cannot hand the hand-off block to the allocator.
    memory::with_frame_allocator(|frames| {
        frames.reserve_range(boot_info as u64, core::mem::size_of::<BootInfo>() as u64);
        let map_bytes = info
            .memory_region_count
            .saturating_mul(core::mem::size_of::<MemoryRegion>() as u64);
        frames.reserve_range(info.memory_regions, map_bytes);
        // The loader retains the staged AppImage and its signed record. They
        // are loader-owned in the firmware map; reserve them again so a loader
        // regression cannot hand these pages to the allocator.
        if info.linux_appimage.is_present() {
            frames.reserve_range(info.linux_appimage.address, info.linux_appimage.length);
        }
        if info.linux_appimage_record.is_present() {
            frames.reserve_range(
                info.linux_appimage_record.address,
                info.linux_appimage_record.length,
            );
        }
        if info.linux_runtime_rootfs.is_present() {
            frames.reserve_range(
                info.linux_runtime_rootfs.address,
                info.linux_runtime_rootfs.length,
            );
        }
        if info.linux_runtime_rootfs_record.is_present() {
            frames.reserve_range(
                info.linux_runtime_rootfs_record.address,
                info.linux_runtime_rootfs_record.length,
            );
        }
    });

    if info.linux_appimage.is_present() && info.linux_appimage_record.is_present() {
        serial_line!(
            "boot: loader staged AppImage address={:#x} length={} record={:#x} record_length={}",
            info.linux_appimage.address,
            info.linux_appimage.length,
            info.linux_appimage_record.address,
            info.linux_appimage_record.length
        );
    } else {
        serial_line!("boot: no loader-staged AppImage in the hand-off");
    }
    if info.linux_runtime_rootfs.is_present() && info.linux_runtime_rootfs_record.is_present() {
        serial_line!(
            "boot: loader staged runtime rootfs address={:#x} length={} record={:#x} record_length={}",
            info.linux_runtime_rootfs.address,
            info.linux_runtime_rootfs.length,
            info.linux_runtime_rootfs_record.address,
            info.linux_runtime_rootfs_record.length
        );
    }
    linux::stage_appimage(info.linux_appimage, info.linux_appimage_record);
    linux::stage_runtime_rootfs(info.linux_runtime_rootfs, info.linux_runtime_rootfs_record);

    let framebuffer_mapped =
        match memory::with_frame_allocator(|frames| memory::paging::init(frames, info, regions)) {
            Some(Ok(paging)) => {
                serial_line!(
                    "boot: paging: identity map to {} MiB, {} page-table frames, CR3={:#018x}",
                    paging.mapped_bytes / (1024 * 1024),
                    paging.page_table_frames,
                    paging.pml4_address
                );
                if paging.framebuffer_mapped {
                    serial_line!(
                        "boot: framebuffer mapped {}x{} stride={} format={}",
                        info.framebuffer.width,
                        info.framebuffer.height,
                        info.framebuffer.stride,
                        info.framebuffer.format
                    );
                } else {
                    serial_line!("boot: framebuffer unavailable; graphics disabled");
                }
                paging.framebuffer_mapped
            }
            Some(Err(error)) => {
                serial_line!("AETHER_BOOT_FAILED reason=paging: {error:?}");
                arch::x86_64::halt();
            }
            None => boot_failure("frame allocator unavailable"),
        };

    match graphics::init(&info.framebuffer, framebuffer_mapped) {
        Ok(true) => serial_line!("boot: graphics service ready"),
        Ok(false) => serial_line!("boot: graphics service headless"),
        Err(error) => {
            serial_line!("AETHER_BOOT_FAILED reason=graphics: {error:?}");
            arch::x86_64::halt();
        }
    }

    // Boot-services memory is safe to reuse only after CR3 points at our own
    // tables. This includes any firmware page tables left behind by UEFI.
    memory::with_frame_allocator(|frames| {
        for region in regions {
            if region.kind() == Some(MemoryKind::BootServices) {
                frames.free_range(region.start, region.len);
            }
        }
    });
    let Some(frame_stats) = memory::frame_stats() else {
        boot_failure("frame allocator unavailable");
    };
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
    scheduler::run()
}

fn boot_failure(reason: &str) -> ! {
    serial_line!("AETHER_BOOT_FAILED reason={reason}");
    arch::x86_64::halt()
}

/// Returns `true` when an absent artifact or a present artifact fully inside a
/// loader-owned memory region. This prevents the import path from
/// dereferencing a firmware-supplied address outside the identity-mapped
/// loader allocation.
fn artifact_in_loader_memory(artifact: aether_abi::BootArtifact, regions: &[MemoryRegion]) -> bool {
    if !artifact.is_present() {
        return true;
    }
    let Some(end) = artifact.address.checked_add(artifact.length) else {
        return false;
    };
    regions.iter().any(|region| {
        region.kind() == Some(MemoryKind::Loader)
            && artifact.address >= region.start
            && region.end().is_some_and(|region_end| end <= region_end)
    })
}

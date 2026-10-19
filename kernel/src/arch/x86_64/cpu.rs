// SPDX-License-Identifier: Apache-2.0
//! CPU feature configuration required by the kernel.
//!
//! The kernel's page tables mark data and stacks non-executable and kernel
//! text non-writable. Those bits only have their intended effect when the CPU
//! is configured explicitly:
//!
//! * `IA32_EFER.NXE` must be set before a page table containing `NO_EXECUTE`
//!   entries is loaded.
//! * `CR0.WP` must be set for supervisor writes to read-only pages to fault.
//!
//! UEFI normally leaves both features enabled, but the kernel must not depend
//! on firmware state for a security property.

use core::arch::asm;
use core::arch::x86_64::__cpuid;

const IA32_EFER: u32 = 0xc000_0080;
const EFER_NXE: u64 = 1 << 11;

const CR0_WP: u64 = 1 << 16;

const CPUID_EXTENDED_MAX: u32 = 0x8000_0000;
const CPUID_EXTENDED_FEATURES: u32 = 0x8000_0001;
const CPUID_FEATURE_NX: u32 = 1 << 20;

/// CPU features required by the kernel are unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuError {
    /// The processor does not implement the no-execute page protection.
    NxUnsupported,
}

/// Enables NX page protection and supervisor write protection.
///
/// # Safety
///
/// Must run once during boot, at CPL0, with interrupts disabled and before the
/// kernel activates page tables that use `NO_EXECUTE` entries.
pub unsafe fn init() -> Result<(), CpuError> {
    // CPUID is available in long mode; the extended leaves are queried only
    // after checking the maximum supported extended leaf.
    let extended_max = __cpuid(CPUID_EXTENDED_MAX).eax;
    if extended_max < CPUID_EXTENDED_FEATURES {
        return Err(CpuError::NxUnsupported);
    }
    let extended_features = __cpuid(CPUID_EXTENDED_FEATURES).edx;
    if extended_features & CPUID_FEATURE_NX == 0 {
        return Err(CpuError::NxUnsupported);
    }

    // SAFETY: RDMSR/WRMSR are valid at CPL0 and IA32_EFER is architectural on
    // every x86-64 processor. Setting NXE does not change segmentation or the
    // current long-mode state.
    unsafe {
        write_msr(IA32_EFER, read_msr(IA32_EFER) | EFER_NXE);
        write_cr0(read_cr0() | CR0_WP);
    }
    Ok(())
}

unsafe fn read_msr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: The caller guarantees CPL0 and a valid MSR index.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: The caller guarantees CPL0, a valid MSR index, and a value made
    // only of supported bits.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: Reading CR0 is valid at CPL0 and has no side effects.
    unsafe {
        asm!(
            "mov {value}, cr0",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

unsafe fn write_cr0(value: u64) {
    // SAFETY: The caller supplies a value that preserves the required paging
    // and protected-mode bits already set by firmware.
    unsafe {
        asm!(
            "mov cr0, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

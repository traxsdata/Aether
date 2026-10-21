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
const EFER_SCE: u64 = 1 << 0;

const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;
const IA32_FS_BASE: u32 = 0xc000_0100;

const CR0_WP: u64 = 1 << 16;
const CR4_OSXSAVE: u64 = 1 << 18;

const CPUID_EXTENDED_MAX: u32 = 0x8000_0000;
const CPUID_EXTENDED_FEATURES: u32 = 0x8000_0001;
const CPUID_FEATURE_NX: u32 = 1 << 20;
const CPUID_XSAVE: u32 = 1 << 26;
const CPUID_AVX: u32 = 1 << 28;

/// CPU features required by the kernel are unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuError {
    /// The processor does not implement the no-execute page protection.
    NxUnsupported,
}

/// Enables the architectural `syscall` instruction for the Linux ABI task.
///
/// # Safety
///
/// Must run once during boot at CPL0, with interrupts disabled and after the
/// GDT is installed. `entry` must be the Linux syscall assembly entry point and
/// must preserve the frame contract expected by the handler.
pub unsafe fn enable_syscall(entry: u64, kernel_code: u16, user_code: u16) {
    let star = (u64::from(user_code) << 48) | (u64::from(kernel_code) << 32);
    // SAFETY: The caller guarantees CPL0 and valid selectors/entry address.
    unsafe {
        write_msr(IA32_STAR, star);
        write_msr(IA32_LSTAR, entry);
        // Clear IF on entry; the assembly stub runs with interrupts masked.
        write_msr(IA32_FMASK, 1 << 9);
        write_msr(IA32_EFER, read_msr(IA32_EFER) | EFER_SCE);
    }
}

/// Sets the FS segment base used by Linux TLS.
///
/// # Safety
///
/// Must run at CPL0 with interrupts serialized against a context switch.
pub unsafe fn set_fs_base(base: u64) {
    // SAFETY: The caller guarantees CPL0 and serialized execution.
    unsafe { write_msr(IA32_FS_BASE, base) };
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

/// Enables the extended processor state used by SSE and AVX.
///
/// Returns `true` when the XSAVE area holds the upper halves of the YMM
/// registers as well, so a context switch can preserve all vector state with
/// `xsave64`/`xrstor64`. When the processor only implements FXSAVE the caller
/// falls back to the legacy 512-byte layout.
///
/// # Safety
///
/// Must run once at boot, at CPL0, with interrupts disabled and before any
/// user task executes vector code.
pub unsafe fn enable_extended_state() -> bool {
    let features = __cpuid(1).ecx;
    if features & (CPUID_XSAVE | CPUID_AVX) != (CPUID_XSAVE | CPUID_AVX) {
        return false;
    }
    // SAFETY: CR4 and XCR0 are architectural at CPL0. Setting OSXSAVE makes
    // XGETBV/XSETBV available; the enabled components (x87, SSE, AVX) are the
    // ones reported by CPUID leaf 1 and leaf 0xD on this processor.
    unsafe {
        write_cr4(read_cr4() | CR4_OSXSAVE);
        asm!(
            "xsetbv",
            in("ecx") 0u32,
            in("eax") 0b111u32,
            in("edx") 0u32,
            options(nomem, nostack, preserves_flags)
        );
    }
    true
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

unsafe fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: Reading CR4 is valid at CPL0 and has no side effects.
    unsafe {
        asm!(
            "mov {value}, cr4",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

unsafe fn write_cr4(value: u64) {
    // SAFETY: The caller supplies a value that preserves the required paging
    // and protected-mode bits already set by firmware.
    unsafe {
        asm!(
            "mov cr4, {value}",
            value = in(reg) value,
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

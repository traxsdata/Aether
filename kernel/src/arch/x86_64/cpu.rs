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
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, Ordering};

const IA32_EFER: u32 = 0xc000_0080;
const EFER_NXE: u64 = 1 << 11;
const EFER_SCE: u64 = 1 << 0;

const IA32_STAR: u32 = 0xc000_0081;
const IA32_LSTAR: u32 = 0xc000_0082;
const IA32_FMASK: u32 = 0xc000_0084;
/// RFLAGS bits cleared on `syscall` entry: TF, IF, DF, IOPL, NT, and AC.
const SYSCALL_FLAG_MASK: u64 = (1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 18);
const IA32_FS_BASE: u32 = 0xc000_0100;

const CR0_WP: u64 = 1 << 16;
const CR4_OSXSAVE: u64 = 1 << 18;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;

const CPUID_STRUCTURED_FEATURES: u32 = 7;
const CPUID_SMEP: u32 = 1 << 7;
const CPUID_SMAP: u32 = 1 << 20;

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
        // Clear IF so the stub runs with interrupts masked, and clear every
        // flag a guest could use against ring 0: TF (a #DB before the stack
        // switch), DF (backwards string copies), IOPL, NT (an `iretq` fault),
        // and AC. The user values return through R11 on `iretq`.
        write_msr(IA32_FMASK, SYSCALL_FLAG_MASK);
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

/// Supervisor protections enabled by [`enable_supervisor_protection`].
#[derive(Clone, Copy, Debug)]
pub struct SupervisorProtection {
    /// Ring 0 cannot execute user pages.
    pub smep: bool,
    /// Ring 0 cannot read or write user pages outside `stac`/`clac`.
    pub smap: bool,
}

/// Enables SMEP and SMAP when the processor implements them.
///
/// With SMAP on, the kernel can touch user memory only inside the user-copy
/// routine, which brackets its single copy with `stac`/`clac`. Any other
/// access faults at a RIP outside the exception table and halts, so a missed
/// conversion shows up in testing instead of working by accident.
///
/// # Safety
///
/// Must run once during boot, at CPL0, with interrupts disabled, before any
/// user task runs.
pub unsafe fn enable_supervisor_protection() -> SupervisorProtection {
    let mut protection = SupervisorProtection {
        smep: false,
        smap: false,
    };
    if __cpuid(0).eax < CPUID_STRUCTURED_FEATURES {
        return protection;
    }
    let features = __cpuid_count(CPUID_STRUCTURED_FEATURES, 0).ebx;
    protection.smep = features & CPUID_SMEP != 0;
    protection.smap = features & CPUID_SMAP != 0;
    let mut bits = 0;
    if protection.smep {
        bits |= CR4_SMEP;
    }
    if protection.smap {
        bits |= CR4_SMAP;
    }
    // SAFETY: CR4 is writable at CPL0, CPUID reported both bits, and no code
    // that executes or accesses user pages outside the copy routine runs yet.
    unsafe { write_cr4(read_cr4() | bits) };
    protection
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

const IA32_PAT: u32 = 0x277;
const CPUID_PAT: u32 = 1 << 16;
/// PAT entry 1, selected by `PWT=1, PCD=0, PAT=0`.
const PAT_ENTRY_WRITE_THROUGH: u32 = 1;
const PAT_WRITE_COMBINING: u64 = 0x01;

static WRITE_COMBINING: AtomicBool = AtomicBool::new(false);

/// Repurposes PAT entry 1 (power-on default: write-through) as
/// write-combining and returns `true` when the processor supports PAT.
///
/// No kernel mapping sets `WRITE_THROUGH` on its own, so after this call the
/// flag selects write-combining. The framebuffer uses it: the compositor only
/// streams whole rows into it, and an uncached aperture turns every 4-byte
/// store into its own bus transaction.
///
/// # Safety
///
/// Must run once during boot, at CPL0, with interrupts disabled and before
/// the kernel activates page tables that map anything with `WRITE_THROUGH`.
pub unsafe fn enable_write_combining() -> bool {
    if __cpuid(1).edx & CPUID_PAT == 0 {
        return false;
    }
    let shift = PAT_ENTRY_WRITE_THROUGH * 8;
    // SAFETY: IA32_PAT is architectural when CPUID reports PAT. Only entry 1
    // changes, and the caller guarantees no live mapping selects it yet.
    unsafe {
        let pat = read_msr(IA32_PAT);
        write_msr(
            IA32_PAT,
            (pat & !(0xff << shift)) | (PAT_WRITE_COMBINING << shift),
        );
        // A PAT change does not invalidate cached translations of the firmware
        // tables still in CR3; reload it so no stale memory type survives.
        write_cr3(read_cr3());
    }
    WRITE_COMBINING.store(true, Ordering::Relaxed);
    true
}

/// Returns `true` once [`enable_write_combining`] reprogrammed the PAT.
#[must_use]
pub fn write_combining_enabled() -> bool {
    WRITE_COMBINING.load(Ordering::Relaxed)
}

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_BASE_BSP: u64 = 1 << 8;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const CPUID_X2APIC: u32 = 1 << 21;
const X2APIC_ICR: u32 = 0x830;
const APIC_BASE_ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const XAPIC_ICR_LOW: u64 = 0x300;
const XAPIC_ICR_HIGH: u64 = 0x310;
const XAPIC_DELIVERY_PENDING: u32 = 1 << 12;
/// INIT, level assert, destination shorthand "all excluding self".
const ICR_INIT_ALL_EXCLUDING_SELF: u64 = (0b101 << 8) | (1 << 14) | (0b11 << 18);

/// How the bootstrap processor dealt with firmware-owned application
/// processors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApParking {
    /// An INIT IPI moved every other processor into wait-for-SIPI.
    Parked,
    /// The local APIC is disabled or this is not the bootstrap processor.
    NoApic,
    /// The processor has no x2APIC and the current page tables do not
    /// identity-map the xAPIC registers.
    Unsupported,
}

/// Parks the application processors the firmware left running.
///
/// The kernel is single-core. After `ExitBootServices` the firmware keeps the
/// other processors in its own idle loop, which may be a `monitor`/`mwait`
/// spin: on QEMU TCG with `-smp 2` that loop costs a full host core forever.
/// An INIT IPI puts each one in wait-for-SIPI, where it executes nothing until
/// a future SMP bring-up sends a startup IPI.
///
/// The APIC stays in the mode the firmware left it in, so legacy PIC delivery
/// through LINT0 is unchanged. An xAPIC is programmed through its MMIO page,
/// which is used only after a walk of the active page tables proves it is
/// identity-mapped; otherwise an x2APIC's MSR interface is used.
///
/// # Safety
///
/// Must run once during boot on the bootstrap processor, at CPL0, with
/// interrupts disabled, while the firmware's identity-mapped page tables are
/// still active, and before any code relies on another processor.
pub unsafe fn park_application_processors() -> ApParking {
    // SAFETY: IA32_APIC_BASE is architectural on every x86-64 processor.
    let base = unsafe { read_msr(IA32_APIC_BASE) };
    if base & APIC_BASE_ENABLE == 0 || base & APIC_BASE_BSP == 0 {
        return ApParking::NoApic;
    }
    if base & APIC_BASE_X2APIC != 0 {
        // SAFETY: x2APIC mode is active. The IPI excludes this processor, and
        // the kernel has not handed any work to another processor.
        unsafe { write_msr(X2APIC_ICR, ICR_INIT_ALL_EXCLUDING_SELF) };
        return ApParking::Parked;
    }
    let registers = base & APIC_BASE_ADDRESS_MASK;
    // SAFETY: The caller guarantees the firmware's page tables are active and
    // identity-map their own table frames.
    if unsafe { identity_mapped(registers) } {
        // SAFETY: The walk proved the xAPIC page is mapped at its physical
        // address. ICR writes are architectural; the shorthand excludes this
        // processor. The delivery-status wait is bounded.
        unsafe {
            let icr_low = (registers + XAPIC_ICR_LOW) as *mut u32;
            ((registers + XAPIC_ICR_HIGH) as *mut u32).write_volatile(0);
            icr_low.write_volatile(ICR_INIT_ALL_EXCLUDING_SELF as u32);
            for _ in 0..100_000 {
                if icr_low.read_volatile() & XAPIC_DELIVERY_PENDING == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
        }
        return ApParking::Parked;
    }
    if __cpuid(1).ecx & CPUID_X2APIC == 0 {
        return ApParking::Unsupported;
    }
    // SAFETY: CPUID reports x2APIC and the APIC is globally enabled, so the
    // xAPIC -> x2APIC transition is permitted and preserves the LVT entries.
    // The IPI excludes this processor.
    unsafe {
        write_msr(IA32_APIC_BASE, base | APIC_BASE_X2APIC);
        write_msr(X2APIC_ICR, ICR_INIT_ALL_EXCLUDING_SELF);
    }
    ApParking::Parked
}

/// Returns `true` when the active page tables map the 4 KiB page at
/// `address` to the same physical address, present and writable.
///
/// # Safety
///
/// The active page tables must identity-map their own table frames, as the
/// UEFI hand-off tables do.
unsafe fn identity_mapped(address: u64) -> bool {
    const PRESENT: u64 = 1 << 0;
    const WRITABLE: u64 = 1 << 1;
    const HUGE: u64 = 1 << 7;
    const FRAME: u64 = 0x000f_ffff_ffff_f000;
    const CR4_LA57: u64 = 1 << 12;
    // SAFETY: Reading CR4 is valid at CPL0. The walk below is four-level.
    if unsafe { read_cr4() } & CR4_LA57 != 0 {
        return false;
    }
    // SAFETY: Reading CR3 is valid at CPL0.
    let mut table = unsafe { read_cr3() } & FRAME;
    for level in (0..4u32).rev() {
        let shift = 12 + 9 * level;
        let index = (address >> shift) & 0x1ff;
        // SAFETY: The caller guarantees table frames are identity-mapped, and
        // `index` is within the 512-entry table.
        let entry = unsafe { ((table + index * 8) as *const u64).read_volatile() };
        if entry & (PRESENT | WRITABLE) != (PRESENT | WRITABLE) {
            return false;
        }
        let size = 1u64 << shift;
        if level == 0 || (entry & HUGE != 0 && (level == 1 || level == 2)) {
            // A huge leaf's PAT bit sits at bit 12; mask it with the offset.
            let frame = entry & FRAME & !(size - 1);
            return frame == address & !(size - 1);
        }
        table = entry & FRAME;
    }
    false
}

unsafe fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: Reading CR3 is valid at CPL0 and has no side effects.
    unsafe {
        asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn write_cr3(value: u64) {
    // SAFETY: The caller reloads the current, valid top-level table.
    unsafe { asm!("mov cr3, {}", in(reg) value, options(nostack, preserves_flags)) };
}

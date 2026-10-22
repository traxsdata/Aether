// SPDX-License-Identifier: Apache-2.0
//! Legacy PIC and PIT setup for the Phase 2 scheduler tick.
//!
//! Aether will use the local APIC and a high-resolution timer later. The 8259
//! PIC and 8254 PIT are sufficient for the first preemptive scheduler and keep
//! the bring-up path small.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xa0;
const PIC2_DATA: u16 = 0xa1;
const PIT_CHANNEL0: u16 = 0x40;
const PIT_COMMAND: u16 = 0x43;

/// Frequency of the scheduler timer in hertz.
pub const TIMER_HZ: u32 = 100;

const PIT_BASE_HZ: u32 = 1_193_182;

/// Time-stamp counter value when the periodic timer was programmed.
static TSC_AT_INIT: AtomicU64 = AtomicU64::new(0);

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller guarantees the port is valid.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

#[inline]
unsafe fn io_wait() {
    // Writing to the unused POST port is the conventional PIC settle delay.
    // SAFETY: Port 0x80 is reserved for this purpose on the supported platform.
    unsafe { outb(0x80, 0) };
}

/// Remaps the PIC and starts the periodic timer.
pub fn init() {
    // SAFETY: Fixed legacy PIC/PIT ports on the supported platform. Interrupts
    // are disabled until the scheduler is ready.
    unsafe {
        // ICW1: begin initialization, cascade, edge-triggered.
        outb(PIC1_COMMAND, 0x11);
        io_wait();
        outb(PIC2_COMMAND, 0x11);
        io_wait();
        // ICW2: remap IRQ0-7 to vectors 32-39 and IRQ8-15 to 40-47.
        outb(PIC1_DATA, 0x20);
        io_wait();
        outb(PIC2_DATA, 0x28);
        io_wait();
        // ICW3: slave on IRQ2.
        outb(PIC1_DATA, 0x04);
        io_wait();
        outb(PIC2_DATA, 0x02);
        io_wait();
        // ICW4: 8086 mode.
        outb(PIC1_DATA, 0x01);
        io_wait();
        outb(PIC2_DATA, 0x01);
        io_wait();

        // Mask everything except IRQ0 (timer), IRQ1 (keyboard), and the IRQ2
        // cascade on the master. Unmask IRQ12 (auxiliary PS/2 mouse) on the
        // slave.
        outb(PIC1_DATA, 0xf8);
        outb(PIC2_DATA, 0xef);

        // PIT channel 0, mode 2 (rate generator), binary.
        outb(PIT_COMMAND, 0x34);
        let divisor = (PIT_BASE_HZ / TIMER_HZ) as u16;
        outb(PIT_CHANNEL0, (divisor & 0xff) as u8);
        outb(PIT_CHANNEL0, (divisor >> 8) as u8);
    }
    TSC_AT_INIT.store(tsc(), Ordering::Relaxed);
}

/// Reads the time-stamp counter.
#[must_use]
pub fn tsc() -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: RDTSC is unprivileged, has no memory effects, and is available
    // on every x86-64 processor.
    unsafe {
        asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

/// Converts a TSC interval to microseconds.
///
/// The TSC rate is calibrated against the PIT: `ticks` timer interrupts have
/// elapsed since [`init`]. The first second is too coarse to calibrate, so it
/// reports zero rather than a wrong figure. This is for diagnostics only.
#[must_use]
pub fn tsc_micros(delta: u64, ticks: u64) -> u64 {
    let elapsed = tsc().saturating_sub(TSC_AT_INIT.load(Ordering::Relaxed));
    if ticks < u64::from(TIMER_HZ) || elapsed == 0 {
        return 0;
    }
    let micros = u128::from(delta) * u128::from(ticks) * (1_000_000 / u128::from(TIMER_HZ))
        / u128::from(elapsed);
    micros.min(u128::from(u64::MAX)) as u64
}

/// Acknowledges an IRQ by its remapped vector.
///
/// IRQ8-15 arrive through the slave controller, which must be acknowledged
/// before the master cascade line.
pub fn acknowledge_vector(vector: u64) {
    // SAFETY: Non-specific EOIs are correct for the PIC in this configuration.
    unsafe {
        if vector >= 40 {
            outb(PIC2_COMMAND, 0x20);
        }
        outb(PIC1_COMMAND, 0x20);
    }
}

/// Enables interrupts on the current CPU.
pub fn enable_interrupts() {
    // SAFETY: The caller must have installed a complete IDT and initialized
    // the scheduler state that the timer handler uses.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Disables interrupts on the current CPU.
pub fn disable_interrupts() {
    // SAFETY: Disabling interrupts is always valid at CPL 0.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Restores the interrupt flag saved by [`InterruptGuard`].
fn restore_interrupt_flag(flags: u64) {
    if flags & (1 << 9) != 0 {
        // SAFETY: The saved RFLAGS had IF set, so restoring it is valid.
        unsafe { asm!("sti", options(nomem, nostack)) };
    }
}

/// RAII guard that masks interrupts and restores the previous IF state.
pub struct InterruptGuard {
    flags: u64,
}

impl InterruptGuard {
    /// Enters a critical section.
    #[must_use]
    pub fn enter() -> Self {
        let flags: u64;
        // SAFETY: `pushfq`/`pop` read RFLAGS and `cli` masks interrupts at
        // CPL 0. The saved value is restored on drop.
        unsafe {
            asm!(
                "pushfq",
                "pop {flags}",
                "cli",
                flags = out(reg) flags,
            );
        }
        Self { flags }
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        restore_interrupt_flag(self.flags);
    }
}

/// Waits for the next interrupt with interrupts enabled.
pub fn halt_until_interrupt() {
    // SAFETY: `hlt` with interrupts disabled would deadlock; callers only use
    // this after interrupts have been enabled.
    unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
}

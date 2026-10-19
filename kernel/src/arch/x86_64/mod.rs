// SPDX-License-Identifier: Apache-2.0
//! x86-64 architecture support.

pub mod cpu;
pub mod gdt;
pub mod idt;
pub mod serial;
pub mod timer;

/// Installs the kernel's own descriptor and interrupt tables.
pub fn init() {
    gdt::init();
    idt::init();
}

/// Stops the CPU until an interrupt arrives.
#[inline]
pub fn halt() -> ! {
    loop {
        // SAFETY: Halting is safe once the kernel has nothing left to run.
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack));
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
//! x86-64 architecture support.

pub mod serial;

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

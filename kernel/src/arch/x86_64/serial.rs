// SPDX-License-Identifier: Apache-2.0
//! COM1 serial output for early kernel diagnostics.

use core::fmt::{self, Write};

const COM1: u16 = 0x3f8;
/// Spin budget for one console byte.
///
/// The console must not drop output while the guest is busy: a dropped byte
/// can hide a kernel marker. The budget is large enough to outlast a burst
/// of guest output and small enough that a machine without a working UART
/// still makes progress.
const MAX_SPIN: u32 = 20_000_000;

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller guarantees the port is valid.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Caller guarantees the port is valid.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Initializes COM1 at 115200 8N1.
pub fn init() {
    // SAFETY: Writes are limited to the conventional COM1 UART.
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x01);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xc7);
        outb(COM1 + 4, 0x0b);
    }
}

/// Reads one byte if the UART has data ready.
///
/// Non-blocking: the idle loop polls this while the shell is active.
#[must_use]
pub fn try_read() -> Option<u8> {
    // SAFETY: COM1 is valid on the supported platform.
    unsafe {
        if inb(COM1 + 5) & 1 == 0 {
            None
        } else {
            Some(inb(COM1))
        }
    }
}

/// Writes one byte, dropping it if the UART is not ready.
pub fn write_byte(byte: u8) {
    let mut spins = 0u32;
    // SAFETY: COM1 is valid on the supported platform.
    unsafe {
        while spins < MAX_SPIN && inb(COM1 + 5) & 0x20 == 0 {
            core::hint::spin_loop();
            spins += 1;
        }
        if spins < MAX_SPIN {
            outb(COM1, byte);
        }
    }
}

/// Writes a string with LF converted to CRLF.
pub fn write_str(value: &str) {
    write_bytes(value.as_bytes());
}

/// Writes bytes with LF converted to CRLF.
///
/// This is the only console write path used by user output, so a native
/// application cannot smuggle terminal control sequences through a different
/// newline convention.
pub fn write_bytes(value: &[u8]) {
    for byte in value {
        if *byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(*byte);
    }
}

/// `core::fmt::Write` adapter.
pub struct Serial;

impl Write for Serial {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        write_str(value);
        Ok(())
    }
}

/// Prints a formatted line to serial.
#[macro_export]
macro_rules! serial_line {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::arch::x86_64::serial::Serial, $($arg)*);
    }};
}

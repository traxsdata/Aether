// SPDX-License-Identifier: Apache-2.0
//! Minimal COM1 serial output.
//!
//! Serial output is the primary diagnostic channel for both the loader and the
//! kernel. It is initialized identically on both sides so a hand-off failure is
//! still visible.

use core::fmt::{self, Write};

const COM1: u16 = 0x3f8;
const MAX_SPIN: u32 = 100_000;

#[inline]
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: Caller guarantees the port is valid for this platform.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: Caller guarantees the port is valid for this platform.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Initializes COM1 at 115200 baud, 8 data bits, no parity, one stop bit.
pub fn init() {
    // SAFETY: COM1 is the conventional x86 serial port and these writes only
    // affect the UART.
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable divisor latch
        outb(COM1, 0x01); // divisor low: 115200
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8N1
        outb(COM1 + 2, 0xc7); // enable and clear FIFOs
        outb(COM1 + 4, 0x0b); // DTR, RTS, OUT2
    }
}

/// Writes one byte, dropping it if the UART never becomes ready.
pub fn write_byte(byte: u8) {
    let mut spins = 0u32;
    // SAFETY: COM1 is a valid UART on the supported platform.
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

/// Writes a string.
pub fn write_str(value: &str) {
    for byte in value.bytes() {
        if byte == b'\n' {
            write_byte(b'\r');
        }
        write_byte(byte);
    }
}

/// A `core::fmt::Write` adapter for serial output.
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
        let _ = writeln!($crate::serial::Serial, $($arg)*);
    }};
}

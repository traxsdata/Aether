// SPDX-License-Identifier: Apache-2.0
//! Bounded three-byte PS/2 mouse decoding.

/// A complete relative mouse packet. Positive Y is upward, as on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MousePacket {
    /// Signed nine-bit horizontal delta, or zero on axis overflow.
    pub dx: i16,
    /// Signed nine-bit vertical delta, or zero on axis overflow.
    pub dy: i16,
    /// Primary, secondary, and middle buttons in bits 0, 1, and 2.
    pub buttons: u8,
}

/// Decoder for the standard mouse protocol selected by the driver's F6 command.
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseDecoder {
    packet: [u8; 3],
    index: usize,
}

impl MouseDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            packet: [0; 3],
            index: 0,
        }
    }

    /// Discards a partial packet after a controller parity/timeout error.
    pub fn reset(&mut self) {
        self.index = 0;
    }

    /// Consumes a byte, checking the fixed synchronization bit at boundaries.
    pub fn push(&mut self, byte: u8) -> Option<MousePacket> {
        if self.index == 0 && byte & 0x08 == 0 {
            return None;
        }
        self.packet[self.index] = byte;
        self.index += 1;
        if self.index < 3 {
            return None;
        }
        self.index = 0;
        let flags = self.packet[0];
        let axis = |value: u8, sign: u8, overflow: u8| -> i16 {
            if flags & overflow != 0 {
                0
            } else {
                i16::from(value) - if flags & sign != 0 { 256 } else { 0 }
            }
        };
        Some(MousePacket {
            dx: axis(self.packet[1], 0x10, 0x40),
            dy: axis(self.packet[2], 0x20, 0x80),
            buttons: flags & 0x07,
        })
    }
}

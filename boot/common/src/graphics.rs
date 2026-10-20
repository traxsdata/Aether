// SPDX-License-Identifier: Apache-2.0
//! UEFI GOP mode layout and validated linear-framebuffer hand-off.
//!
//! Keep the firmware layout host-testable: a wrong field offset can otherwise
//! appear to work on a tightly packed QEMU surface and corrupt a padded one.

use aether_abi::FramebufferInfo;

/// `EFI_PIXEL_BITMASK`: one set of channel masks, not an array of masks.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PixelBitmask {
    /// Red channel mask.
    pub red_mask: u32,
    /// Green channel mask.
    pub green_mask: u32,
    /// Blue channel mask.
    pub blue_mask: u32,
    /// Reserved channel mask.
    pub reserved_mask: u32,
}

/// `EFI_GRAPHICS_OUTPUT_MODE_INFORMATION` from the public UEFI specification.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GraphicsOutputModeInformation {
    /// Structure version; currently zero.
    pub version: u32,
    /// Visible width.
    pub horizontal_resolution: u32,
    /// Visible height.
    pub vertical_resolution: u32,
    /// GOP pixel format (0 = RGBX, 1 = BGRX).
    pub pixel_format: u32,
    /// The single inline set of masks used by bitmask modes.
    pub pixel_information: PixelBitmask,
    /// Actual scan-line stride, in pixels, including padding.
    pub pixels_per_scan_line: u32,
}

impl GraphicsOutputModeInformation {
    /// Builds a hand-off only when the *reported* aperture covers every row.
    /// Never invent a larger aperture or substitute width for an invalid stride.
    #[must_use]
    pub fn framebuffer(&self, base: u64, size: u64) -> Option<FramebufferInfo> {
        if self.version != 0 || base == 0 {
            return None;
        }
        let info = FramebufferInfo {
            base,
            size,
            width: self.horizontal_resolution,
            height: self.vertical_resolution,
            stride: self.pixels_per_scan_line,
            format: self.pixel_format,
            reserved: 0,
        };
        info.validate().ok()?;
        Some(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    fn mode() -> GraphicsOutputModeInformation {
        GraphicsOutputModeInformation {
            version: 0,
            horizontal_resolution: 800,
            vertical_resolution: 600,
            pixel_format: 1,
            pixel_information: PixelBitmask::default(),
            pixels_per_scan_line: 832,
        }
    }

    #[test]
    fn gop_layout_matches_uefi() {
        assert_eq!(size_of::<PixelBitmask>(), 16);
        assert_eq!(size_of::<GraphicsOutputModeInformation>(), 36);
        assert_eq!(
            offset_of!(GraphicsOutputModeInformation, pixel_information),
            16
        );
        assert_eq!(
            offset_of!(GraphicsOutputModeInformation, pixels_per_scan_line),
            32
        );
    }

    #[test]
    fn padded_stride_and_reported_size_are_preserved() {
        let info = mode().framebuffer(0x8000_0000, 832 * 600 * 4).unwrap();
        assert_eq!(info.width, 800);
        assert_eq!(info.stride, 832);
        assert_eq!(info.size, 832 * 600 * 4);
    }

    #[test]
    fn rejects_short_apertures_and_invalid_strides() {
        assert!(mode().framebuffer(0x8000_0000, 800 * 600 * 4).is_none());
        let mut mode = mode();
        mode.pixels_per_scan_line = 799;
        assert!(mode.framebuffer(0x8000_0000, 832 * 600 * 4).is_none());
    }

    #[test]
    fn rejects_unsupported_modes_and_bad_addresses() {
        for format in [2, 3, u32::MAX] {
            let mut mode = mode();
            mode.pixel_format = format;
            assert!(mode.framebuffer(0x8000_0000, 832 * 600 * 4).is_none());
        }
        assert!(mode().framebuffer(0, 832 * 600 * 4).is_none());
        assert!(mode().framebuffer(0x8000_0001, 832 * 600 * 4).is_none());
        assert!(mode().framebuffer(u64::MAX - 3, 832 * 600 * 4).is_none());
        let mut mode = mode();
        mode.version = 1;
        assert!(mode.framebuffer(0x8000_0000, 832 * 600 * 4).is_none());
    }
}

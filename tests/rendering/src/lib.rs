// SPDX-License-Identifier: Apache-2.0
//! Host tests compile the actual kernel drawing implementation, not a mock.
//!
//! Only serial output is replaced; the tested surfaces are ordinary bounded
//! allocations with guard/padding words instead of mapped hardware apertures.

#![cfg(test)]

mod logging {
    #[macro_export]
    macro_rules! serial_line {
        ($($argument:tt)*) => { let _ = core::format_args!($($argument)*); };
    }
}

// The kernel binary remains no_std and target-specific. Include its graphics
// module in this dedicated host harness so raw surface operations are tested.
#[path = "../../../kernel/src/graphics.rs"]
#[allow(dead_code)]
mod graphics;

use aether_abi::{FramebufferInfo, PixelFormat};

fn surface(format: PixelFormat, width: u32, height: u32, stride: u32) -> Vec<u32> {
    let mut pixels = vec![0xaabb_ccdd; (stride * height) as usize + 16];
    let info = FramebufferInfo {
        base: pixels.as_mut_ptr() as u64,
        size: u64::from(stride) * u64::from(height) * 4,
        width,
        height,
        stride,
        format: format as u32,
        reserved: 0,
    };
    assert_eq!(graphics::init(&info, true), Ok(true));
    graphics::clear_clip();
    graphics::clear_round_clip();
    graphics::clear_damage();
    graphics::fill_rect(0, 0, width as i32, height as i32, 0);
    pixels
}

#[test]
fn software_surface_regressions() {
    // One serial test owns the renderer's boot-time globals. Each allocation
    // stays alive until the next init replaces the corresponding descriptor.
    for format in [
        PixelFormat::RedGreenBlueReserved,
        PixelFormat::BlueGreenRedReserved,
    ] {
        let mut pixels = surface(format, 64, 48, 72);
        graphics::fill_rect(-10, -10, 20, 20, 0x123456);
        graphics::present();
        assert_eq!(pixels[0], format.encode_rgb(0x123456).unwrap());
        assert_eq!(pixels[9 * 72 + 9], pixels[0]);
        assert_eq!(pixels[10], 0);
        for row in 0..48 {
            assert_eq!(&pixels[row * 72 + 64..row * 72 + 72], &[0xaabb_ccdd; 8]);
        }
        assert_eq!(&pixels[72 * 48..], &[0xaabb_ccdd; 16]);

        graphics::fill_rect(0, 0, 64, 48, 0);
        graphics::set_damage(10, 10, 4, 4);
        graphics::set_clip(0, 0, 64, 48);
        graphics::fill_rect(0, 0, 64, 48, 0xff0000);
        graphics::clear_clip();
        // Clearing a window's inner clip must not widen the damage clip.
        graphics::fill_rect(0, 0, 8, 8, 0x00ff00);
        graphics::clear_damage();
        graphics::present();
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[10 * 72 + 10], format.encode_rgb(0xff0000).unwrap());
        assert_eq!(pixels[14 * 72 + 14], 0);

        graphics::fill_rect(0, 0, 64, 48, 0);
        graphics::set_round_clip(5, 5, 40, 32, 8);
        graphics::set_clip(6, 18, 38, 18);
        graphics::fill_rect(0, 0, 64, 48, 0xffffff);
        graphics::clear_clip();
        graphics::clear_round_clip();
        graphics::present();
        assert_eq!(pixels[35 * 72 + 6], 0);
        assert_eq!(format.decode_rgb(pixels[35 * 72 + 20]), Some(0xffffff));
        // Rounded clipping partly off-screen may yield empty row spans.
        graphics::set_round_clip(62, 40, 40, 32, 8);
        graphics::fill_rect(0, 0, 64, 48, 0xffffff);
        graphics::clear_round_clip();

        graphics::fill_rect(0, 0, 64, 48, 0x204060);
        graphics::cache_wallpaper();
        graphics::blend_rect(0, 0, 10, 10, 0x806040, 128);
        graphics::present();
        assert_eq!(format.decode_rgb(pixels[0]), Some(0x50504f));
        assert!(graphics::restore_wallpaper(0, 0, 5, 5));
        graphics::present_region(0, 0, 5, 5);
        assert_eq!(format.decode_rgb(pixels[0]), Some(0x204060));
        assert_eq!(format.decode_rgb(pixels[6 * 72 + 6]), Some(0x50504f));

        graphics::fill_rect(0, 0, 64, 48, 0x112233);
        graphics::save_cursor_background(2, 2);
        graphics::fill_rect(
            2,
            2,
            graphics::CURSOR_WIDTH,
            graphics::CURSOR_HEIGHT,
            0xffffff,
        );
        assert_eq!(graphics::restore_cursor_background(), Some((2, 2, 15, 22)));
        graphics::save_cursor_background(62, 46);
        graphics::fill_rect(62, 46, 15, 22, 0xff0000);
        assert_eq!(graphics::restore_cursor_background(), Some((62, 46, 2, 2)));
        graphics::present();
        assert_eq!(format.decode_rgb(pixels[2 * 72 + 2]), Some(0x112233));
        assert_eq!(format.decode_rgb(pixels[47 * 72 + 63]), Some(0x112233));

        // A region present must not clobber unrelated front-buffer pixels.
        pixels[20] = 0xdead_beef;
        graphics::present_region(0, 0, 1, 1);
        assert_eq!(pixels[20], 0xdead_beef);
        graphics::fill_rect(i32::MAX - 1, 0, i32::MAX, 1, 0xffffff);
        graphics::fill_rect(i32::MIN, i32::MIN, 20, 20, 0xffffff);
        graphics::present();
        assert_eq!(&pixels[72 * 48..], &[0xaabb_ccdd; 16]);

        // The two-lane blend must match per-channel truncating division for
        // every alpha, including both channel extremes.
        let reference = |existing: u32, rgb: u32, alpha: u32| -> u32 {
            let channel = |shift: u32| {
                (((existing >> shift) & 0xff) * (255 - alpha) + ((rgb >> shift) & 0xff) * alpha)
                    / 255
            };
            (channel(16) << 16) | (channel(8) << 8) | channel(0)
        };
        let pairs = [
            (0x000000, 0xffffff),
            (0xffffff, 0x000000),
            (0xffffff, 0xffffff),
            (0x204060, 0x806040),
            (0x01fe7f, 0xfe0180),
        ];
        for alpha in 1..=254u32 {
            for (existing, rgb) in pairs {
                graphics::fill_rect(0, 0, 1, 1, existing);
                graphics::blend_rect(0, 0, 1, 1, rgb, alpha as u8);
                graphics::present_region(0, 0, 1, 1);
                assert_eq!(
                    format.decode_rgb(pixels[0]),
                    Some(reference(existing, rgb, alpha)),
                    "alpha={alpha} existing={existing:06x} rgb={rgb:06x}"
                );
            }
        }

        // Blending around a hole is the full rounded blend minus the hole.
        let snapshot = |pixels: &[u32]| pixels[..72 * 48].to_vec();
        graphics::fill_rect(0, 0, 64, 48, 0x203040);
        graphics::blend_round_rect(4, 3, 50, 40, 9, 0x000000, 78);
        graphics::present();
        let full = snapshot(&pixels);
        graphics::fill_rect(0, 0, 64, 48, 0x203040);
        let hole = (6, 12, 46, 22);
        graphics::blend_round_rect_around(4, 3, 50, 40, 9, 0x000000, 78, hole);
        graphics::present();
        let around = snapshot(&pixels);
        let base = format.encode_rgb(0x203040).unwrap();
        for row in 0..48i32 {
            for column in 0..64i32 {
                let index = (row * 72 + column) as usize;
                let inside = column >= hole.0
                    && column < hole.0 + hole.2
                    && row >= hole.1
                    && row < hole.1 + hole.3;
                let expected = if inside { base } else { full[index] };
                assert_eq!(around[index], expected, "pixel {column},{row}");
            }
        }

        // Span-based glyphs match drawing every lit cell as its own clipped
        // rectangle, including under rounded, damage, and content clips.
        let text = "Ag#9 Wm.";
        type Bounds = Option<(i32, i32, i32, i32)>;
        let cases: [(i32, i32, i32, Bounds, Bounds); 5] = [
            (2, 3, 1, None, None),
            (-3, -2, 2, None, None),
            (1, 1, 3, Some((3, 2, 40, 30)), None),
            (0, 4, 2, None, Some((4, 6, 21, 13))),
            (5, 0, 3, Some((2, 1, 30, 40)), Some((9, 3, 50, 9))),
        ];
        for (x, y, scale, round, clip) in cases {
            let apply = || {
                graphics::fill_rect(0, 0, 64, 48, 0x102030);
                graphics::set_damage(1, 1, 60, 45);
                if let Some((cx, cy, cw, ch)) = round {
                    graphics::set_round_clip(cx, cy, cw, ch, 9);
                }
                if let Some((cx, cy, cw, ch)) = clip {
                    graphics::set_clip(cx, cy, cw, ch);
                }
            };
            let reset = || {
                graphics::clear_clip();
                graphics::clear_round_clip();
                graphics::clear_damage();
                graphics::present();
            };
            apply();
            graphics::draw_text(x, y, text, 0xf0e0d0, scale);
            reset();
            let spans = snapshot(&pixels);
            apply();
            let mut cursor = x;
            for character in text.chars() {
                let rows = graphics::glyph(character.to_ascii_uppercase());
                for (row, bits) in rows.iter().enumerate() {
                    for column in 0..5 {
                        if bits & (1 << (4 - column)) != 0 {
                            graphics::fill_rect(
                                cursor + column * scale,
                                y + row as i32 * scale,
                                scale,
                                scale,
                                0xf0e0d0,
                            );
                        }
                    }
                }
                cursor += 6 * scale;
            }
            reset();
            assert_eq!(spans, snapshot(&pixels), "glyph case {x},{y} scale {scale}");
        }

        let direct = surface(format, 1921, 1, 1928);
        graphics::fill_rect(0, 0, 1921, 1, 0xff0000);
        assert_eq!(direct[0], format.encode_rgb(0xff0000).unwrap());
        assert_eq!(direct[1920], direct[0]);
        assert_eq!(&direct[1921..], &[0xaabb_ccdd; 23]);
        assert!(!graphics::restore_wallpaper(0, 0, 1, 1));
        // Direct (volatile) painting uses the same blend.
        graphics::blend_rect(0, 0, 2, 1, 0x806040, 128);
        assert_eq!(
            format.decode_rgb(direct[1]),
            Some(reference(0xff0000, 0x806040, 128))
        );
        assert_eq!(direct[2], format.encode_rgb(0xff0000).unwrap());
    }
}

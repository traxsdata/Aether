// SPDX-License-Identifier: Apache-2.0
//! Original vector artwork for the desktop.
//!
//! Every icon is drawn from primitives in this repository. No bitmap, font, or
//! icon from another operating system is embedded or traced. The shapes use the
//! project's own "layered glass" language: a tinted squircle, a bright top
//! highlight, and a simple geometric glyph.

use crate::graphics;
use aether_wm::{Palette, WindowKind};

/// Draws the Aether four-point compass mark.
pub fn aether_mark(center_x: i32, center_y: i32, radius: i32, color: u32) {
    if radius <= 0 {
        return;
    }
    let inner = (radius / 3).max(1);
    let points = [
        (center_x, center_y - radius),
        (center_x + inner, center_y - inner),
        (center_x + radius, center_y),
        (center_x + inner, center_y + inner),
        (center_x, center_y + radius),
        (center_x - inner, center_y + inner),
        (center_x - radius, center_y),
        (center_x - inner, center_y - inner),
    ];
    graphics::fill_polygon(&points, color);
    graphics::fill_circle(center_x, center_y, (radius / 5).max(1), 0xfff3d6);
}

/// Draws one Craft app tile for the Applications launcher.
///
/// The palette is deliberately varied so the grid reads at a glance without
/// shipping any upstream application artwork.
pub fn craft_tile(index: usize, rect: aether_wm::Rect, _palette: Palette) {
    const COLORS: [u32; 13] = [
        0x2c7080, 0x8a4b3a, 0x6d3b6d, 0x3a5b8a, 0x2d7048, 0x806a2d, 0x46566d, 0x754049, 0x3d4775,
        0x7a5b2b, 0x2d6d6d, 0x5b3f75, 0x4d6b2d,
    ];
    if rect.width < 18 || rect.height < 18 {
        return;
    }
    let background = COLORS[index % COLORS.len()];
    let radius = (rect.width / 5).max(6);
    graphics::fill_round_rect(rect.x, rect.y, rect.width, rect.height, radius, background);
    graphics::blend_round_rect(
        rect.x + 1,
        rect.y + 1,
        rect.width - 2,
        rect.height / 2,
        radius.saturating_sub(1),
        0xffffff,
        26,
    );
    graphics::blend_rect(
        rect.x + radius,
        rect.y + 1,
        rect.width - radius * 2,
        1,
        0xffffff,
        85,
    );
    let name = crate::linux::pack_entry_name(index).unwrap_or("?");
    let mut initials = [0u8; 2];
    let mut count = 0usize;
    for byte in name.bytes() {
        if count == 0 && byte.is_ascii_alphabetic() {
            initials[0] = byte.to_ascii_uppercase();
            count = 1;
        } else if count == 1 {
            initials[1] = byte.to_ascii_uppercase();
            break;
        }
    }
    let text = core::str::from_utf8(&initials[..count.max(1)]).unwrap_or("?");
    let scale = if rect.width >= 54 { 3 } else { 2 };
    let width = graphics::text_width(text, scale);
    graphics::draw_text(
        rect.center().x - width / 2,
        rect.center().y - 4 * scale,
        text,
        0xffffff,
        scale,
    );
}

/// Draws a small squared application tile with an original glyph.
pub fn app_icon(kind: WindowKind, rect: aether_wm::Rect, palette: Palette) {
    if rect.width < 16 || rect.height < 16 {
        return;
    }
    let high_contrast = palette.text == 0xffffff && palette.window == 0x0a0a0a;
    let (background, glyph) = if high_contrast {
        (0x000000, 0xffffff)
    } else {
        match kind {
            WindowKind::Files => (0x2c7080, 0xd7fbff),
            WindowKind::Security => (0x2d7048, 0xd9ffe6),
            WindowKind::Portal => (0x80522d, 0xffe8cc),
            WindowKind::Settings => (0x46566d, 0xe8eef9),
            WindowKind::Recovery => (0x754049, 0xffe1e6),
            WindowKind::Log => (0x3d4775, 0xe2e6ff),
            WindowKind::Welcome => (0x7a5b2b, 0xfff0c9),
            WindowKind::Linux => (0x333d54, 0xdce7ff),
            WindowKind::Linux2 => (0x2f4a63, 0xd8f2ff),
            WindowKind::Applications => (0x5b3f75, 0xf3e2ff),
        }
    };
    let radius = (rect.width / 5).max(6);
    graphics::fill_round_rect(rect.x, rect.y, rect.width, rect.height, radius, background);
    // Glass highlight: brighter top band and a specular edge.
    graphics::blend_round_rect(
        rect.x + 1,
        rect.y + 1,
        rect.width - 2,
        rect.height / 2,
        radius.saturating_sub(1),
        0xffffff,
        24,
    );
    graphics::blend_rect(
        rect.x + radius,
        rect.y + 1,
        rect.width - radius * 2,
        1,
        0xffffff,
        85,
    );
    graphics::blend_rect(
        rect.x + 1,
        rect.bottom() - 2,
        rect.width - 2,
        1,
        0x000000,
        55,
    );
    let padding = (rect.width / 5).max(5);
    let inner = aether_wm::Rect::new(
        rect.x + padding,
        rect.y + padding,
        rect.width - padding * 2,
        rect.height - padding * 2,
    );
    draw_glyph(kind, inner, glyph);
}

fn draw_glyph(kind: WindowKind, rect: aether_wm::Rect, color: u32) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let size = rect.width.min(rect.height);
    match kind {
        WindowKind::Files => folder(rect.x, rect.y + size / 5, size, color),
        WindowKind::Security => shield(
            rect.x + size / 8,
            rect.y + size / 10,
            size - size / 4,
            color,
        ),
        WindowKind::Portal => portal(rect.x, rect.y, size, color),
        WindowKind::Settings => gear(cx, cy, size / 3, color),
        WindowKind::Recovery => {
            graphics::stroke_circle(cx, cy, size / 3, color);
            graphics::fill_polygon(
                &[
                    (cx + size / 3, cy - 2),
                    (cx + size / 3 + 5, cy - size / 3),
                    (cx + size / 3 - 2, cy - size / 3 - 4),
                ],
                color,
            );
            graphics::fill_circle(cx, cy, size / 12, color);
        }
        WindowKind::Log => {
            for index in 0..3 {
                let y = rect.y + size / 5 + index * (size / 4);
                let width = size - if index == 1 { size / 5 } else { 0 };
                graphics::fill_rect(rect.x, y, width, 2, color);
                graphics::fill_circle(rect.x + size - 1, y + 1, 1, color);
            }
        }
        WindowKind::Welcome => aether_mark(cx, cy, size / 2, color),
        WindowKind::Linux | WindowKind::Linux2 => {
            // A small framed surface: compositor window with presented pixels.
            graphics::outline_round_rect(
                rect.x + size / 10,
                rect.y + size / 8,
                size - size / 5,
                size - size / 4,
                3,
                color,
            );
            graphics::fill_rect(rect.x + size / 4, rect.y + size / 2 - 1, size / 2, 2, color);
            graphics::fill_circle(rect.x + size / 2, rect.y + size / 2, size / 10, color);
        }
        WindowKind::Applications => {
            // A 3x3 launch grid with one highlighted tile.
            for row in 0..3 {
                for column in 0..3 {
                    let cell = size / 4;
                    let x = rect.x + column * cell + cell / 4;
                    let y = rect.y + row * cell + cell / 4;
                    if row == 1 && column == 1 {
                        graphics::fill_round_rect(x, y, cell / 2, cell / 2, 1, color);
                    } else {
                        graphics::outline_round_rect(x, y, cell / 2, cell / 2, 1, color);
                    }
                }
            }
        }
    }
}

fn folder(x: i32, y: i32, size: i32, color: u32) {
    let width = size;
    let height = (size * 2) / 3;
    graphics::fill_round_rect(x, y + 3, width / 2, 4, 2, color);
    graphics::fill_round_rect(x, y + 4, width, height - 4, 3, color);
    graphics::blend_rect(x + 2, y + 7, width - 4, 1, 0x000000, 45);
}

fn shield(x: i32, y: i32, size: i32, color: u32) {
    let width = size;
    let height = size;
    let points = [
        (x + width / 2, y),
        (x + width, y + height / 5),
        (x + width - width / 8, y + height * 2 / 3),
        (x + width / 2, y + height),
        (x + width / 8, y + height * 2 / 3),
        (x, y + height / 5),
    ];
    graphics::fill_polygon(&points, color);
    // Cut the check mark in the tile's dark background by drawing a dark line.
    let check = [
        (x + width / 3, y + height / 2),
        (x + width / 2 - 1, y + height * 2 / 3 - 2),
        (x + width * 3 / 4, y + height / 3),
    ];
    graphics::draw_line(check[0].0, check[0].1, check[1].0, check[1].1, 0x1b2a20);
    graphics::draw_line(check[1].0, check[1].1, check[2].0, check[2].1, 0x1b2a20);
    graphics::draw_line(
        check[0].0,
        check[0].1 + 1,
        check[1].0,
        check[1].1 + 1,
        0x1b2a20,
    );
    graphics::draw_line(
        check[1].0,
        check[1].1 + 1,
        check[2].0,
        check[2].1 + 1,
        0x1b2a20,
    );
}

fn portal(x: i32, y: i32, size: i32, color: u32) {
    let box_size = size * 3 / 4;
    let left = x + (size - box_size) / 2;
    let top = y + (size - box_size) / 2;
    graphics::outline_round_rect(left, top, box_size, box_size, 4, color);
    let arrow_y = top + box_size / 2;
    graphics::draw_line(
        left + box_size / 4,
        arrow_y,
        left + box_size * 3 / 4,
        arrow_y,
        color,
    );
    graphics::draw_line(
        left + box_size * 3 / 4 - 4,
        arrow_y - 3,
        left + box_size * 3 / 4,
        arrow_y,
        color,
    );
    graphics::draw_line(
        left + box_size * 3 / 4 - 4,
        arrow_y + 3,
        left + box_size * 3 / 4,
        arrow_y,
        color,
    );
}

fn gear(cx: i32, cy: i32, radius: i32, color: u32) {
    graphics::stroke_circle(cx, cy, radius, color);
    graphics::stroke_circle(cx, cy, radius / 2, color);
    for step in 0..8 {
        let (dx, dy) = match step {
            0 => (1, 0),
            1 => (1, 1),
            2 => (0, 1),
            3 => (-1, 1),
            4 => (-1, 0),
            5 => (-1, -1),
            6 => (0, -1),
            _ => (1, -1),
        };
        let inner = radius - 1;
        let outer = radius + 2;
        graphics::draw_line(
            cx + dx * inner,
            cy + dy * inner,
            cx + dx * outer,
            cy + dy * outer,
            color,
        );
    }
}

/// Draws a magnifying glass.
pub fn search(rect: aether_wm::Rect, color: u32) {
    let radius = (rect.width / 3).max(3);
    let cx = rect.x + radius + 2;
    let cy = rect.y + radius + 1;
    graphics::stroke_circle(cx, cy, radius, color);
    graphics::draw_line(
        cx + radius - 1,
        cy + radius - 1,
        rect.right() - 2,
        rect.bottom() - 2,
        color,
    );
    graphics::draw_line(
        cx + radius,
        cy + radius - 1,
        rect.right() - 1,
        rect.bottom() - 2,
        color,
    );
}

/// Draws a shield outline with a check.
pub fn shield_status(rect: aether_wm::Rect, color: u32) {
    let points = [
        (rect.x + rect.width / 2, rect.y),
        (rect.right() - 1, rect.y + rect.height / 4),
        (rect.right() - 2, rect.y + rect.height * 2 / 3),
        (rect.x + rect.width / 2, rect.bottom() - 1),
        (rect.x + 2, rect.y + rect.height * 2 / 3),
        (rect.x + 1, rect.y + rect.height / 4),
    ];
    graphics::fill_polygon(&points, color);
    let cx = rect.center().x;
    let cy = rect.center().y;
    graphics::draw_line(cx - 4, cy, cx - 1, cy + 3, 0x12221a);
    graphics::draw_line(cx - 1, cy + 3, cx + 4, cy - 3, 0x12221a);
}

/// Draws four workspace tiles.
pub fn workspace_grid(rect: aether_wm::Rect, color: u32) {
    let gap = 2;
    let width = (rect.width - gap) / 2;
    let height = (rect.height - gap) / 2;
    for row in 0..2 {
        for column in 0..2 {
            graphics::fill_round_rect(
                rect.x + column * (width + gap),
                rect.y + row * (height + gap),
                width,
                height,
                2,
                color,
            );
        }
    }
}

/// Draws a check mark.
pub fn check(x: i32, y: i32, size: i32, color: u32) {
    graphics::draw_line(x, y + size / 2, x + size / 3, y + size, color);
    graphics::draw_line(x + size / 3, y + size, x + size, y, color);
    graphics::draw_line(x, y + size / 2 + 1, x + size / 3, y + size + 1, color);
    graphics::draw_line(x + size / 3, y + size + 1, x + size, y + 1, color);
}

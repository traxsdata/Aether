// SPDX-License-Identifier: Apache-2.0
//! Aether's clean-room window manager model.
//!
//! This crate contains the parts of the desktop that benefit from deterministic
//! host tests: geometry, window and workspace state, focus and z-order rules,
//! hit testing, menus, dock layout, and integer animation. It contains no
//! hardware I/O, ambient service access, or capability handling. Files requests
//! go through an explicitly supplied service adapter. The kernel renders the
//! state through its own graphics service and remains the only component that
//! touches the display.
//!
//! The design is inspired by familiar desktop conventions, not copied from any
//! existing operating system. Names, metrics, palettes, artwork, and animation
//! curves are original to this project.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod anim;
pub mod browser;
pub mod geometry;
pub mod layout;
pub mod model;
pub mod theme;

pub use anim::{Anim, ease_in_out, ease_out_cubic};
pub use geometry::{DAMAGE_RECTS, Damage, Point, Rect, Size};
pub use layout::{
    DOCK_HEIGHT, DOCK_ICON_BASE, DOCK_ITEMS, DOCK_MARGIN, DOCK_SLOT_WIDTH, DockSlot,
    MENU_BAR_HEIGHT, TITLE_BAR_HEIGHT, WINDOW_RADIUS, default_window_frame, dock_icon_rects,
    dock_panel, dock_slot_at, menu_item_rect, menu_panel_rect, menu_title_rects, overview_cards,
    traffic_lights, work_area,
};
pub use model::{
    Command, Hit, Key, KeyOutcome, MAX_WINDOWS, MenuItem, MenuKind, MenuState, Modifiers,
    SearchItem, Settings, WORKSPACES, Window, WindowKind, WindowState, Wm,
};
pub use theme::{GlassLevel, Palette, ThemeKind};

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: Apache-2.0
//! Persisted desktop settings.
//!
//! `aether_wm::Settings` (reduce motion, high contrast, liquid glass, dock
//! magnification) drives accessibility. The window manager keeps only the
//! in-memory copy, so these settings are mirrored into a small versioned
//! record in the user's config area of the unified filesystem.
//!
//! The record is written only when a value actually changes and loaded into
//! a freshly booted window manager before the first desktop frame. A
//! missing, truncated, corrupt, or unknown-version record falls back to the
//! defaults and logs exactly one serial line; boot never stops because of it.

use crate::fs;
use crate::serial_line;
use aether_wm::{SETTINGS_RECORD_LEN, Settings, Wm};

/// User config area: a hidden application directory in Home, created with
/// the rest of the user workspace at format time.
pub const CONFIG_DIR: &str = "home/.aether";
/// The single settings record in that directory.
pub const SETTINGS_PATH: &str = "/home/.aether/settings";

/// Loads the saved settings into a freshly created window manager.
///
/// Call this after `Wm::new` and before the first frame so a high-contrast
/// user never sees the default theme. Any problem decoding the record keeps
/// the defaults already in `wm` and logs exactly one line.
pub fn load(wm: &mut Wm) {
    let mut stored = [0u8; SETTINGS_RECORD_LEN];
    match fs::read_path(SETTINGS_PATH, &mut stored) {
        Ok(length) => match Settings::parse(&stored[..length]) {
            Ok(settings) => wm.apply_settings(settings),
            Err(error) => serial_line!("desktop: settings record {error}; using defaults"),
        },
        Err(_) => serial_line!("desktop: settings record missing; using defaults"),
    }
}

/// Writes the current settings for `wm`.
///
/// Only call this when a value actually changed (see `Wm::settings_dirty`),
/// so the volume is touched once per user action, never per frame.
pub fn persist(wm: &Wm) {
    let mut record = [0u8; SETTINGS_RECORD_LEN];
    wm.settings().encode(&mut record);
    if fs::write_path(SETTINGS_PATH, &record).is_ok() {
        return;
    }
    // An older image may predate the config directory; create it once and
    // retry before reporting the failure.
    if fs::create_dir(CONFIG_DIR).is_ok() && fs::write_path(SETTINGS_PATH, &record).is_ok() {
        return;
    }
    serial_line!("desktop: failed to persist settings");
}

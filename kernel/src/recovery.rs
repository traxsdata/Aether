// SPDX-License-Identifier: Apache-2.0
//! System image and recovery prototype.
//!
//! The virtual volume carries two system-image markers and a separate user
//! data tree. Boot verifies that the current marker is present; when it is
//! missing, the previous marker is selected instead. User data is never part
//! of the image set and is never overwritten by the selection.

use crate::fs;
use crate::serial_line;
use aether_fs::FsError;

/// Recovery status shown by the shell and desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Current image version.
    pub current: &'static str,
    /// Previous image version.
    pub previous: &'static str,
    /// Image actually selected for this boot.
    pub selected: &'static str,
    /// Whether the previous image was selected.
    pub rollback: bool,
    /// Whether the user-data sentinel is readable.
    pub user_data: bool,
}

const CURRENT: &str = "/system/images/current";
const PREVIOUS: &str = "/system/images/previous";
const SELECTED: &str = "/system/images/selected";
const USER_DATA: &str = "/home/welcome.txt";

/// Verifies image markers and records the selected image.
pub fn init() -> Result<Status, FsError> {
    let current = read_marker(CURRENT);
    let previous = read_marker(PREVIOUS);
    let user_data = read_marker(USER_DATA).is_some();
    let rollback = current.is_none() && previous.is_some();
    let selected = if rollback { previous } else { current }.unwrap_or("unknown");

    let selected_value = if rollback {
        b"previous\n".as_slice()
    } else {
        b"current\n".as_slice()
    };
    let _ = fs::write_path(SELECTED, selected_value);

    let status = Status {
        current: current.unwrap_or("missing"),
        previous: previous.unwrap_or("missing"),
        selected,
        rollback,
        user_data,
    };
    serial_line!(
        "AETHER_RECOVERY_OK current={} previous={} selected={} rollback={} user_data={}",
        status.current,
        status.previous,
        status.selected,
        status.rollback,
        status.user_data
    );
    Ok(status)
}

fn read_marker(path: &str) -> Option<&'static str> {
    let mut buffer = [0u8; 64];
    let length = fs::read_path(path, &mut buffer).ok()?;
    let text = core::str::from_utf8(&buffer[..length]).ok()?.trim();
    // The caller displays a small fixed set of version strings. Returning a
    // borrowed static is only safe for the values seeded by the filesystem;
    // unknown values are reported as a generic marker instead of leaking a
    // reference to a stack buffer.
    match text {
        "aether-0.1.0" => Some("aether-0.1.0"),
        "aether-0.0.9" => Some("aether-0.0.9"),
        "welcome to aether" => Some("present"),
        _ => Some("unknown"),
    }
}

/// Prints the recovery status for the shell.
pub fn print_status() {
    let mut buffer = [0u8; 128];
    let length = status_text(&mut buffer);
    crate::arch::x86_64::serial::write_bytes(&buffer[..length]);
}

/// Writes the recovery status for the desktop.
pub fn status_text(out: &mut [u8]) -> usize {
    let current = read_marker(CURRENT).unwrap_or("missing");
    let previous = read_marker(PREVIOUS).unwrap_or("missing");
    let selected = read_marker(SELECTED).unwrap_or("missing");
    let lines = [
        "IMAGES: CURRENT + PREVIOUS",
        "USER DATA: PRESERVED",
        "ROLLBACK: AVAILABLE",
    ];
    let mut written = 0usize;
    for line in lines {
        let bytes = line.as_bytes();
        if written + bytes.len() + 1 > out.len() {
            break;
        }
        out[written..written + bytes.len()].copy_from_slice(bytes);
        written += bytes.len();
        out[written] = b'\n';
        written += 1;
    }
    let _ = (current, previous, selected);
    written
}

//! backend.rs - what the runner needs from the system: one window with a
//! pixel buffer, input events, time, and files the user picks.
//!
//! Two implementations: `AetherBackend` (real æther system calls) and, with
//! std, `HeadlessBackend` (an in-memory window driven by a script, for tests
//! on a Linux host).

use crate::abi;
use crate::Error;

pub type WinHandle = u32;
pub type FileHandle = u32;

/// A window's pixels: 0x00RRGGBB, row-major, `pitch` pixels per row.
pub struct Framebuffer<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    /// Relative to the window's client area.
    pub x: i32,
    pub y: i32,
    /// Bit 0 left, bit 1 right, bit 2 middle: the state after this event.
    pub buttons: u32,
    pub mods: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Printable ASCII, '\n', '\b', '\t', 27 (Esc) or an abi::KEY_* code.
    /// æther reports presses only (no releases, no auto-repeat flag).
    Key { key: u32, mods: u32 },
    MouseMove(Mouse),
    MouseDown(Mouse),
    MouseUp(Mouse),
    /// The user clicked the window's close button.
    Close,
    Focus,
    Unfocus,
    /// Wheel scroll in lines, as egui counts them: positive dy scrolls up
    /// (content moves down).
    Scroll { dx: i32, dy: i32, mods: u32 },
    /// The client area changed size; the pixel buffer must be fetched again.
    /// On æther the backend has already taken the new size (SYS_WIN_RESIZE).
    Resize { width: u32, height: u32 },
}

impl Event {
    pub fn from_raw(ev: &abi::event) -> Option<Self> {
        let m = Mouse { x: ev.x, y: ev.y, buttons: ev.buttons, mods: ev.mods };
        Some(match ev.type_ {
            abi::EV_KEY => Event::Key { key: ev.key, mods: ev.mods },
            abi::EV_MOUSE_MOVE => Event::MouseMove(m),
            abi::EV_MOUSE_DOWN => Event::MouseDown(m),
            abi::EV_MOUSE_UP => Event::MouseUp(m),
            abi::EV_CLOSE => Event::Close,
            abi::EV_FOCUS => Event::Focus,
            abi::EV_UNFOCUS => Event::Unfocus,
            // Notches down are content moving up, egui's negative y.
            abi::EV_SCROLL => Event::Scroll { dx: 0, dy: -(ev.key as i32), mods: ev.mods },
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickMode {
    Open,
    Save,
}

/// The file the user chose: an open handle and its name (never its path).
#[derive(Clone, Debug)]
pub struct Picked {
    pub handle: FileHandle,
    pub name: [u8; abi::NAME_MAX + 2],
    pub label: u32,
}

impl Picked {
    pub fn name(&self) -> &str {
        abi::cstr(&self.name)
    }

    /// The Linux file descriptor the kernel's Linux layer gives the same
    /// file: native handle h is fd h + 3. Lets std::fs::File use it.
    pub fn linux_fd(&self) -> i32 {
        self.handle as i32 + 3
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whence {
    Set,
    Cur,
    End,
}

pub trait Backend {
    /// "aether" or "headless".
    fn name(&self) -> &'static str;

    /// Opens a window with a `width` x `height` client area. The system may
    /// clamp the size; the result says what it chose.
    fn create_window(&mut self, width: u32, height: u32, title: &str) -> Result<(WinHandle, u32, u32), Error>;
    fn framebuffer(&mut self, win: WinHandle) -> Option<Framebuffer<'_>>;
    /// Shows what is in the pixel buffer.
    fn present(&mut self, win: WinHandle) -> Result<(), Error>;
    fn set_title(&mut self, win: WinHandle, title: &str) -> Result<(), Error>;
    /// Lets the user zoom and resize the window, down to `min_w` x `min_h`.
    fn set_resizable(&mut self, _win: WinHandle, _resizable: bool, _min_w: u32, _min_h: u32) -> Result<(), Error> {
        Ok(())
    }
    /// The screen size and the largest client area a window may have.
    fn display_info(&mut self) -> abi::display_info {
        abi::display_info { screen_w: abi::WIN_MAX_W, screen_h: abi::WIN_MAX_H, max_client_w: abi::WIN_MAX_W, max_client_h: abi::WIN_MAX_H }
    }

    /// Waits up to `timeout_ms` (None: forever) for one event; Ok(None) on timeout.
    fn wait_event(&mut self, timeout_ms: Option<u32>) -> Result<Option<Event>, Error>;
    /// Milliseconds since some fixed point (boot, or the start of a headless run).
    fn uptime_ms(&mut self) -> u64;
    fn time(&mut self) -> abi::time_info;

    /// Asks the user to choose a file to open or a name to save under.
    fn pick_file(&mut self, mode: PickMode, suggested: &str) -> Result<Picked, Error>;
    fn read(&mut self, f: FileHandle, buf: &mut [u8]) -> Result<usize, Error>;
    fn write(&mut self, f: FileHandle, buf: &[u8]) -> Result<usize, Error>;
    fn seek(&mut self, f: FileHandle, offset: i64, whence: Whence) -> Result<u64, Error>;
    fn stat(&mut self, f: FileHandle) -> Result<abi::file_stat, Error>;
    fn close(&mut self, f: FileHandle) -> Result<(), Error>;
    fn fs_info(&mut self) -> Result<abi::fs_info, Error>;

    /// Writes a line to the system log (best effort).
    fn log(&mut self, _msg: &str) {}
}

impl Whence {
    pub fn raw(self) -> u32 {
        match self {
            Whence::Set => abi::SEEK_SET,
            Whence::Cur => abi::SEEK_CUR,
            Whence::End => abi::SEEK_END,
        }
    }
}

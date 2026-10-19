//! aether.rs - the Backend on real æther, through native system calls.

use crate::abi;
use crate::backend::*;
use crate::sys;
use crate::Error;

struct Win {
    handle: WinHandle,
    info: abi::win_info,
}

#[derive(Default)]
pub struct AetherBackend {
    wins: [Option<Win>; 4],
}

// The pixel buffer is plain process memory the kernel mapped for us; only
// the thread that owns the backend touches it.
unsafe impl Send for AetherBackend {}

impl AetherBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn win(&self, h: WinHandle) -> Option<&Win> {
        self.wins.iter().flatten().find(|w| w.handle == h)
    }
}

impl Backend for AetherBackend {
    fn name(&self) -> &'static str {
        "aether"
    }

    fn create_window(&mut self, width: u32, height: u32, title: &str) -> Result<(WinHandle, u32, u32), Error> {
        let d = self.display_info();
        let width = width.clamp(1, d.max_client_w.clamp(1, abi::WIN_MAX_W));
        let height = height.clamp(1, d.max_client_h.clamp(1, abi::WIN_MAX_H));
        let slot = self.wins.iter().position(|w| w.is_none()).ok_or(Error(abi::E_LIMIT))?;
        let mut buf = [0u8; abi::WIN_TITLE_MAX];
        let (handle, info) = sys::win_create(width, height, ascii_title(title, &mut buf))?;
        if info.pixels == 0 || info.pitch < info.width {
            return Err(Error(abi::E_INVAL));
        }
        self.wins[slot] = Some(Win { handle, info });
        Ok((handle, info.width, info.height))
    }

    fn framebuffer(&mut self, win: WinHandle) -> Option<Framebuffer<'_>> {
        let w = self.win(win)?;
        let i = w.info;
        let len = i.pitch as usize * i.height as usize;
        // The kernel guarantees pitch * height pixels at `pixels` for the
        // window's lifetime, which is the process's.
        let pixels = unsafe { core::slice::from_raw_parts_mut(i.pixels as *mut u32, len) };
        Some(Framebuffer { pixels, width: i.width, height: i.height, pitch: i.pitch })
    }

    fn present(&mut self, win: WinHandle) -> Result<(), Error> {
        sys::win_present(win)
    }

    fn set_title(&mut self, win: WinHandle, title: &str) -> Result<(), Error> {
        let mut buf = [0u8; abi::WIN_TITLE_MAX];
        sys::win_title(win, ascii_title(title, &mut buf))
    }

    fn set_resizable(&mut self, win: WinHandle, resizable: bool, min_w: u32, min_h: u32) -> Result<(), Error> {
        sys::win_flags(win, if resizable { abi::WIN_FLAG_RESIZABLE } else { 0 }, min_w, min_h)
    }

    fn display_info(&mut self) -> abi::display_info {
        // Older kernels lack the call: fall back to abi.h's limits.
        sys::display_info().unwrap_or(abi::display_info {
            screen_w: abi::WIN_MAX_W,
            screen_h: abi::WIN_MAX_H,
            max_client_w: abi::WIN_MAX_W,
            max_client_h: abi::WIN_MAX_H,
        })
    }

    fn wait_event(&mut self, timeout_ms: Option<u32>) -> Result<Option<Event>, Error> {
        loop {
            match sys::event_wait(timeout_ms)? {
                None => return Ok(None),
                // Another thread asked for a frame: end the wait as if it timed out.
                Some(raw) if raw.type_ == abi::EV_WAKE => return Ok(None),
                // The user zoomed or resized the window: take the new size now,
                // before anything draws into the old buffer again.
                Some(raw) if raw.type_ == abi::EV_RESIZE => {
                    let Some(w) = self.wins.iter_mut().flatten().find(|w| w.handle == raw.window) else { continue };
                    match sys::win_resize(w.handle) {
                        Ok(info) if info.pixels != 0 && info.pitch >= info.width => {
                            w.info = info;
                            return Ok(Some(Event::Resize { width: info.width, height: info.height }));
                        }
                        _ => continue, // kept the old size
                    }
                }
                Some(raw) => {
                    if let Some(ev) = Event::from_raw(&raw) {
                        return Ok(Some(ev));
                    }
                    // An event type this runner doesn't know: skip it.
                }
            }
        }
    }

    fn uptime_ms(&mut self) -> u64 {
        sys::time().map(|t| t.uptime_ms).unwrap_or(0)
    }

    fn time(&mut self) -> abi::time_info {
        sys::time().unwrap_or_default()
    }

    fn pick_file(&mut self, mode: PickMode, suggested: &str) -> Result<Picked, Error> {
        let m = match mode {
            PickMode::Open => abi::PICK_OPEN,
            PickMode::Save => abi::PICK_SAVE,
        };
        let (handle, res) = sys::pick_file(m, suggested)?;
        Ok(Picked { handle, name: res.name, label: res.label })
    }

    fn read(&mut self, f: FileHandle, buf: &mut [u8]) -> Result<usize, Error> {
        sys::read(f, buf)
    }

    fn write(&mut self, f: FileHandle, buf: &[u8]) -> Result<usize, Error> {
        sys::write(f, buf)
    }

    fn seek(&mut self, f: FileHandle, offset: i64, whence: Whence) -> Result<u64, Error> {
        sys::seek(f, offset, whence.raw())
    }

    fn stat(&mut self, f: FileHandle) -> Result<abi::file_stat, Error> {
        sys::fstat(f)
    }

    fn close(&mut self, f: FileHandle) -> Result<(), Error> {
        sys::close(f)
    }

    fn fs_info(&mut self) -> Result<abi::fs_info, Error> {
        sys::fs_info()
    }

    // No `log`: untrusted apps lack CAP_LOG, and each refused SYS_LOG would
    // land in the kernel's audit log. Logging goes to stderr instead.
}

/// The kernel draws titles with an ASCII font: common punctuation becomes its
/// ASCII look-alike, anything else '?', one per character.
pub fn ascii_title<'a>(title: &str, buf: &'a mut [u8; abi::WIN_TITLE_MAX]) -> &'a str {
    let mut n = 0;
    for c in title.chars() {
        let a: &[u8] = match c {
            ' '..='~' => &[c as u8],
            '\u{2013}' | '\u{2014}' | '\u{2212}' => b"-",
            '\u{2018}' | '\u{2019}' => b"'",
            '\u{201C}' | '\u{201D}' => b"\"",
            '\u{2022}' | '\u{00B7}' => b"*",
            '\u{2026}' => b"...",
            '\u{00A0}' | '\t' => b" ",
            _ => b"?",
        };
        if n + a.len() >= buf.len() {
            break;
        }
        buf[n..n + a.len()].copy_from_slice(a);
        n += a.len();
    }
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

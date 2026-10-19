//! headless.rs - the Backend for tests on a Linux host: windows are memory
//! buffers, input comes from a script, time is virtual, and frames can be
//! written out as PNG files.
//!
//! Script lines (from `AETHER_SCRIPT=<file>` or `Script::parse`):
//!   frame [n]          end the current batch of input n times (one egui frame each)
//!   wait <ms>          let virtual time pass
//!   move x y           pointer motion
//!   down x y [button]  button: left (default), right, middle
//!   up x y [button]
//!   click x y [button] down + up
//!   type <text>        one key event per character, a frame apart
//!   key [mods+]<name>  enter, backspace, tab, esc, up, down, left, right,
//!                      home, end, pgup, pgdn, delete, or one character;
//!                      mods: ctrl, shift, alt (e.g. "key ctrl+a")
//!   dump <file.png>    write what the window shows now
//!   pick <host path>   the next file picker returns this file
//!   scroll dx dy       wheel lines (positive dy scrolls up, as in egui)
//!   focus / unfocus / close
//! When the script runs out the window is closed, so every run ends.
//! `AETHER_DUMP_DIR=<dir>` also writes every presented frame to dir/frame-NNNNN.png.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use crate::abi;
use crate::backend::*;
use crate::Error;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Event(Event),
    /// Ends a batch: the runner sees a timeout and draws a frame.
    Frame,
    Wait(u64),
    Dump(PathBuf),
    Pick(PathBuf),
}

#[derive(Clone, Debug, Default)]
pub struct Script {
    pub steps: VecDeque<Step>,
}

fn button_bit(s: Option<&str>) -> Result<u32, String> {
    match s.unwrap_or("left") {
        "left" => Ok(1),
        "right" => Ok(2),
        "middle" => Ok(4),
        b => Err(format!("unknown button {b:?}")),
    }
}

fn key_code(name: &str) -> Result<u32, String> {
    Ok(match name {
        "enter" | "return" => b'\n' as u32,
        "backspace" => 8,
        "tab" => b'\t' as u32,
        "esc" | "escape" => 27,
        "space" => b' ' as u32,
        "up" => abi::KEY_UP,
        "down" => abi::KEY_DOWN,
        "left" => abi::KEY_LEFT,
        "right" => abi::KEY_RIGHT,
        "home" => abi::KEY_HOME,
        "end" => abi::KEY_END,
        "pgup" => abi::KEY_PGUP,
        "pgdn" => abi::KEY_PGDN,
        "delete" | "del" => abi::KEY_DELETE,
        s if s.chars().count() == 1 && s.is_ascii() => s.as_bytes()[0] as u32,
        s => return Err(format!("unknown key {s:?}")),
    })
}

impl Script {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut s = Script::default();
        let mut buttons = 0u32;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
            let mut args = rest.split_whitespace();
            let mut num = |what: &str| -> Result<i32, String> {
                args.next().and_then(|a| a.parse().ok()).ok_or_else(|| format!("line {}: {cmd} needs {what}", n + 1))
            };
            let mouse = |x, y, buttons| Mouse { x, y, buttons, mods: 0 };
            match cmd {
                "frame" => {
                    let k = rest.trim().parse().unwrap_or(1);
                    for _ in 0..k {
                        s.steps.push_back(Step::Frame);
                    }
                }
                "wait" => s.steps.push_back(Step::Wait(num("milliseconds")? as u64)),
                "move" => {
                    let (x, y) = (num("x")?, num("y")?);
                    s.steps.push_back(Step::Event(Event::MouseMove(mouse(x, y, buttons))));
                }
                "down" | "up" | "click" => {
                    let (x, y) = (num("x")?, num("y")?);
                    let bit = button_bit(rest.split_whitespace().nth(2)).map_err(|e| format!("line {}: {e}", n + 1))?;
                    if cmd != "up" {
                        buttons |= bit;
                        s.steps.push_back(Step::Event(Event::MouseDown(mouse(x, y, buttons))));
                    }
                    if cmd != "down" {
                        buttons &= !bit;
                        s.steps.push_back(Step::Event(Event::MouseUp(mouse(x, y, buttons))));
                    }
                }
                "type" => {
                    for c in rest.chars() {
                        if c.is_ascii() {
                            let mods = if c.is_ascii_uppercase() { abi::MOD_SHIFT } else { 0 };
                            s.steps.push_back(Step::Event(Event::Key { key: c as u32, mods }));
                            // A person's keys land in separate frames.
                            s.steps.push_back(Step::Frame);
                        }
                    }
                }
                "key" => {
                    let spec = rest.trim();
                    let mut mods = 0;
                    let mut name = spec;
                    // "ctrl+a"; a lone "+" is the plus key.
                    while let Some((m, tail)) = name.split_once('+').filter(|(m, t)| !m.is_empty() && !t.is_empty()) {
                        mods |= match m {
                            "ctrl" => abi::MOD_CTRL,
                            "shift" => abi::MOD_SHIFT,
                            "alt" => abi::MOD_ALT,
                            _ => return Err(format!("line {}: unknown modifier {m:?}", n + 1)),
                        };
                        name = tail;
                    }
                    let key = key_code(name).map_err(|e| format!("line {}: {e}", n + 1))?;
                    s.steps.push_back(Step::Event(Event::Key { key, mods }));
                }
                "dump" => s.steps.push_back(Step::Dump(PathBuf::from(rest.trim()))),
                "pick" => s.steps.push_back(Step::Pick(PathBuf::from(rest.trim()))),
                "scroll" => {
                    let (dx, dy) = (num("dx")?, num("dy")?);
                    s.steps.push_back(Step::Event(Event::Scroll { dx, dy, mods: 0 }));
                }
                "focus" => s.steps.push_back(Step::Event(Event::Focus)),
                "unfocus" => s.steps.push_back(Step::Event(Event::Unfocus)),
                "close" => s.steps.push_back(Step::Event(Event::Close)),
                _ => return Err(format!("line {}: unknown command {cmd:?}", n + 1)),
            }
        }
        Ok(s)
    }
}

struct Win {
    handle: WinHandle,
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    title: String,
}

pub struct HeadlessBackend {
    wins: Vec<Win>,
    script: Script,
    /// Virtual milliseconds since the run started.
    clock: u64,
    /// Frames presented so far.
    pub presents: u64,
    dump_dir: Option<PathBuf>,
    next_pick: Option<PathBuf>,
    files: Vec<(FileHandle, File)>,
    next_file: FileHandle,
    closed_sent: bool,
    idle_after_close: u32,
}

impl HeadlessBackend {
    pub fn new(script: Script) -> Self {
        HeadlessBackend {
            wins: Vec::new(),
            script,
            clock: 0,
            presents: 0,
            dump_dir: None,
            next_pick: None,
            files: Vec::new(),
            next_file: 3,
            closed_sent: false,
            idle_after_close: 0,
        }
    }

    /// Configured from the environment: AETHER_SCRIPT, AETHER_DUMP_DIR, AETHER_PICK.
    pub fn from_env() -> Self {
        let script = match std::env::var_os("AETHER_SCRIPT") {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => Script::parse(&text).unwrap_or_else(|e| {
                    std::eprintln!("aether-host: bad script {}: {e}", PathBuf::from(&path).display());
                    Script::default()
                }),
                Err(e) => {
                    std::eprintln!("aether-host: can't read script {}: {e}", PathBuf::from(&path).display());
                    Script::default()
                }
            },
            None => Script::default(),
        };
        let mut b = Self::new(script);
        b.dump_dir = std::env::var_os("AETHER_DUMP_DIR").map(PathBuf::from);
        b.next_pick = std::env::var_os("AETHER_PICK").map(PathBuf::from);
        b
    }

    pub fn set_dump_dir(&mut self, dir: Option<PathBuf>) {
        self.dump_dir = dir;
    }

    pub fn title(&self, win: WinHandle) -> Option<&str> {
        self.wins.iter().find(|w| w.handle == win).map(|w| w.title.as_str())
    }

    fn dump(&self, path: &std::path::Path) {
        let Some(w) = self.wins.first() else { return };
        let png = crate::png::encode_xrgb(&w.pixels, w.width, w.height, w.width);
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(path, png) {
            std::eprintln!("aether-host: can't write {}: {e}", path.display());
        }
    }

    fn file(&mut self, f: FileHandle) -> Result<&mut File, Error> {
        self.files.iter_mut().find(|(h, _)| *h == f).map(|(_, file)| file).ok_or(Error(abi::E_NOENT))
    }
}

fn io_err(e: std::io::Error) -> Error {
    use std::io::ErrorKind::*;
    Error(match e.kind() {
        NotFound => abi::E_NOENT,
        PermissionDenied => abi::E_PERM,
        AlreadyExists => abi::E_EXIST,
        InvalidInput => abi::E_INVAL,
        _ => abi::E_IO,
    })
}

impl Backend for HeadlessBackend {
    fn name(&self) -> &'static str {
        "headless"
    }

    fn create_window(&mut self, width: u32, height: u32, title: &str) -> Result<(WinHandle, u32, u32), Error> {
        let width = width.clamp(1, abi::WIN_MAX_W);
        let height = height.clamp(1, abi::WIN_MAX_H);
        let handle = self.wins.len() as WinHandle + 1;
        self.wins.push(Win {
            handle,
            width,
            height,
            pixels: std::vec![0; (width * height) as usize],
            title: title.into(),
        });
        Ok((handle, width, height))
    }

    fn framebuffer(&mut self, win: WinHandle) -> Option<Framebuffer<'_>> {
        let w = self.wins.iter_mut().find(|w| w.handle == win)?;
        Some(Framebuffer { pixels: &mut w.pixels, width: w.width, height: w.height, pitch: w.width })
    }

    fn present(&mut self, win: WinHandle) -> Result<(), Error> {
        if !self.wins.iter().any(|w| w.handle == win) {
            return Err(Error(abi::E_NOENT));
        }
        self.presents += 1;
        if let Some(dir) = &self.dump_dir {
            let path = dir.join(format!("frame-{:05}.png", self.presents));
            self.dump(&path);
        }
        Ok(())
    }

    fn set_title(&mut self, win: WinHandle, title: &str) -> Result<(), Error> {
        let w = self.wins.iter_mut().find(|w| w.handle == win).ok_or(Error(abi::E_NOENT))?;
        w.title = title.chars().take(abi::WIN_TITLE_MAX - 1).collect();
        Ok(())
    }

    fn wait_event(&mut self, timeout_ms: Option<u32>) -> Result<Option<Event>, Error> {
        loop {
            match self.script.steps.pop_front() {
                Some(Step::Event(e)) => {
                    // Input arrives a little apart, as from a person.
                    self.clock += 1;
                    return Ok(Some(e));
                }
                Some(Step::Frame) => {
                    self.clock += 16;
                    return Ok(None);
                }
                Some(Step::Wait(ms)) => {
                    // Time passes; a repaint deadline inside it fires first.
                    // A busy app (timeout 0) still sees a frame's worth pass.
                    match timeout_ms.map(|t| (t as u64).max(16)) {
                        Some(t) if t < ms => {
                            self.clock += t;
                            self.script.steps.push_front(Step::Wait(ms - t));
                            return Ok(None);
                        }
                        _ => self.clock += ms,
                    }
                }
                Some(Step::Dump(path)) => self.dump(&path),
                Some(Step::Pick(path)) => self.next_pick = Some(path),
                None => {
                    // Script finished: close, once; afterwards just let time pass.
                    if !self.closed_sent {
                        self.closed_sent = true;
                        return Ok(Some(Event::Close));
                    }
                    self.clock += timeout_ms.unwrap_or(1000).max(16) as u64;
                    self.idle_after_close += 1;
                    if self.idle_after_close > 2000 {
                        // The app cancelled the close and keeps running: end the test run.
                        std::eprintln!("aether-host: headless script over and the app won't close; exiting");
                        std::process::exit(3);
                    }
                    return Ok(None);
                }
            }
        }
    }

    fn uptime_ms(&mut self) -> u64 {
        self.clock
    }

    fn time(&mut self) -> abi::time_info {
        abi::time_info { uptime_ms: self.clock, year: 2026, month: 1, day: 1, ..Default::default() }
    }

    fn pick_file(&mut self, mode: PickMode, suggested: &str) -> Result<Picked, Error> {
        let path = match (self.next_pick.take(), mode) {
            (Some(p), PickMode::Save) if p.is_dir() => p.join(suggested),
            (Some(p), _) => p,
            (None, _) => return Err(Error(abi::E_DENIED)), // the user cancelled
        };
        let file = match mode {
            PickMode::Open => File::open(&path),
            PickMode::Save => OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path),
        }
        .map_err(io_err)?;
        let handle = self.next_file;
        self.next_file += 1;
        self.files.push((handle, file));
        let mut name = [0u8; abi::NAME_MAX + 2];
        let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        abi::copy_cstr(&mut name, &base);
        Ok(Picked { handle, name, label: abi::LABEL_PERSONAL })
    }

    fn read(&mut self, f: FileHandle, buf: &mut [u8]) -> Result<usize, Error> {
        self.file(f)?.read(buf).map_err(io_err)
    }

    fn write(&mut self, f: FileHandle, buf: &[u8]) -> Result<usize, Error> {
        self.file(f)?.write(buf).map_err(io_err)
    }

    fn seek(&mut self, f: FileHandle, offset: i64, whence: Whence) -> Result<u64, Error> {
        let pos = match whence {
            Whence::Set => SeekFrom::Start(offset.max(0) as u64),
            Whence::Cur => SeekFrom::Current(offset),
            Whence::End => SeekFrom::End(offset),
        };
        self.file(f)?.seek(pos).map_err(io_err)
    }

    fn stat(&mut self, f: FileHandle) -> Result<abi::file_stat, Error> {
        let md = self.file(f)?.metadata().map_err(io_err)?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Ok(abi::file_stat {
            size: md.len(),
            mtime,
            type_: if md.is_dir() { abi::FT_DIR } else { abi::FT_FILE },
            label: abi::LABEL_PERSONAL,
        })
    }

    fn close(&mut self, f: FileHandle) -> Result<(), Error> {
        let i = self.files.iter().position(|(h, _)| *h == f).ok_or(Error(abi::E_NOENT))?;
        self.files.remove(i);
        Ok(())
    }

    fn fs_info(&mut self) -> Result<abi::fs_info, Error> {
        let mut fi = abi::fs_info { kind: abi::FS_TEMPORARY, block_size: 4096, ..Default::default() };
        abi::copy_cstr(&mut fi.device, "headless");
        Ok(fi)
    }

    fn log(&mut self, msg: &str) {
        std::eprintln!("[aether log] {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scripts() {
        let s = Script::parse("click 10 20\nframe 2\ntype Hi\nkey ctrl+a\nkey +\nwait 5").unwrap();
        let v: Vec<_> = s.steps.into_iter().collect();
        assert_eq!(v.len(), 11);
        assert_eq!(v[0], Step::Event(Event::MouseDown(Mouse { x: 10, y: 20, buttons: 1, mods: 0 })));
        assert_eq!(v[1], Step::Event(Event::MouseUp(Mouse { x: 10, y: 20, buttons: 0, mods: 0 })));
        assert_eq!(v[4], Step::Event(Event::Key { key: 'H' as u32, mods: abi::MOD_SHIFT }));
        assert_eq!(v[5], Step::Frame);
        assert_eq!(v[8], Step::Event(Event::Key { key: 'a' as u32, mods: abi::MOD_CTRL }));
        assert_eq!(v[9], Step::Event(Event::Key { key: '+' as u32, mods: 0 }));
    }

    #[test]
    fn waits_end_for_busy_apps() {
        let mut b = HeadlessBackend::new(Script::parse("wait 100").unwrap());
        let mut n = 0;
        while b.wait_event(Some(0)).unwrap() != Some(Event::Close) {
            n += 1;
            assert!(n < 100, "wait never ended");
        }
        assert!(b.uptime_ms() >= 100);
    }

    #[test]
    fn closes_when_script_ends() {
        let mut b = HeadlessBackend::new(Script::parse("frame").unwrap());
        assert_eq!(b.wait_event(None).unwrap(), None);
        assert_eq!(b.wait_event(None).unwrap(), Some(Event::Close));
    }
}

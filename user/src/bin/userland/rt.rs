// SPDX-License-Identifier: Apache-2.0
//! Small runtime shared by every terminal program: arguments, buffered
//! output, standard input, paths, and file helpers.
//!
//! There is no heap. Buffers live on the 4 MiB user stack, and every limit
//! below follows a kernel bound (a file is at most 4 KiB, a path 128 bytes).

use aether_abi::user::{
    ARGUMENT_BLOCK_BYTES, MAX_ARGUMENTS, STDIN_IS_TERMINAL, STDOUT_IS_TERMINAL, errno,
};
use aether_sdk as sdk;
use core::fmt::{self, Write as _};

/// Largest file the volume stores.
pub const MAX_FILE: usize = 4096;
/// Longest path the kernel resolves.
pub const MAX_PATH: usize = 128;

/// The argument block: working directory, program name, and arguments.
pub struct Args {
    block: [u8; ARGUMENT_BLOCK_BYTES],
    starts: [(u16, u16); MAX_ARGUMENTS],
    count: usize,
}

impl Args {
    /// Reads the caller's argument block from the kernel.
    pub fn load() -> Self {
        let mut args = Self {
            block: [0; ARGUMENT_BLOCK_BYTES],
            starts: [(0, 0); MAX_ARGUMENTS],
            count: 0,
        };
        let length = sdk::arguments(&mut args.block).unwrap_or(0);
        let mut start = 0;
        for index in 0..=length {
            if (index == length || args.block[index] == 0) && args.count < MAX_ARGUMENTS {
                if index == length && start == index {
                    break;
                }
                args.starts[args.count] = (start as u16, index as u16);
                args.count += 1;
                start = index + 1;
            }
        }
        args
    }

    fn entry(&self, index: usize) -> &str {
        if index >= self.count {
            return "";
        }
        let (start, end) = self.starts[index];
        core::str::from_utf8(&self.block[usize::from(start)..usize::from(end)]).unwrap_or("")
    }

    /// The working directory the program was started in.
    pub fn cwd(&self) -> &str {
        match self.entry(0) {
            "" => "/",
            cwd => cwd,
        }
    }

    /// The program name (`argv[0]`).
    pub fn program(&self) -> &str {
        self.entry(1)
    }

    /// Number of arguments after the program name.
    pub fn len(&self) -> usize {
        self.count.saturating_sub(2)
    }

    /// Argument `index` after the program name.
    pub fn get(&self, index: usize) -> &str {
        self.entry(index + 2)
    }

    /// Iterates over the arguments after the program name.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|index| self.get(index))
    }
}

/// Writes all of `bytes` to standard output, waiting while a pipe is full.
/// A pipe without readers ends the program, as `SIGPIPE` does.
pub fn write_all(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let chunk = &bytes[..bytes.len().min(4096)];
        let written = sdk::print(chunk);
        if written == errno::EPIPE {
            sdk::exit(141);
        }
        if written == errno::EAGAIN || written == 0 {
            let _ = sdk::sleep();
            continue;
        }
        if written < 0 {
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

/// Buffered standard output with `core::fmt` support.
pub struct Out {
    buffer: [u8; 2048],
    len: usize,
}

impl Out {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self {
            buffer: [0; 2048],
            len: 0,
        }
    }

    /// Appends bytes, flushing when the buffer fills.
    pub fn bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.len == self.buffer.len() {
                self.flush();
            }
            self.buffer[self.len] = byte;
            self.len += 1;
        }
    }

    /// Appends a string.
    pub fn str(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    /// Writes the buffered bytes.
    pub fn flush(&mut self) {
        write_all(&self.buffer[..self.len]);
        self.len = 0;
    }
}

impl fmt::Write for Out {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.str(text);
        Ok(())
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Returns `(stdin is the terminal, stdout is the terminal)`.
pub fn stdio_terminal() -> (bool, bool) {
    let mode = sdk::terminal_mode().unwrap_or(0);
    (
        mode & STDIN_IS_TERMINAL != 0,
        mode & STDOUT_IS_TERMINAL != 0,
    )
}

/// Reads raw standard input, waiting until something arrives. Returns 0 at
/// end of input.
pub fn read_raw(buffer: &mut [u8]) -> usize {
    loop {
        match sdk::read_input(buffer) {
            Ok(count) => return count,
            Err(errno::EAGAIN) => {
                let _ = sdk::sleep();
            }
            Err(_) => return 0,
        }
    }
}

/// Reads raw standard input without waiting. `None` means nothing queued.
pub fn try_read_raw(buffer: &mut [u8]) -> Option<usize> {
    match sdk::read_input(buffer) {
        Ok(count) => Some(count),
        Err(errno::EAGAIN) => None,
        Err(_) => Some(0),
    }
}

/// Standard input as a byte stream. A terminal is read a line at a time with
/// echo and Backspace, and Ctrl+D ends the input, as a cooked tty would.
pub struct Stdin {
    terminal: bool,
    buffer: [u8; 512],
    start: usize,
    end: usize,
    eof: bool,
}

impl Stdin {
    /// Opens standard input.
    pub fn new() -> Self {
        Self {
            terminal: stdio_terminal().0,
            buffer: [0; 512],
            start: 0,
            end: 0,
            eof: false,
        }
    }

    fn fill(&mut self) -> bool {
        if self.eof {
            return false;
        }
        self.start = 0;
        self.end = 0;
        if !self.terminal {
            self.end = read_raw(&mut self.buffer);
            if self.end == 0 {
                self.eof = true;
            }
            return self.end > 0;
        }
        // Cooked line input from the terminal.
        let mut echo = Out::new();
        loop {
            let mut key = [0u8; 16];
            let count = read_raw(&mut key);
            for &byte in &key[..count] {
                match byte {
                    b'\r' | b'\n' => {
                        if self.end < self.buffer.len() {
                            self.buffer[self.end] = b'\n';
                            self.end += 1;
                        }
                        echo.str("\n");
                        echo.flush();
                        return true;
                    }
                    0x04 => {
                        echo.flush();
                        if self.end == 0 {
                            self.eof = true;
                            return false;
                        }
                        return true;
                    }
                    0x7f | 0x08 => {
                        if self.end > 0 {
                            self.end -= 1;
                            echo.str("\x08 \x08");
                        }
                    }
                    0x20..=0x7e | b'\t' if self.end < self.buffer.len() - 1 => {
                        self.buffer[self.end] = byte;
                        self.end += 1;
                        echo.bytes(&[byte]);
                    }
                    _ => {}
                }
            }
            echo.flush();
        }
    }

    /// Reads up to `out.len()` bytes; 0 means end of input.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        if self.start == self.end && !self.fill() {
            return 0;
        }
        let count = (self.end - self.start).min(out.len());
        out[..count].copy_from_slice(&self.buffer[self.start..self.start + count]);
        self.start += count;
        count
    }

    /// Reads one line (without its newline) into `out`. Returns `None` at end
    /// of input. Longer lines are split.
    pub fn read_line<'a>(&mut self, out: &'a mut [u8]) -> Option<&'a [u8]> {
        let mut length = 0;
        loop {
            if self.start == self.end && !self.fill() {
                return if length == 0 {
                    None
                } else {
                    Some(&out[..length])
                };
            }
            let byte = self.buffer[self.start];
            self.start += 1;
            if byte == b'\n' {
                return Some(&out[..length]);
            }
            if length < out.len() {
                out[length] = byte;
                length += 1;
            } else {
                self.start -= 1;
                return Some(&out[..length]);
            }
        }
    }
}

/// A normalized absolute path inside the terminal's root.
#[derive(Clone, Copy)]
pub struct Path {
    bytes: [u8; MAX_PATH],
    len: usize,
}

impl Path {
    /// The root, `/`.
    pub const fn root() -> Self {
        let mut bytes = [0; MAX_PATH];
        bytes[0] = b'/';
        Self { bytes, len: 1 }
    }

    /// Resolves `input` against `cwd`: `.` and `..` are folded lexically and
    /// `..` stops at the root. Returns `None` when the result is too long.
    pub fn resolve(cwd: &str, input: &str) -> Option<Self> {
        let mut path = Self::root();
        if !input.starts_with('/') {
            for component in cwd.split('/') {
                path.push_component(component)?;
            }
        }
        for component in input.split('/') {
            path.push_component(component)?;
        }
        Some(path)
    }

    fn push_component(&mut self, component: &str) -> Option<()> {
        match component {
            "" | "." => Some(()),
            ".." => {
                self.pop();
                Some(())
            }
            name => self.push(name),
        }
    }

    /// Appends a name.
    pub fn push(&mut self, name: &str) -> Option<()> {
        let extra = name.len() + usize::from(self.len > 1);
        if self.len + extra > MAX_PATH {
            return None;
        }
        if self.len > 1 {
            self.bytes[self.len] = b'/';
            self.len += 1;
        }
        self.bytes[self.len..self.len + name.len()].copy_from_slice(name.as_bytes());
        self.len += name.len();
        Some(())
    }

    /// Removes the last component.
    pub fn pop(&mut self) {
        while self.len > 1 && self.bytes[self.len - 1] != b'/' {
            self.len -= 1;
        }
        if self.len > 1 {
            self.len -= 1;
        }
    }

    /// The path text.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("/")
    }

    /// The final component (`/` for the root).
    pub fn name(&self) -> &str {
        let text = self.as_str();
        match text.rsplit_once('/') {
            Some((_, "")) | None => "/",
            Some((_, name)) => name,
        }
    }

    /// Returns `true` for the root.
    pub fn is_root(&self) -> bool {
        self.len == 1
    }
}

/// What a path names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A regular file of the given size.
    File(usize),
    /// A directory.
    Directory,
}

/// Looks up what `path` names.
pub fn kind(path: &Path) -> Result<Kind, i64> {
    match sdk::file_size(path.as_str()) {
        Ok(size) => Ok(Kind::File(size)),
        Err(errno::EISDIR) => Ok(Kind::Directory),
        Err(errno::EINVAL) if path.is_root() => Ok(Kind::Directory),
        Err(error) => Err(error),
    }
}

/// Reads a whole file into `out` and returns its length.
pub fn read_file(path: &Path, out: &mut [u8]) -> Result<usize, i64> {
    let handle = sdk::open(path.as_str(), sdk::READ)?;
    let mut length = 0;
    let result = loop {
        if length == out.len() {
            break Ok(length);
        }
        match sdk::read(handle, &mut out[length..]) {
            Ok(0) => break Ok(length),
            Ok(count) => length += count,
            Err(error) => break Err(error),
        }
    };
    let _ = sdk::close(handle);
    result
}

/// Replaces a file's contents with `data`, creating it when missing.
pub fn write_file(path: &Path, data: &[u8]) -> Result<(), i64> {
    if data.len() > MAX_FILE {
        return Err(errno::ENOSPC);
    }
    match sdk::remove(path.as_str()) {
        Ok(()) | Err(errno::ENOENT) => {}
        Err(error) => return Err(error),
    }
    let handle = sdk::open(path.as_str(), sdk::WRITE | sdk::CREATE)?;
    let mut written = 0;
    let result = loop {
        if written == data.len() {
            break Ok(());
        }
        match sdk::write_file(handle, &data[written..]) {
            Ok(0) => break Err(errno::ENOSPC),
            Ok(count) => written += count,
            Err(error) => break Err(error),
        }
    };
    let _ = sdk::close(handle);
    result
}

/// One directory entry from a listing.
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    /// Entry name.
    pub name: &'a str,
    /// File or directory.
    pub directory: bool,
    /// Size in bytes (zero for directories).
    pub size: usize,
}

/// A directory listing held in a caller buffer.
pub struct Listing<'a> {
    text: &'a str,
}

impl<'a> Listing<'a> {
    /// Lists `path` into `buffer`.
    pub fn read(path: &Path, buffer: &'a mut [u8]) -> Result<Self, i64> {
        let length = sdk::list(path.as_str(), buffer)?;
        let text = core::str::from_utf8(&buffer[..length]).map_err(|_| errno::EIO)?;
        Ok(Self { text })
    }

    /// Iterates over the entries in directory order.
    pub fn entries(&self) -> impl Iterator<Item = Entry<'a>> + 'a {
        self.text.lines().filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?;
            let directory = fields.next()? == "d";
            let size = fields
                .next()
                .and_then(|size| size.parse().ok())
                .unwrap_or(0);
            Some(Entry {
                name,
                directory,
                size,
            })
        })
    }
}

/// Prints `program: subject: message` for an error code.
pub fn report(out: &mut Out, program: &str, subject: &str, error: i64) {
    use core::fmt::Write;
    let _ = if subject.is_empty() {
        writeln!(out, "{program}: {}", sdk::error_message(error))
    } else {
        writeln!(out, "{program}: {subject}: {}", sdk::error_message(error))
    };
}

/// Formats a byte count as `1.5K`, `12M`, and so on.
pub struct Human(pub u64);

impl fmt::Display for Human {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let units = ["B", "K", "M", "G", "T"];
        let mut value = self.0;
        let mut tenths = 0;
        let mut unit = 0;
        while value >= 1024 && unit < units.len() - 1 {
            tenths = (value % 1024) * 10 / 1024;
            value /= 1024;
            unit += 1;
        }
        // Format into a small buffer first so width and alignment apply.
        let mut text = Small::new();
        let _ = if unit == 0 {
            write!(text, "{value}{}", units[unit])
        } else if value < 10 {
            write!(text, "{value}.{tenths}{}", units[unit])
        } else {
            write!(text, "{value}{}", units[unit])
        };
        f.pad(text.as_str())
    }
}

/// A short formatted string on the stack.
struct Small {
    bytes: [u8; 32],
    len: usize,
}

impl Small {
    const fn new() -> Self {
        Self {
            bytes: [0; 32],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl fmt::Write for Small {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let count = text.len().min(self.bytes.len() - self.len);
        self.bytes[self.len..self.len + count].copy_from_slice(&text.as_bytes()[..count]);
        self.len += count;
        Ok(())
    }
}

/// Formats timer ticks as `H:MM:SS` or `D days, H:MM`.
pub struct Duration {
    /// Ticks.
    pub ticks: u64,
    /// Ticks per second.
    pub hz: u64,
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let seconds = self.ticks / self.hz.max(1);
        let (days, hours, minutes, seconds) = (
            seconds / 86_400,
            seconds / 3600 % 24,
            seconds / 60 % 60,
            seconds % 60,
        );
        if days > 0 {
            write!(
                f,
                "{days} day{}, {hours:02}:{minutes:02}",
                if days == 1 { "" } else { "s" }
            )
        } else {
            write!(f, "{hours:02}:{minutes:02}:{seconds:02}")
        }
    }
}

/// Matches `name` against a glob with `*` and `?`.
pub fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob(&pattern[1..], name) || (!name.is_empty() && glob(pattern, &name[1..]))
        }
        (Some(b'?'), Some(_)) => glob(&pattern[1..], &name[1..]),
        (Some(&expected), Some(&actual)) => expected == actual && glob(&pattern[1..], &name[1..]),
        _ => false,
    }
}

/// Parses a decimal count, accepting a leading `+` or `-`.
pub fn parse_number(text: &str) -> Option<i64> {
    let text = text.strip_prefix('+').unwrap_or(text);
    text.parse().ok()
}

/// A whole screen of output, written in one go so the terminal never paints a
/// half-drawn frame of a full-screen program.
pub struct Frame {
    buffer: [u8; 8192],
    len: usize,
}

impl Frame {
    /// An empty frame.
    pub const fn new() -> Self {
        Self {
            buffer: [0; 8192],
            len: 0,
        }
    }

    /// Appends bytes; anything past the buffer is dropped.
    pub fn bytes(&mut self, bytes: &[u8]) {
        let count = bytes.len().min(self.buffer.len() - self.len);
        self.buffer[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
    }

    /// Appends a string.
    pub fn str(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    /// Appends `count` spaces.
    pub fn pad(&mut self, count: usize) {
        for _ in 0..count {
            self.bytes(b" ");
        }
    }

    /// The bytes held.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Number of bytes held.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Writes the frame and empties it.
    pub fn flush(&mut self) {
        write_all(&self.buffer[..self.len]);
        self.len = 0;
    }
}

impl fmt::Write for Frame {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.str(text);
        Ok(())
    }
}

/// A decoded key from raw terminal input.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable byte.
    Char(u8),
    /// A control byte (`0x01` is Ctrl+A).
    Ctrl(u8),
    /// Enter.
    Enter,
    /// Backspace.
    Backspace,
    /// Forward delete.
    Delete,
    /// Tab.
    Tab,
    /// A lone Escape.
    Escape,
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A function key, one-based.
    Function(u8),
    /// An escape sequence this decoder does not know.
    Unknown,
}

/// Decodes the key at the start of `bytes` and returns it with the number of
/// bytes it used.
pub fn decode_key(bytes: &[u8]) -> (Key, usize) {
    let Some(&first) = bytes.first() else {
        return (Key::Unknown, 0);
    };
    match first {
        b'\r' | b'\n' => return (Key::Enter, 1),
        b'\t' => return (Key::Tab, 1),
        0x7f | 0x08 => return (Key::Backspace, 1),
        0x1b => {}
        0x00..=0x1f => return (Key::Ctrl(first), 1),
        _ => return (Key::Char(first), 1),
    }
    match bytes.get(1) {
        None => (Key::Escape, 1),
        Some(b'O') => {
            let key = match bytes.get(2) {
                Some(b'P') => Key::Function(1),
                Some(b'Q') => Key::Function(2),
                Some(b'R') => Key::Function(3),
                Some(b'S') => Key::Function(4),
                Some(b'H') => Key::Home,
                Some(b'F') => Key::End,
                _ => Key::Unknown,
            };
            (key, bytes.len().min(3))
        }
        Some(b'[') => {
            // CSI: parameters, then one final byte in 0x40..=0x7e.
            let mut index = 2;
            let mut number: u32 = 0;
            while let Some(&byte) = bytes.get(index) {
                if (0x40..=0x7e).contains(&byte) {
                    let key = match byte {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => Key::Right,
                        b'D' => Key::Left,
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        b'~' => match number {
                            1 | 7 => Key::Home,
                            3 => Key::Delete,
                            4 | 8 => Key::End,
                            5 => Key::PageUp,
                            6 => Key::PageDown,
                            15 => Key::Function(5),
                            17..=21 => Key::Function((number - 11) as u8),
                            23 | 24 => Key::Function((number - 12) as u8),
                            _ => Key::Unknown,
                        },
                        _ => Key::Unknown,
                    };
                    return (key, index + 1);
                }
                if byte.is_ascii_digit() {
                    number = number
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'));
                } else if byte != b';' {
                    break;
                }
                index += 1;
            }
            (Key::Unknown, index.min(bytes.len()))
        }
        // Alt+key arrives as Escape then the key; treat it as the key.
        Some(_) => (Key::Escape, 1),
    }
}

/// Splits raw input into keys.
pub fn keys(mut bytes: &[u8], mut each: impl FnMut(Key)) {
    while !bytes.is_empty() {
        let (key, used) = decode_key(bytes);
        each(key);
        bytes = &bytes[used.max(1)..];
    }
}

/// Puts the terminal in raw input mode and the alternate screen, and undoes
/// both when dropped.
pub struct FullScreen;

impl FullScreen {
    /// Enters full-screen mode.
    pub fn enter() -> Self {
        let _ = sdk::set_terminal_mode(aether_abi::user::TERMINAL_RAW);
        write_all(b"\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J");
        Self
    }
}

impl Drop for FullScreen {
    fn drop(&mut self) {
        write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = sdk::set_terminal_mode(0);
    }
}

/// Ticks per second of the system timer.
pub fn tick_hz() -> u64 {
    sdk::system_info().map_or(100, |info| u64::from(info.tick_hz.max(1)))
}

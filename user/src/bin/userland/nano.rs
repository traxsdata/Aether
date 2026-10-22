// SPDX-License-Identifier: Apache-2.0
//! `nano`: a small full-screen text editor modelled on GNU nano.
//!
//! The layout and keys follow nano: a title bar, the text, a status line, and
//! two rows of shortcuts. `^O` writes, `^X` exits (offering to save), `^K` and
//! `^U` cut and paste lines, `^W` searches, `^G` shows help, and `^C` reports
//! the cursor position. Files are at most 4 KiB, the volume's limit.

use crate::rt::{self, Args, Frame, FullScreen, Key, MAX_FILE, Path};
use aether_abi::user::errno;
use aether_sdk as sdk;
use core::fmt::Write as _;

/// Formats a status-line message.
macro_rules! say {
    ($editor:expr, ($($arg:tt)*)) => {{
        let mut text = Frame::new();
        let _ = write!(text, $($arg)*);
        $editor.set_status(text.as_bytes());
    }};
}

const TAB_WIDTH: usize = 8;

/// A key and its label in the shortcut bars.
type Shortcut = (&'static str, &'static str);
const PROMPT_BYTES: usize = 96;

/// What the bottom line is asking for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prompt {
    None,
    /// File name to write; `exit` closes the editor after a successful save.
    WriteName {
        exit: bool,
    },
    /// Save before exiting?
    SaveOnExit,
    /// Search text.
    Search,
    /// Line number to jump to.
    GoTo,
}

struct Editor {
    text: [u8; MAX_FILE],
    len: usize,
    cursor: usize,
    /// Column Up/Down try to keep.
    goal_column: usize,
    top_line: usize,
    left_column: usize,
    modified: bool,
    name: [u8; PROMPT_BYTES],
    name_len: usize,
    cwd: [u8; rt::MAX_PATH],
    cwd_len: usize,
    cut: [u8; MAX_FILE],
    cut_len: usize,
    last_was_cut: bool,
    status: [u8; 96],
    status_len: usize,
    prompt: Prompt,
    answer: [u8; PROMPT_BYTES],
    answer_len: usize,
    last_search: [u8; PROMPT_BYTES],
    last_search_len: usize,
    help: bool,
    columns: usize,
    lines: usize,
}

impl Editor {
    fn new(cwd: &str) -> Self {
        let (columns, lines) = sdk::terminal_size().map_or((80, 24), |(columns, rows)| {
            (usize::from(columns).max(20), usize::from(rows).max(6))
        });
        let mut editor = Self {
            text: [0; MAX_FILE],
            len: 0,
            cursor: 0,
            goal_column: 0,
            top_line: 0,
            left_column: 0,
            modified: false,
            name: [0; PROMPT_BYTES],
            name_len: 0,
            cwd: [0; rt::MAX_PATH],
            cwd_len: 0,
            cut: [0; MAX_FILE],
            cut_len: 0,
            last_was_cut: false,
            status: [0; 96],
            status_len: 0,
            prompt: Prompt::None,
            answer: [0; PROMPT_BYTES],
            answer_len: 0,
            last_search: [0; PROMPT_BYTES],
            last_search_len: 0,
            help: false,
            columns,
            lines,
        };
        let cwd = &cwd.as_bytes()[..cwd.len().min(rt::MAX_PATH)];
        editor.cwd[..cwd.len()].copy_from_slice(cwd);
        editor.cwd_len = cwd.len();
        editor
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }

    fn set_name(&mut self, name: &[u8]) {
        let length = name.len().min(PROMPT_BYTES);
        self.name[..length].copy_from_slice(&name[..length]);
        self.name_len = length;
    }

    fn path(&self) -> Option<Path> {
        let cwd = core::str::from_utf8(&self.cwd[..self.cwd_len]).unwrap_or("/");
        Path::resolve(cwd, self.name())
    }

    fn set_status(&mut self, text: &[u8]) {
        let length = text.len().min(self.status.len());
        self.status[..length].copy_from_slice(&text[..length]);
        self.status_len = length;
    }

    /// Loads the named file, if it exists.
    fn load(&mut self) {
        let Some(path) = self.path() else {
            say!(self, ("[ Path is too long ]"));
            return;
        };
        match rt::kind(&path) {
            Ok(rt::Kind::Directory) => {
                say!(self, ("[ \"{}\" is a directory ]", self.name()));
                self.name_len = 0;
            }
            Ok(rt::Kind::File(_)) => match rt::read_file(&path, &mut self.text) {
                Ok(length) => {
                    self.len = length;
                    let lines = self.text[..length]
                        .iter()
                        .filter(|&&byte| byte == b'\n')
                        .count();
                    say!(
                        self,
                        ("[ Read {lines} line{} ]", if lines == 1 { "" } else { "s" })
                    );
                }
                Err(error) => say!(
                    self,
                    (
                        "[ Error reading {}: {} ]",
                        self.name(),
                        sdk::error_message(error)
                    )
                ),
            },
            Err(errno::ENOENT) => say!(self, ("[ New File ]")),
            Err(error) => say!(self, ("[ {} ]", sdk::error_message(error))),
        }
    }

    /// Writes the buffer to the named file.
    fn save(&mut self) -> bool {
        let Some(path) = self.path() else {
            say!(self, ("[ Path is too long ]"));
            return false;
        };
        match rt::write_file(&path, &self.text[..self.len]) {
            Ok(()) => {
                self.modified = false;
                let lines = self.line_count()
                    - usize::from(self.len == 0 || self.text[self.len - 1] == b'\n');
                say!(
                    self,
                    (
                        "[ Wrote {lines} line{} ]",
                        if lines == 1 { "" } else { "s" }
                    )
                );
                true
            }
            Err(error) => {
                let reason = if error == errno::EACCES {
                    "only your home folder is writable"
                } else {
                    sdk::error_message(error)
                };
                say!(self, ("[ Error writing {}: {reason} ]", self.name()));
                false
            }
        }
    }

    // --- Text geometry -------------------------------------------------

    fn line_start(&self, position: usize) -> usize {
        self.text[..position]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |index| index + 1)
    }

    fn line_end(&self, position: usize) -> usize {
        self.text[position..self.len]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(self.len, |index| position + index)
    }

    fn line_of(&self, position: usize) -> usize {
        self.text[..position]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count()
    }

    fn line_count(&self) -> usize {
        self.line_of(self.len) + 1
    }

    /// Start of line `line` (clamped to the last line).
    fn start_of_line(&self, line: usize) -> usize {
        let mut position = 0;
        for _ in 0..line {
            let end = self.line_end(position);
            if end == self.len {
                break;
            }
            position = end + 1;
        }
        position
    }

    /// Display column of `position` within its line.
    fn column_of(&self, position: usize) -> usize {
        let mut column = 0;
        for &byte in &self.text[self.line_start(position)..position] {
            column = advance(column, byte);
        }
        column
    }

    /// Position on the line starting at `start` closest to display `column`.
    fn position_at(&self, start: usize, column: usize) -> usize {
        let end = self.line_end(start);
        let mut current = 0;
        for position in start..end {
            let next = advance(current, self.text[position]);
            if next > column {
                return position;
            }
            current = next;
        }
        end
    }

    // --- Editing -------------------------------------------------------

    fn insert(&mut self, bytes: &[u8]) -> bool {
        if self.len + bytes.len() > MAX_FILE {
            say!(
                self,
                ("[ The file is full: 4 KiB is the most a file can hold ]")
            );
            return false;
        }
        self.text
            .copy_within(self.cursor..self.len, self.cursor + bytes.len());
        self.text[self.cursor..self.cursor + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        self.cursor += bytes.len();
        self.modified = true;
        true
    }

    fn delete_range(&mut self, start: usize, end: usize) {
        self.text.copy_within(end..self.len, start);
        self.len -= end - start;
        if self.cursor > start {
            self.cursor = self.cursor.saturating_sub(end - start).max(start);
        }
        self.modified = true;
    }

    fn cut_line(&mut self) {
        let start = self.line_start(self.cursor);
        let mut end = self.line_end(self.cursor);
        if end < self.len {
            end += 1;
        }
        if start == end {
            return;
        }
        if !self.last_was_cut {
            self.cut_len = 0;
        }
        let count = (end - start).min(MAX_FILE - self.cut_len);
        self.cut[self.cut_len..self.cut_len + count]
            .copy_from_slice(&self.text[start..start + count]);
        self.cut_len += count;
        self.cursor = start;
        self.delete_range(start, end);
    }

    fn paste(&mut self) {
        if self.cut_len == 0 {
            say!(self, ("[ The cutbuffer is empty ]"));
            return;
        }
        let mut cut = [0u8; MAX_FILE];
        cut[..self.cut_len].copy_from_slice(&self.cut[..self.cut_len]);
        let _ = self.insert(&cut[..self.cut_len]);
    }

    fn vertical(&mut self, delta: isize) {
        let line = self.line_of(self.cursor) as isize;
        let target = (line + delta).clamp(0, self.line_count() as isize - 1) as usize;
        let start = self.start_of_line(target);
        self.cursor = self.position_at(start, self.goal_column);
    }

    fn search(&mut self) {
        let needle_len = self.last_search_len;
        if needle_len == 0 {
            return;
        }
        let needle = self.last_search;
        let needle = &needle[..needle_len];
        let found = (self.cursor + 1..=self.len)
            .chain(0..=self.cursor.min(self.len))
            .find(|&start| self.text[start..self.len].starts_with(needle));
        match found {
            Some(position) => {
                if position <= self.cursor {
                    say!(self, ("[ Search Wrapped ]"));
                }
                self.cursor = position;
            }
            None => {
                let text = core::str::from_utf8(needle).unwrap_or("");
                say!(self, ("[ \"{text}\" not found ]"));
            }
        }
    }

    fn report_position(&mut self) {
        let line = self.line_of(self.cursor) + 1;
        let lines = self.line_count();
        let column = self.column_of(self.cursor) + 1;
        let columns = self.column_of(self.line_end(self.cursor)) + 1;
        say!(
            self,
            (
                "[ line {line}/{lines} ({}%), col {column}/{columns} ({}%), char {}/{} ({}%) ]",
                line * 100 / lines,
                column * 100 / columns,
                self.cursor,
                self.len,
                self.cursor * 100 / self.len.max(1)
            )
        );
    }

    fn open_prompt(&mut self, prompt: Prompt) {
        self.prompt = prompt;
        self.answer_len = 0;
        if let Prompt::WriteName { .. } = prompt {
            let name = self.name;
            self.answer[..self.name_len].copy_from_slice(&name[..self.name_len]);
            self.answer_len = self.name_len;
        }
    }

    /// Handles a key while a prompt is open; returns `false` to exit.
    fn prompt_key(&mut self, key: Key) -> bool {
        let prompt = self.prompt;
        if prompt == Prompt::SaveOnExit {
            match key {
                Key::Char(b'y' | b'Y') => {
                    if self.name_len == 0 {
                        self.open_prompt(Prompt::WriteName { exit: true });
                        return true;
                    }
                    self.prompt = Prompt::None;
                    return !self.save();
                }
                Key::Char(b'n' | b'N') => return false,
                Key::Ctrl(0x03) => {
                    self.prompt = Prompt::None;
                    say!(self, ("[ Cancelled ]"));
                }
                _ => {}
            }
            return true;
        }
        match key {
            Key::Ctrl(0x03) | Key::Escape => {
                self.prompt = Prompt::None;
                say!(self, ("[ Cancelled ]"));
            }
            Key::Backspace => self.answer_len = self.answer_len.saturating_sub(1),
            Key::Char(byte) if self.answer_len < PROMPT_BYTES => {
                self.answer[self.answer_len] = byte;
                self.answer_len += 1;
            }
            Key::Enter => {
                self.prompt = Prompt::None;
                let answer = self.answer;
                let answer = &answer[..self.answer_len];
                match prompt {
                    Prompt::WriteName { exit } => {
                        if answer.is_empty() {
                            say!(self, ("[ Cancelled ]"));
                            return true;
                        }
                        self.set_name(answer);
                        if self.save() && exit {
                            return false;
                        }
                    }
                    Prompt::Search => {
                        if !answer.is_empty() {
                            self.last_search[..answer.len()].copy_from_slice(answer);
                            self.last_search_len = answer.len();
                        }
                        self.search();
                    }
                    Prompt::GoTo => {
                        let line = core::str::from_utf8(answer)
                            .ok()
                            .and_then(|text| text.trim().parse::<usize>().ok());
                        match line {
                            Some(line) if line > 0 => {
                                self.cursor = self.start_of_line(line - 1);
                            }
                            _ => say!(self, ("[ Invalid line number ]")),
                        }
                    }
                    Prompt::None | Prompt::SaveOnExit => {}
                }
            }
            _ => {}
        }
        true
    }

    /// Handles a key; returns `false` to exit.
    fn key(&mut self, key: Key) -> bool {
        if self.help {
            self.help = false;
            return true;
        }
        if self.prompt != Prompt::None {
            return self.prompt_key(key);
        }
        self.status_len = 0;
        let was_cut = self.last_was_cut;
        self.last_was_cut = false;
        let page = self.text_rows().saturating_sub(2).max(1) as isize;
        let mut keep_goal = false;
        match key {
            Key::Char(byte) => {
                let _ = self.insert(&[byte]);
            }
            Key::Tab => {
                let _ = self.insert(b"\t");
            }
            Key::Enter => {
                let _ = self.insert(b"\n");
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.delete_range(self.cursor - 1, self.cursor);
                }
            }
            Key::Delete | Key::Ctrl(0x04) => {
                if self.cursor < self.len {
                    self.delete_range(self.cursor, self.cursor + 1);
                }
            }
            Key::Left | Key::Ctrl(0x02) => self.cursor = self.cursor.saturating_sub(1),
            Key::Right | Key::Ctrl(0x06) => self.cursor = (self.cursor + 1).min(self.len),
            Key::Up | Key::Ctrl(0x10) => {
                self.vertical(-1);
                keep_goal = true;
            }
            Key::Down | Key::Ctrl(0x0e) => {
                self.vertical(1);
                keep_goal = true;
            }
            Key::PageUp | Key::Ctrl(0x19) => {
                self.vertical(-page);
                self.top_line = self.top_line.saturating_sub(page as usize);
                keep_goal = true;
            }
            Key::PageDown | Key::Ctrl(0x16) => {
                self.vertical(page);
                self.top_line += page as usize;
                keep_goal = true;
            }
            Key::Home | Key::Ctrl(0x01) => self.cursor = self.line_start(self.cursor),
            Key::End | Key::Ctrl(0x05) => self.cursor = self.line_end(self.cursor),
            Key::Ctrl(0x0b) => {
                self.last_was_cut = was_cut;
                self.cut_line();
                self.last_was_cut = true;
            }
            Key::Ctrl(0x15) => self.paste(),
            Key::Ctrl(0x0f) => self.open_prompt(Prompt::WriteName { exit: false }),
            Key::Ctrl(0x13) => {
                if self.name_len == 0 {
                    self.open_prompt(Prompt::WriteName { exit: false });
                } else {
                    let _ = self.save();
                }
            }
            Key::Ctrl(0x18) | Key::Function(2) => {
                if !self.modified {
                    return false;
                }
                self.open_prompt(Prompt::SaveOnExit);
            }
            Key::Ctrl(0x17) | Key::Function(6) => self.open_prompt(Prompt::Search),
            Key::Ctrl(0x1f) | Key::Function(4) => self.open_prompt(Prompt::GoTo),
            Key::Ctrl(0x07) | Key::Function(1) => self.help = true,
            Key::Ctrl(0x03) => self.report_position(),
            _ => {}
        }
        if !keep_goal {
            self.goal_column = self.column_of(self.cursor);
        }
        true
    }

    // --- Drawing -------------------------------------------------------

    fn text_rows(&self) -> usize {
        self.lines.saturating_sub(4)
    }

    fn draw(&mut self, frame: &mut Frame) {
        frame.str("\x1b[H\x1b[0m");
        if self.help {
            draw_help(frame);
            return;
        }
        // Title bar.
        let title = if self.name_len == 0 {
            "New Buffer"
        } else {
            self.name()
        };
        let left = "  Aether nano 1.0";
        let right = if self.modified { "Modified  " } else { "" };
        let middle = self.columns.saturating_sub(left.len() + right.len());
        let title = &title[..title.len().min(middle.saturating_sub(2))];
        let before = middle.saturating_sub(title.len()) / 2;
        frame.str("\x1b[7m");
        frame.str(left);
        frame.pad(before);
        frame.str(title);
        frame.pad(middle - before - title.len());
        frame.str(right);
        frame.str("\x1b[0m\r\n");
        // Keep the cursor on screen.
        let rows = self.text_rows();
        let cursor_line = self.line_of(self.cursor);
        let last_top = self.line_count().saturating_sub(1);
        self.top_line = self.top_line.min(last_top);
        if cursor_line < self.top_line {
            self.top_line = cursor_line;
        }
        if cursor_line >= self.top_line + rows {
            self.top_line = cursor_line + 1 - rows;
        }
        let cursor_column = self.column_of(self.cursor);
        if cursor_column < self.left_column {
            self.left_column = cursor_column;
        }
        if cursor_column >= self.left_column + self.columns - 1 {
            self.left_column = cursor_column + 2 - self.columns;
        }
        // Text.
        let mut position = self.start_of_line(self.top_line);
        let mut line = self.top_line;
        let past_end = |line: usize| line >= self.line_count();
        for _ in 0..rows {
            if past_end(line) {
                frame.str("\x1b[K\r\n");
                continue;
            }
            let end = self.line_end(position);
            let mut column = 0;
            for &byte in &self.text[position..end] {
                let next = advance(column, byte);
                for shown in column..next {
                    if shown >= self.left_column && shown < self.left_column + self.columns {
                        let glyph = match byte {
                            b'\t' => b' ',
                            0x20..=0x7e => byte,
                            _ => b'?',
                        };
                        frame.bytes(&[glyph]);
                    }
                }
                column = next;
            }
            frame.str("\x1b[K\r\n");
            position = (end + 1).min(self.len);
            line += 1;
        }
        // Status line.
        match self.prompt {
            Prompt::None => {
                if self.status_len > 0 {
                    let status =
                        core::str::from_utf8(&self.status[..self.status_len]).unwrap_or("");
                    let status = &status[..status.len().min(self.columns)];
                    let indent = self.columns.saturating_sub(status.len()) / 2;
                    frame.pad(indent);
                    let _ = write!(frame, "\x1b[7m{status}\x1b[0m");
                }
                frame.str("\x1b[K\r\n");
            }
            Prompt::SaveOnExit => {
                frame.str("\x1b[7mSave modified buffer?  \x1b[0m\x1b[K\r\n");
            }
            prompt => {
                let label = match prompt {
                    Prompt::WriteName { .. } => "File Name to Write: ",
                    Prompt::Search => "Search: ",
                    _ => "Enter line number: ",
                };
                let answer = core::str::from_utf8(&self.answer[..self.answer_len]).unwrap_or("");
                frame.str("\x1b[7m");
                frame.str(label);
                frame.str(answer);
                frame.pad(self.columns.saturating_sub(label.len() + answer.len()));
                frame.str("\x1b[0m\r\n");
            }
        }
        // Shortcut bars.
        let (top, bottom): (&[Shortcut], &[Shortcut]) = match self.prompt {
            Prompt::SaveOnExit => (&[(" Y", "Yes")], &[(" N", "No"), ("^C", "Cancel")]),
            Prompt::None => (
                &[
                    ("^G", "Help"),
                    ("^O", "Write Out"),
                    ("^W", "Where Is"),
                    ("^K", "Cut"),
                    ("^C", "Location"),
                ],
                &[
                    ("^X", "Exit"),
                    ("^S", "Save"),
                    ("^_", "Go To Line"),
                    ("^U", "Paste"),
                    ("^Y", "Prev Page"),
                ],
            ),
            _ => (&[("^G", "Help")], &[("^C", "Cancel")]),
        };
        self.shortcuts(frame, top);
        frame.str("\r\n");
        self.shortcuts(frame, bottom);
        // Cursor.
        let screen_row = 2 + cursor_line - self.top_line;
        let screen_column = 1 + cursor_column - self.left_column;
        match self.prompt {
            Prompt::None => {
                let _ = write!(frame, "\x1b[{screen_row};{screen_column}H\x1b[?25h");
            }
            Prompt::SaveOnExit => frame.str("\x1b[?25l"),
            prompt => {
                let label = match prompt {
                    Prompt::WriteName { .. } => 20,
                    Prompt::Search => 8,
                    _ => 19,
                };
                let column = (label + self.answer_len + 1).min(self.columns);
                let _ = write!(frame, "\x1b[{};{column}H\x1b[?25h", self.lines - 2);
            }
        }
    }

    fn shortcuts(&self, frame: &mut Frame, items: &[Shortcut]) {
        let width = (self.columns / 5).max(12);
        let mut used = 0;
        for (key, label) in items {
            if used + width > self.columns {
                break;
            }
            let _ = write!(frame, "\x1b[7m{key}\x1b[0m ");
            let label = &label[..label.len().min(width - key.len() - 1)];
            frame.str(label);
            frame.pad(width - key.len() - 1 - label.len());
            used += width;
        }
        frame.str("\x1b[K");
    }
}

/// Display column after drawing `byte` at `column`.
fn advance(column: usize, byte: u8) -> usize {
    if byte == b'\t' {
        (column / TAB_WIDTH + 1) * TAB_WIDTH
    } else {
        column + 1
    }
}

fn draw_help(frame: &mut Frame) {
    frame.str(
        "\x1b[7m  Aether nano 1.0                  Help                                    \x1b[0m\r\n\x1b[K\r\n\
         Type to insert text; the arrow keys, Home, End, PgUp and PgDn move.\x1b[K\r\n\
         Commands use Ctrl (shown as ^):\x1b[K\r\n\x1b[K\r\n\
         \x1b[1m ^O\x1b[0m  Write the file out (asks for a name)\x1b[K\r\n\
         \x1b[1m ^S\x1b[0m  Save under the current name\x1b[K\r\n\
         \x1b[1m ^X\x1b[0m  Exit, offering to save changes\x1b[K\r\n\
         \x1b[1m ^K\x1b[0m  Cut the current line (repeat to cut several)\x1b[K\r\n\
         \x1b[1m ^U\x1b[0m  Paste the cut lines\x1b[K\r\n\
         \x1b[1m ^W\x1b[0m  Search forward; Enter on an empty search repeats it\x1b[K\r\n\
         \x1b[1m ^_\x1b[0m  Go to a line number\x1b[K\r\n\
         \x1b[1m ^C\x1b[0m  Show the cursor position\x1b[K\r\n\
         \x1b[1m ^A ^E\x1b[0m  Start and end of line\x1b[K\r\n\
         \x1b[1m ^Y ^V\x1b[0m  Previous and next page\x1b[K\r\n\
         \x1b[1m ^D\x1b[0m  Delete the character under the cursor\x1b[K\r\n\x1b[K\r\n\
         Files hold at most 4 KiB, and only your home folder is writable.\x1b[K\r\n\x1b[K\r\n\
         Press any key to return to the file.\x1b[J\x1b[?25l",
    );
}

pub fn main(args: &Args) -> i64 {
    let (stdin_terminal, stdout_terminal) = rt::stdio_terminal();
    if !stdin_terminal || !stdout_terminal {
        rt::write_all(b"nano: standard input and output must be the terminal\n");
        return 1;
    }
    let mut editor = Editor::new(args.cwd());
    if let Some(name) = args
        .iter()
        .find(|arg| !arg.starts_with('-') && !arg.starts_with('+'))
    {
        editor.set_name(name.as_bytes());
        editor.load();
    }
    // `nano +LINE file` starts on that line.
    if let Some(line) = args.iter().find_map(|arg| {
        arg.strip_prefix('+')
            .and_then(|line| line.parse::<usize>().ok())
    }) {
        editor.cursor = editor.start_of_line(line.saturating_sub(1));
    }
    let _screen = FullScreen::enter();
    let mut frame = Frame::new();
    loop {
        editor.draw(&mut frame);
        frame.flush();
        let mut input = [0u8; 64];
        let count = rt::read_raw(&mut input);
        if count == 0 {
            return 0;
        }
        let mut running = true;
        rt::keys(&input[..count], |key| {
            if running && !editor.key(key) {
                running = false;
            }
        });
        if !running {
            return 0;
        }
    }
}

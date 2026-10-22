// SPDX-License-Identifier: Apache-2.0
//! Allocation-free terminal emulator for the Aether Terminal window.
//!
//! The emulator turns a byte stream from ring-3 programs into a fixed grid of
//! character cells. It understands the subset of ECMA-48 / VT100 / xterm
//! control sequences that ordinary command-line programs and full-screen
//! tools (`htop`, `nano`) use: cursor movement, erase, insert and delete,
//! scroll regions, 16-colour and bright SGR attributes, cursor visibility,
//! the alternate screen, and cursor-position reports. Anything else is parsed
//! and ignored, so a hostile stream can only change the grid, never escape
//! it.
//!
//! The crate has no hardware access and no authority. The kernel feeds it
//! bytes, reads cells back to paint them, and forwards encoded keys to the
//! session's input queue.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod font;

use aether_input::{Key, Modifiers};

/// Grid width in cells.
pub const COLS: usize = 80;
/// Grid height in cells.
pub const ROWS: usize = 24;
/// Cell width in pixels.
pub const CELL_WIDTH: usize = 8;
/// Cell height in pixels.
pub const CELL_HEIGHT: usize = 16;
/// Colour index meaning "the default foreground or background".
pub const DEFAULT_COLOR: u8 = 16;

/// Bold (bright) text.
pub const ATTR_BOLD: u8 = 1 << 0;
/// Swap foreground and background.
pub const ATTR_INVERSE: u8 = 1 << 1;
/// Underlined text.
pub const ATTR_UNDERLINE: u8 = 1 << 2;
/// Dim text.
pub const ATTR_DIM: u8 = 1 << 3;

/// Default foreground colour (`0x00RRGGBB`).
pub const DEFAULT_FG: u32 = 0x00d8_dee9;
/// Default background colour.
pub const DEFAULT_BG: u32 = 0x0012_1620;

/// The 16-colour palette: eight normal colours, then eight bright ones.
pub const PALETTE: [u32; 16] = [
    0x001c_2030, // black
    0x00e0_6c75, // red
    0x0098_c379, // green
    0x00e5_c07b, // yellow
    0x0061_afef, // blue
    0x00c6_78dd, // magenta
    0x0056_b6c2, // cyan
    0x00c8_ccd4, // white
    0x005c_6370, // bright black
    0x00ff_7a85, // bright red
    0x00b5_e890, // bright green
    0x00ff_d68a, // bright yellow
    0x0082_c4ff, // bright blue
    0x00e0_9bf5, // bright magenta
    0x0070_d6e0, // bright cyan
    0x00ff_ffff, // bright white
];

/// Unicode code points with a glyph above ASCII, in font order.
const EXTRA_GLYPHS: [u32; 18] = [
    0x2588, 0x2591, 0x2592, 0x2593, 0x2500, 0x2502, 0x250C, 0x2510, 0x2514, 0x2518, 0x251C, 0x2524,
    0x252C, 0x2534, 0x253C, 0x25B2, 0x25BC, 0x00B7,
];

const _: () = assert!(font::GLYPH_COUNT == 95 + EXTRA_GLYPHS.len());

/// Internal code for a glyph that is not in the font.
pub const REPLACEMENT: u8 = b'?';

/// Maps a Unicode scalar to the byte stored in a cell.
///
/// Printable ASCII maps to itself; the supported block and box-drawing
/// characters map to codes from 128. Anything else becomes `?`.
#[must_use]
pub fn glyph_code(scalar: u32) -> u8 {
    if (0x20..0x7f).contains(&scalar) {
        return scalar as u8;
    }
    match EXTRA_GLYPHS.iter().position(|&code| code == scalar) {
        Some(index) => 128 + index as u8,
        None => REPLACEMENT,
    }
}

/// Returns the 8x16 bitmap for a cell code.
#[must_use]
pub fn glyph(code: u8) -> &'static [u8; 16] {
    let index = match code {
        0x20..=0x7e => usize::from(code - 0x20),
        128.. if usize::from(code - 128) < EXTRA_GLYPHS.len() => 95 + usize::from(code - 128),
        _ => usize::from(REPLACEMENT - 0x20),
    };
    &font::GLYPHS[index]
}

/// One character cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Glyph code (see [`glyph_code`]).
    pub code: u8,
    /// Foreground palette index, or [`DEFAULT_COLOR`].
    pub fg: u8,
    /// Background palette index, or [`DEFAULT_COLOR`].
    pub bg: u8,
    /// `ATTR_*` bits.
    pub attr: u8,
}

impl Cell {
    /// An empty cell with default colours.
    pub const BLANK: Self = Self {
        code: b' ',
        fg: DEFAULT_COLOR,
        bg: DEFAULT_COLOR,
        attr: 0,
    };

    /// Resolves the cell's colours to `(foreground, background)` RGB values.
    #[must_use]
    pub fn colors(self) -> (u32, u32) {
        let mut fg = match self.fg {
            index @ 0..=7 if self.attr & ATTR_BOLD != 0 => PALETTE[usize::from(index) + 8],
            index @ 0..=15 => PALETTE[usize::from(index)],
            _ => DEFAULT_FG,
        };
        let mut bg = match self.bg {
            index @ 0..=15 => PALETTE[usize::from(index)],
            _ => DEFAULT_BG,
        };
        if self.attr & ATTR_DIM != 0 {
            fg = dim(fg);
        }
        if self.attr & ATTR_INVERSE != 0 {
            core::mem::swap(&mut fg, &mut bg);
        }
        (fg, bg)
    }
}

fn dim(rgb: u32) -> u32 {
    let channel = |shift: u32| (((rgb >> shift) & 0xff) * 2 / 3) << shift;
    channel(16) | channel(8) | channel(0)
}

#[derive(Clone, Copy)]
struct Grid {
    cells: [[Cell; COLS]; ROWS],
}

impl Grid {
    const BLANK: Self = Self {
        cells: [[Cell::BLANK; COLS]; ROWS],
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    /// `ESC (` or `ESC )`: the next byte names a character set and is ignored.
    Charset,
    Csi,
    /// Operating-system command, ignored until BEL or `ESC \`.
    Osc,
    OscEscape,
}

const MAX_PARAMS: usize = 8;
/// Longest operating-system command kept open. Titles and hyperlinks fit;
/// a program that never terminates one cannot swallow the screen.
const MAX_OSC_BYTES: u16 = 512;
/// Bytes of pending replies (cursor-position reports).
const REPLY_BYTES: usize = 32;

#[derive(Clone, Copy)]
struct Saved {
    x: usize,
    y: usize,
    fg: u8,
    bg: u8,
    attr: u8,
}

/// The terminal state machine and its screens.
#[derive(Clone)]
pub struct Terminal {
    main: Grid,
    alt: Grid,
    using_alt: bool,
    x: usize,
    y: usize,
    /// The cursor sits past the last column; the next printable wraps first.
    wrap_pending: bool,
    fg: u8,
    bg: u8,
    attr: u8,
    saved: Saved,
    alt_saved: Saved,
    cursor_visible: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    state: State,
    params: [u16; MAX_PARAMS],
    param_count: usize,
    private: bool,
    utf8_scalar: u32,
    utf8_remaining: u8,
    /// Bytes consumed by the current operating-system command.
    osc_len: u16,
    reply: [u8; REPLY_BYTES],
    reply_len: usize,
    generation: u64,
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminal {
    /// Creates a blank terminal with the cursor at the top left.
    #[must_use]
    pub const fn new() -> Self {
        const ORIGIN: Saved = Saved {
            x: 0,
            y: 0,
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            attr: 0,
        };
        Self {
            main: Grid::BLANK,
            alt: Grid::BLANK,
            using_alt: false,
            x: 0,
            y: 0,
            wrap_pending: false,
            fg: DEFAULT_COLOR,
            bg: DEFAULT_COLOR,
            attr: 0,
            saved: ORIGIN,
            alt_saved: ORIGIN,
            cursor_visible: true,
            scroll_top: 0,
            scroll_bottom: ROWS - 1,
            state: State::Ground,
            params: [0; MAX_PARAMS],
            param_count: 0,
            private: false,
            utf8_scalar: 0,
            utf8_remaining: 0,
            osc_len: 0,
            reply: [0; REPLY_BYTES],
            reply_len: 0,
            generation: 0,
        }
    }

    /// Resets every screen, attribute, and parser state.
    pub fn reset(&mut self) {
        let generation = self.generation;
        *self = Self::new();
        self.generation = generation.wrapping_add(1);
    }

    /// Counter bumped by every change that affects the painted grid.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the cell at `(column, row)`.
    #[must_use]
    pub fn cell(&self, column: usize, row: usize) -> Cell {
        self.grid().cells[row.min(ROWS - 1)][column.min(COLS - 1)]
    }

    /// Returns the cursor position as `(column, row)`.
    #[must_use]
    pub const fn cursor(&self) -> (usize, usize) {
        (self.x, self.y)
    }

    /// Returns `true` when the program has not hidden the cursor.
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Returns `true` while the alternate screen is shown.
    #[must_use]
    pub const fn alternate_screen(&self) -> bool {
        self.using_alt
    }

    /// Copies one row's text into `out` without trailing blanks and returns
    /// its length. Glyphs above ASCII are written as `#`.
    pub fn row_text(&self, row: usize, out: &mut [u8; COLS]) -> usize {
        let cells = &self.grid().cells[row.min(ROWS - 1)];
        let mut length = 0;
        for (index, cell) in cells.iter().enumerate() {
            out[index] = if cell.code < 128 { cell.code } else { b'#' };
            if cell.code != b' ' {
                length = index + 1;
            }
        }
        length
    }

    /// Removes pending replies (for example a cursor-position report) that
    /// must be delivered to the program as input.
    pub fn take_reply(&mut self, out: &mut [u8]) -> usize {
        let count = self.reply_len.min(out.len());
        out[..count].copy_from_slice(&self.reply[..count]);
        self.reply.copy_within(count..self.reply_len, 0);
        self.reply_len -= count;
        count
    }

    /// Feeds program output into the emulator.
    pub fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn grid(&self) -> &Grid {
        if self.using_alt {
            &self.alt
        } else {
            &self.main
        }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        if self.using_alt {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn blank(&self) -> Cell {
        // Erased cells take the current background, as xterm does.
        Cell {
            bg: self.bg,
            ..Cell::BLANK
        }
    }

    fn byte(&mut self, byte: u8) {
        match self.state {
            State::Ground => self.ground(byte),
            State::Escape => self.escape(byte),
            State::Charset => self.state = State::Ground,
            State::Csi => self.csi(byte),
            State::Osc => match byte {
                0x07 => self.state = State::Ground,
                0x1b => self.state = State::OscEscape,
                // CAN and SUB cancel any sequence, as on a VT100.
                0x18 | 0x1a => self.state = State::Ground,
                _ => {
                    self.osc_len += 1;
                    if self.osc_len > MAX_OSC_BYTES {
                        self.state = State::Ground;
                    }
                }
            },
            State::OscEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                } else {
                    // ESC ends the command and begins a new sequence.
                    self.state = State::Escape;
                    self.escape(byte);
                }
            }
        }
    }

    fn ground(&mut self, byte: u8) {
        if self.utf8_remaining > 0 {
            if byte & 0xc0 == 0x80 {
                self.utf8_scalar = (self.utf8_scalar << 6) | u32::from(byte & 0x3f);
                self.utf8_remaining -= 1;
                if self.utf8_remaining == 0 {
                    self.print(glyph_code(self.utf8_scalar));
                }
                return;
            }
            // A truncated sequence prints one replacement, then the byte is
            // handled on its own.
            self.utf8_remaining = 0;
            self.print(REPLACEMENT);
        }
        match byte {
            0x1b => {
                self.state = State::Escape;
            }
            b'\r' => {
                self.x = 0;
                self.wrap_pending = false;
            }
            // Programs write bare `\n`; there is no tty layer to translate
            // it, so the terminal performs ONLCR itself.
            b'\n' | 0x0b | 0x0c => {
                self.x = 0;
                self.wrap_pending = false;
                self.line_feed();
            }
            0x08 => {
                self.x = self.x.saturating_sub(1);
                self.wrap_pending = false;
            }
            b'\t' => {
                self.x = ((self.x / 8) + 1) * 8;
                if self.x >= COLS {
                    self.x = COLS - 1;
                }
                self.wrap_pending = false;
            }
            0x20..=0x7e => self.print(byte),
            0xc2..=0xdf => self.utf8_start(u32::from(byte & 0x1f), 1),
            0xe0..=0xef => self.utf8_start(u32::from(byte & 0x0f), 2),
            0xf0..=0xf4 => self.utf8_start(u32::from(byte & 0x07), 3),
            0x80..=0xff => self.print(REPLACEMENT),
            _ => {}
        }
    }

    fn utf8_start(&mut self, bits: u32, remaining: u8) {
        self.utf8_scalar = bits;
        self.utf8_remaining = remaining;
    }

    fn print(&mut self, code: u8) {
        if self.wrap_pending {
            self.x = 0;
            self.wrap_pending = false;
            self.line_feed();
        }
        let cell = Cell {
            code,
            fg: self.fg,
            bg: self.bg,
            attr: self.attr,
        };
        let (x, y) = (self.x, self.y);
        self.grid_mut().cells[y][x] = cell;
        if self.x + 1 >= COLS {
            self.wrap_pending = true;
        } else {
            self.x += 1;
        }
    }

    fn line_feed(&mut self) {
        if self.y == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.y + 1 < ROWS {
            self.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        if self.y == self.scroll_top {
            self.scroll_down(1);
        } else {
            self.y = self.y.saturating_sub(1);
        }
    }

    /// Scrolls the scroll region up by `count` lines.
    fn scroll_up(&mut self, count: usize) {
        self.scroll_region_up(self.scroll_top, count);
    }

    fn scroll_region_up(&mut self, top: usize, count: usize) {
        let bottom = self.scroll_bottom;
        if top > bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        let blank = self.blank();
        let grid = self.grid_mut();
        grid.cells.copy_within(top + count..=bottom, top);
        for row in &mut grid.cells[bottom + 1 - count..=bottom] {
            *row = [blank; COLS];
        }
    }

    fn scroll_down(&mut self, count: usize) {
        self.scroll_region_down(self.scroll_top, count);
    }

    fn scroll_region_down(&mut self, top: usize, count: usize) {
        let bottom = self.scroll_bottom;
        if top > bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        let blank = self.blank();
        let grid = self.grid_mut();
        if count <= bottom - top {
            grid.cells.copy_within(top..=bottom - count, top + count);
        }
        for row in &mut grid.cells[top..top + count] {
            *row = [blank; COLS];
        }
    }

    fn escape(&mut self, byte: u8) {
        self.state = State::Ground;
        match byte {
            b'[' => {
                self.state = State::Csi;
                self.params = [0; MAX_PARAMS];
                self.param_count = 0;
                self.private = false;
            }
            b']' => {
                self.state = State::Osc;
                self.osc_len = 0;
            }
            b'(' | b')' => self.state = State::Charset,
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.line_feed(),
            b'E' => {
                self.x = 0;
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            b'c' => self.reset(),
            _ => {}
        }
    }

    fn csi(&mut self, byte: u8) {
        match byte {
            b'0'..=b'9' => {
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                let slot = &mut self.params[self.param_count - 1];
                *slot = slot
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
            }
            b';' => {
                if self.param_count == 0 {
                    self.param_count = 1;
                }
                if self.param_count < MAX_PARAMS {
                    self.param_count += 1;
                }
            }
            b'?' | b'>' | b'=' => self.private = true,
            0x40..=0x7e => {
                self.state = State::Ground;
                self.dispatch(byte);
            }
            // A control byte inside a sequence is executed, as VT100 does.
            0x1b => self.state = State::Escape,
            _ => {}
        }
    }

    /// Returns parameter `index`, treating zero or a missing value as
    /// `default`.
    fn param(&self, index: usize, default: usize) -> usize {
        match self.params.get(index) {
            Some(&value) if index < self.param_count && value != 0 => usize::from(value),
            _ => default,
        }
    }

    fn dispatch(&mut self, final_byte: u8) {
        self.wrap_pending = false;
        if self.private {
            self.private_mode(final_byte);
            return;
        }
        match final_byte {
            b'A' => {
                self.y = self
                    .y
                    .saturating_sub(self.param(0, 1))
                    .max(self.top_limit())
            }
            b'B' | b'e' => self.y = (self.y + self.param(0, 1)).min(self.bottom_limit()),
            b'C' | b'a' => self.x = (self.x + self.param(0, 1)).min(COLS - 1),
            b'D' => self.x = self.x.saturating_sub(self.param(0, 1)),
            b'E' => {
                self.y = (self.y + self.param(0, 1)).min(self.bottom_limit());
                self.x = 0;
            }
            b'F' => {
                self.y = self
                    .y
                    .saturating_sub(self.param(0, 1))
                    .max(self.top_limit());
                self.x = 0;
            }
            b'G' | b'`' => self.x = (self.param(0, 1) - 1).min(COLS - 1),
            b'd' => self.y = (self.param(0, 1) - 1).min(ROWS - 1),
            b'H' | b'f' => {
                self.y = (self.param(0, 1) - 1).min(ROWS - 1);
                self.x = (self.param(1, 1) - 1).min(COLS - 1);
            }
            b'J' => self.erase_display(self.param(0, 0)),
            b'K' => self.erase_line(self.param(0, 0)),
            b'L' => {
                if (self.scroll_top..=self.scroll_bottom).contains(&self.y) {
                    self.scroll_region_down(self.y, self.param(0, 1));
                }
            }
            b'M' => {
                if (self.scroll_top..=self.scroll_bottom).contains(&self.y) {
                    self.scroll_region_up(self.y, self.param(0, 1));
                }
            }
            b'P' => self.delete_chars(self.param(0, 1)),
            b'@' => self.insert_chars(self.param(0, 1)),
            b'X' => {
                let (x, y) = (self.x, self.y);
                let end = (x + self.param(0, 1)).min(COLS);
                let blank = self.blank();
                self.grid_mut().cells[y][x..end].fill(blank);
            }
            b'S' => self.scroll_up(self.param(0, 1)),
            b'T' => self.scroll_down(self.param(0, 1)),
            b'm' => self.sgr(),
            b'r' => {
                let top = self.param(0, 1) - 1;
                let bottom = self.param(1, ROWS) - 1;
                if top < bottom && bottom < ROWS {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                } else {
                    self.scroll_top = 0;
                    self.scroll_bottom = ROWS - 1;
                }
                self.x = 0;
                self.y = 0;
            }
            b's' => self.save_cursor(),
            b'u' => self.restore_cursor(),
            b'n' if self.param(0, 0) == 6 => self.report_cursor(),
            _ => {}
        }
    }

    fn top_limit(&self) -> usize {
        if self.y >= self.scroll_top {
            self.scroll_top
        } else {
            0
        }
    }

    fn bottom_limit(&self) -> usize {
        if self.y <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            ROWS - 1
        }
    }

    fn private_mode(&mut self, final_byte: u8) {
        let set = match final_byte {
            b'h' => true,
            b'l' => false,
            _ => return,
        };
        for index in 0..self.param_count.max(1) {
            match self.param(index, 0) {
                25 => self.cursor_visible = set,
                47 | 1047 | 1049 => self.set_alternate(set),
                _ => {}
            }
        }
    }

    fn set_alternate(&mut self, enable: bool) {
        if enable == self.using_alt {
            return;
        }
        if enable {
            self.alt_saved = Saved {
                x: self.x,
                y: self.y,
                fg: self.fg,
                bg: self.bg,
                attr: self.attr,
            };
            self.alt = Grid::BLANK;
            self.using_alt = true;
        } else {
            self.using_alt = false;
            let saved = self.alt_saved;
            self.x = saved.x;
            self.y = saved.y;
            self.fg = saved.fg;
            self.bg = saved.bg;
            self.attr = saved.attr;
        }
        self.scroll_top = 0;
        self.scroll_bottom = ROWS - 1;
    }

    fn save_cursor(&mut self) {
        self.saved = Saved {
            x: self.x,
            y: self.y,
            fg: self.fg,
            bg: self.bg,
            attr: self.attr,
        };
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved;
        self.x = saved.x.min(COLS - 1);
        self.y = saved.y.min(ROWS - 1);
        self.fg = saved.fg;
        self.bg = saved.bg;
        self.attr = saved.attr;
        self.wrap_pending = false;
    }

    fn report_cursor(&mut self) {
        let mut buffer = [0u8; 16];
        let mut length = 0;
        let mut push = |byte: u8| {
            buffer[length] = byte;
            length += 1;
        };
        push(0x1b);
        push(b'[');
        for (index, value) in [self.y + 1, self.x + 1].into_iter().enumerate() {
            if index == 1 {
                push(b';');
            }
            if value >= 10 {
                push(b'0' + (value / 10) as u8);
            }
            push(b'0' + (value % 10) as u8);
        }
        push(b'R');
        let room = REPLY_BYTES - self.reply_len;
        if length <= room {
            self.reply[self.reply_len..self.reply_len + length].copy_from_slice(&buffer[..length]);
            self.reply_len += length;
        }
    }

    fn erase_display(&mut self, mode: usize) {
        let blank = self.blank();
        let (x, y) = (self.x, self.y);
        let grid = self.grid_mut();
        match mode {
            0 => {
                grid.cells[y][x..].fill(blank);
                for row in &mut grid.cells[y + 1..] {
                    *row = [blank; COLS];
                }
            }
            1 => {
                for row in &mut grid.cells[..y] {
                    *row = [blank; COLS];
                }
                grid.cells[y][..=x].fill(blank);
            }
            2 | 3 => {
                grid.cells.fill([blank; COLS]);
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: usize) {
        let blank = self.blank();
        let (x, y) = (self.x, self.y);
        let row = &mut self.grid_mut().cells[y];
        match mode {
            0 => row[x..].fill(blank),
            1 => row[..=x].fill(blank),
            2 => row.fill(blank),
            _ => {}
        }
    }

    fn delete_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (x, y) = (self.x, self.y);
        let count = count.min(COLS - x);
        let row = &mut self.grid_mut().cells[y];
        row.copy_within(x + count.., x);
        row[COLS - count..].fill(blank);
    }

    fn insert_chars(&mut self, count: usize) {
        let blank = self.blank();
        let (x, y) = (self.x, self.y);
        let count = count.min(COLS - x);
        let row = &mut self.grid_mut().cells[y];
        row.copy_within(x..COLS - count, x + count);
        row[x..x + count].fill(blank);
    }

    fn sgr(&mut self) {
        if self.param_count == 0 {
            self.fg = DEFAULT_COLOR;
            self.bg = DEFAULT_COLOR;
            self.attr = 0;
            return;
        }
        let mut index = 0;
        while index < self.param_count {
            let value = self.params[index];
            match value {
                0 => {
                    self.fg = DEFAULT_COLOR;
                    self.bg = DEFAULT_COLOR;
                    self.attr = 0;
                }
                1 => self.attr |= ATTR_BOLD,
                2 => self.attr |= ATTR_DIM,
                4 => self.attr |= ATTR_UNDERLINE,
                7 => self.attr |= ATTR_INVERSE,
                22 => self.attr &= !(ATTR_BOLD | ATTR_DIM),
                24 => self.attr &= !ATTR_UNDERLINE,
                27 => self.attr &= !ATTR_INVERSE,
                30..=37 => self.fg = (value - 30) as u8,
                39 => self.fg = DEFAULT_COLOR,
                40..=47 => self.bg = (value - 40) as u8,
                49 => self.bg = DEFAULT_COLOR,
                90..=97 => self.fg = (value - 90 + 8) as u8,
                100..=107 => self.bg = (value - 100 + 8) as u8,
                // 256-colour and true-colour forms: map to the nearest of the
                // 16 colours instead of mis-reading the following parameters.
                38 | 48 => {
                    let (color, used) = self.extended_color(index + 1);
                    if let Some(color) = color {
                        if value == 38 {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                    }
                    index += used;
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn extended_color(&self, start: usize) -> (Option<u8>, usize) {
        let get = |offset: usize| -> u16 {
            if start + offset < self.param_count {
                self.params[start + offset]
            } else {
                0
            }
        };
        match get(0) {
            5 => {
                let index = get(1);
                let color = if index < 16 {
                    index as u8
                } else if index >= 232 {
                    // Grayscale ramp.
                    if index < 244 { 8 } else { 7 }
                } else {
                    let cube = index - 16;
                    let (r, g, b) = (cube / 36, (cube / 6) % 6, cube % 6);
                    nearest(r * 51, g * 51, b * 51)
                };
                (Some(color), 2)
            }
            2 => (Some(nearest(get(1), get(2), get(3))), 4),
            _ => (None, 0),
        }
    }
}

/// Picks the palette entry closest to an RGB colour.
fn nearest(r: u16, g: u16, b: u16) -> u8 {
    let mut best = 0usize;
    let mut best_distance = u32::MAX;
    for (index, rgb) in PALETTE.iter().enumerate() {
        let dr = i32::from(r) - ((rgb >> 16) & 0xff) as i32;
        let dg = i32::from(g) - ((rgb >> 8) & 0xff) as i32;
        let db = i32::from(b) - (rgb & 0xff) as i32;
        let distance = (dr * dr + dg * dg + db * db) as u32;
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best as u8
}

/// Encodes a key press as the bytes an xterm-compatible terminal sends.
///
/// Returns the number of bytes written to `out` (at most 8), or zero when the
/// key has no terminal encoding.
pub fn encode_key(key: Key, modifiers: Modifiers, out: &mut [u8; 8]) -> usize {
    let sequence: &[u8] = match key {
        Key::Char(byte) => {
            let byte = if modifiers.shift { shifted(byte) } else { byte };
            if modifiers.ctrl {
                let control = match byte {
                    b'a'..=b'z' => byte - b'a' + 1,
                    b'A'..=b'Z' => byte - b'A' + 1,
                    b'@' | b' ' | b'2' => 0,
                    b'[' | b'3' => 0x1b,
                    b'\\' | b'4' => 0x1c,
                    b']' | b'5' => 0x1d,
                    b'^' | b'6' => 0x1e,
                    b'_' | b'-' | b'7' => 0x1f,
                    b'?' | b'8' => 0x7f,
                    _ => return 0,
                };
                out[0] = control;
                return 1;
            }
            if modifiers.alt {
                out[0] = 0x1b;
                out[1] = byte;
                return 2;
            }
            out[0] = byte;
            return 1;
        }
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Delete => b"\x1b[3~",
        Key::Tab if modifiers.shift => b"\x1b[Z",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Right => b"\x1b[C",
        Key::Left => b"\x1b[D",
        Key::Function(number) => match number {
            1 => b"\x1bOP",
            2 => b"\x1bOQ",
            3 => b"\x1bOR",
            4 => b"\x1bOS",
            5 => b"\x1b[15~",
            6 => b"\x1b[17~",
            7 => b"\x1b[18~",
            8 => b"\x1b[19~",
            9 => b"\x1b[20~",
            10 => b"\x1b[21~",
            11 => b"\x1b[23~",
            12 => b"\x1b[24~",
            _ => return 0,
        },
    };
    out[..sequence.len()].copy_from_slice(sequence);
    sequence.len()
}

/// Applies a US-layout shift to an unshifted printable key.
fn shifted(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => byte - 32,
        b'1' => b'!',
        b'2' => b'@',
        b'3' => b'#',
        b'4' => b'$',
        b'5' => b'%',
        b'6' => b'^',
        b'7' => b'&',
        b'8' => b'*',
        b'9' => b'(',
        b'0' => b')',
        b'-' => b'_',
        b'=' => b'+',
        b'[' => b'{',
        b']' => b'}',
        b'\\' => b'|',
        b';' => b':',
        b'\'' => b'"',
        b',' => b'<',
        b'.' => b'>',
        b'/' => b'?',
        b'`' => b'~',
        other => other,
    }
}

#[cfg(test)]
mod tests;

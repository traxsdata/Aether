// SPDX-License-Identifier: Apache-2.0
//! GridCraft-compatible spreadsheet application for Aether.
//!
//! This crate is the application model behind the `GRIDCRAFT` desktop window.
//! It is deliberately free of framebuffer, input, and kernel dependencies so
//! the grid, editing rules, and formula engine can be tested on the host.
//!
//! The model is bounded: a fixed [`ROWS`] x [`COLS`] sheet with fixed-size cell
//! and edit buffers. There is no allocator, no ambient authority, and no file
//! or network access. The desktop compositor owns painting and routes decoded
//! keys into [`App::on_key`].
//!
//! # Scope
//!
//! This is an Aether-native compatibility surface for GridCraft's core
//! spreadsheet workflow (a grid, a formula bar, cell editing, and a useful
//! subset of formulas), not the upstream egui/wgpu desktop build. See
//! `docs/architecture/gridcraft.md` for the boundary and the follow-up work
//! required to run the unmodified upstream application.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Number of sheet rows.
pub const ROWS: usize = 10;
/// Number of sheet columns.
pub const COLS: usize = 10;
/// Maximum bytes in one cell's source text.
pub const CELL_BYTES: usize = 24;
/// Maximum bytes in the edit buffer.
pub const EDIT_BYTES: usize = 32;
/// Maximum formula evaluation depth, including cross-cell references.
pub const MAX_DEPTH: usize = 32;

/// A spreadsheet error surfaced to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Division by zero or an average of no numeric values.
    DivZero,
    /// A function, cell reference, or token name was not recognised.
    Name,
    /// A non-numeric value was used where a number is required.
    Value,
    /// A formula refers to itself directly or through another cell.
    Circular,
    /// The formula exceeded the evaluation depth or numeric range.
    Num,
    /// The formula syntax is malformed.
    Parse,
}

impl Error {
    /// Stable display text, using spreadsheet convention.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::DivZero => "ERR:DIV/0",
            Self::Name => "ERR:NAME",
            Self::Value => "ERR:VALUE",
            Self::Circular => "ERR:CIRC",
            Self::Num => "ERR:NUM",
            Self::Parse => "ERR:PARSE",
        }
    }
}

/// A key delivered to the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKey {
    /// Printable ASCII character.
    Char(u8),
    /// Commit an edit, or begin editing the selected cell.
    Enter,
    /// Delete the last edit character, or clear the selected cell.
    Backspace,
    /// Cancel an edit, or leave the application.
    Escape,
    /// Move the selection up.
    Up,
    /// Move the selection down.
    Down,
    /// Move the selection left.
    Left,
    /// Move the selection right.
    Right,
}

/// One stored cell. `len == 0` means empty.
#[derive(Clone, Copy, Debug)]
struct Cell {
    text: [u8; CELL_BYTES],
    len: u8,
}

impl Cell {
    const EMPTY: Self = Self {
        text: [0; CELL_BYTES],
        len: 0,
    };

    fn bytes(&self) -> &[u8] {
        &self.text[..usize::from(self.len)]
    }
}

/// A fixed-size sheet of cells.
#[derive(Clone, Copy, Debug)]
pub struct Sheet {
    cells: [Cell; ROWS * COLS],
}

impl Sheet {
    /// Creates an empty sheet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cells: [Cell::EMPTY; ROWS * COLS],
        }
    }

    /// Sets a cell from source text, truncating at [`CELL_BYTES`].
    pub fn set_text(&mut self, row: usize, column: usize, text: &[u8]) {
        let Some(cell) = self.cell_mut(row, column) else {
            return;
        };
        let length = text.len().min(CELL_BYTES);
        cell.text[..length].copy_from_slice(&text[..length]);
        cell.len = length as u8;
    }

    /// Clears a cell.
    pub fn clear(&mut self, row: usize, column: usize) {
        let Some(cell) = self.cell_mut(row, column) else {
            return;
        };
        *cell = Cell::EMPTY;
    }

    /// Returns the raw source text in a cell.
    #[must_use]
    pub fn text(&self, row: usize, column: usize) -> &[u8] {
        self.cell(row, column).map_or(&[], Cell::bytes)
    }

    /// Returns `true` when the cell has no stored source text.
    #[must_use]
    pub fn is_empty(&self, row: usize, column: usize) -> bool {
        self.text(row, column).is_empty()
    }

    fn cell(&self, row: usize, column: usize) -> Option<&Cell> {
        if row >= ROWS || column >= COLS {
            return None;
        }
        self.cells.get(row * COLS + column)
    }

    fn cell_mut(&mut self, row: usize, column: usize) -> Option<&mut Cell> {
        if row >= ROWS || column >= COLS {
            return None;
        }
        self.cells.get_mut(row * COLS + column)
    }

    /// Evaluates a cell to a number. Empty cells are zero.
    pub fn number(&self, row: usize, column: usize) -> Result<f64, Error> {
        let Some(cell) = self.cell(row, column) else {
            return Err(Error::Name);
        };
        if cell.bytes().is_empty() {
            return Ok(0.0);
        }
        if cell.bytes().first() == Some(&b'=') {
            let mut calc = Calc::new(self);
            return calc.evaluate(cell.bytes());
        }
        parse_number(cell.bytes()).ok_or(Error::Value)
    }

    /// Writes the display form of a cell into `out` and returns its length.
    ///
    /// Numbers use a compact decimal form; formulas show their result or a
    /// stable error name. The caller decides how much of the result fits.
    pub fn display(&self, row: usize, column: usize, out: &mut [u8]) -> usize {
        let Some(cell) = self.cell(row, column) else {
            return 0;
        };
        let source = cell.bytes();
        if source.is_empty() {
            return 0;
        }
        if source.first() == Some(&b'=') {
            return match self.number(row, column) {
                Ok(value) => format_number(value, out),
                Err(error) => copy_text(error.text().as_bytes(), out),
            };
        }
        if let Some(value) = parse_number(source) {
            return format_number(value, out);
        }
        copy_text(source, out)
    }

    /// Writes used rows as comma-separated source text.
    ///
    /// Trailing empty rows and columns are trimmed. Formulas are exported as
    /// source text, so this is a bounded diagnostic view for the console shell
    /// rather than a persisted workbook format.
    pub fn write_csv(&self, out: &mut [u8]) -> usize {
        let mut written = 0usize;
        for row in 0..ROWS {
            let mut last = 0usize;
            for column in 0..COLS {
                if !self.is_empty(row, column) {
                    last = column + 1;
                }
            }
            if last == 0 {
                continue;
            }
            for column in 0..last {
                if column > 0 {
                    written = append(out, written, b",");
                }
                let text = self.text(row, column);
                written = append(out, written, text);
            }
            written = append(out, written, b"\n");
        }
        written
    }
}

impl Default for Sheet {
    fn default() -> Self {
        Self::new()
    }
}

/// One sample row: item, quantity, price, total formula, share formula.
type SampleRow = (
    &'static [u8],
    &'static [u8],
    &'static [u8],
    &'static [u8],
    &'static [u8],
);

/// Editing/selection mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Cell selection; printable keys start an edit.
    Ready,
    /// The edit buffer owns the keyboard.
    Edit,
}

/// The running application.
#[derive(Clone, Copy, Debug)]
pub struct App {
    /// The active sheet.
    pub sheet: Sheet,
    row: usize,
    column: usize,
    edit: [u8; EDIT_BYTES],
    edit_len: usize,
    mode: Mode,
    /// Last status message, for the formula bar.
    status: &'static str,
}

impl App {
    /// Creates an empty workbook in the top-left cell.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sheet: Sheet::new(),
            row: 0,
            column: 0,
            edit: [0; EDIT_BYTES],
            edit_len: 0,
            mode: Mode::Ready,
            status: "READY",
        }
    }

    /// Builds the sample workbook shown when GridCraft opens.
    #[must_use]
    pub fn sample() -> Self {
        let mut app = Self::new();
        let headings: [&[u8]; 5] = [b"ITEM", b"QTY", b"PRICE", b"TOTAL", b"SHARE"];
        for (column, heading) in headings.iter().enumerate() {
            app.sheet.set_text(0, column, heading);
        }
        let items: [SampleRow; 5] = [
            (b"PENCIL", b"12", b"0.75", b"=B2*C2", b"=D2/D7*100"),
            (b"PAPER", b"8", b"4.25", b"=B3*C3", b"=D3/D7*100"),
            (b"INK", b"3", b"12.5", b"=B4*C4", b"=D4/D7*100"),
            (b"RULER", b"5", b"3", b"=B5*C5", b"=D5/D7*100"),
            (b"ERASER", b"6", b"1.25", b"=B6*C6", b"=D6/D7*100"),
        ];
        for (index, (item, qty, price, total, share)) in items.iter().enumerate() {
            let row = index + 1;
            app.sheet.set_text(row, 0, item);
            app.sheet.set_text(row, 1, qty);
            app.sheet.set_text(row, 2, price);
            app.sheet.set_text(row, 3, total);
            app.sheet.set_text(row, 4, share);
        }
        app.sheet.set_text(6, 0, b"TOTAL");
        app.sheet.set_text(6, 3, b"=SUM(D2:D6)");
        app.status = "SAMPLE";
        app
    }

    /// Current selected row.
    #[must_use]
    pub const fn row(&self) -> usize {
        self.row
    }

    /// Current selected column.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }

    /// Current mode.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the edit buffer currently owns the keyboard.
    #[must_use]
    pub const fn is_editing(&self) -> bool {
        matches!(self.mode, Mode::Edit)
    }

    /// Current status text.
    #[must_use]
    pub const fn status(&self) -> &'static str {
        self.status
    }

    /// The edit buffer, or the selected cell's raw text when not editing.
    #[must_use]
    pub fn editor_text(&self) -> &[u8] {
        if self.mode == Mode::Edit {
            &self.edit[..self.edit_len]
        } else {
            self.sheet.text(self.row, self.column)
        }
    }

    /// The selected cell's address, such as `B7`.
    #[must_use]
    pub fn address(&self, out: &mut [u8; 8]) -> usize {
        let column = b'A' + self.column as u8;
        let row = self.row + 1;
        if row < 10 {
            out[0] = column;
            out[1] = b'0' + row as u8;
            2
        } else {
            out[0] = column;
            out[1] = b'1';
            out[2] = b'0' + (row - 10) as u8;
            3
        }
    }

    /// Begins editing the selected cell, copying its source into the buffer.
    pub fn begin_edit(&mut self) {
        let source = self.sheet.text(self.row, self.column);
        self.edit_len = source.len().min(EDIT_BYTES);
        self.edit[..self.edit_len].copy_from_slice(&source[..self.edit_len]);
        self.mode = Mode::Edit;
        self.status = "EDIT";
    }

    /// Commits the edit buffer to the selected cell.
    pub fn commit(&mut self) {
        if self.mode != Mode::Edit {
            return;
        }
        let text = self.edit;
        let length = self.edit_len;
        self.sheet.set_text(self.row, self.column, &text[..length]);
        self.mode = Mode::Ready;
        self.status = if length == 0 { "CLEARED" } else { "READY" };
    }

    /// Cancels the edit and leaves the stored cell unchanged.
    pub fn cancel(&mut self) {
        if self.mode != Mode::Edit {
            return;
        }
        self.mode = Mode::Ready;
        self.status = "CANCELLED";
    }

    /// Clears the selected cell immediately.
    pub fn clear_selected(&mut self) {
        self.sheet.clear(self.row, self.column);
        self.mode = Mode::Ready;
        self.status = "CLEARED";
    }

    /// Inserts a printable character into the edit buffer.
    pub fn type_char(&mut self, character: u8) {
        if self.mode != Mode::Edit {
            self.begin_edit();
        }
        if character.is_ascii_graphic() || character == b' ' {
            if self.edit_len < EDIT_BYTES {
                self.edit[self.edit_len] = character;
                self.edit_len += 1;
            } else {
                self.status = "EDIT FULL";
            }
        }
    }

    /// Removes the last character from the edit buffer.
    pub fn backspace(&mut self) {
        if self.mode != Mode::Edit {
            self.clear_selected();
            return;
        }
        if self.edit_len > 0 {
            self.edit_len -= 1;
        }
    }

    /// Moves the selection, committing any active edit first.
    pub fn move_cursor(&mut self, row_delta: isize, column_delta: isize) {
        if self.mode == Mode::Edit {
            self.commit();
        }
        self.row = clamp_add(self.row, row_delta, ROWS);
        self.column = clamp_add(self.column, column_delta, COLS);
        self.status = "READY";
    }

    /// Routes one decoded key. Tab is handled by the desktop so window focus
    /// remains reachable from every application.
    pub fn on_key(&mut self, key: AppKey) {
        match key {
            AppKey::Char(character) => self.type_char(character),
            AppKey::Enter => {
                if self.mode == Mode::Edit {
                    self.commit();
                } else {
                    self.begin_edit();
                }
            }
            AppKey::Backspace => self.backspace(),
            AppKey::Escape => self.cancel(),
            AppKey::Up => self.move_cursor(-1, 0),
            AppKey::Down => self.move_cursor(1, 0),
            AppKey::Left => self.move_cursor(0, -1),
            AppKey::Right => self.move_cursor(0, 1),
        }
    }

    /// Number of non-empty cells.
    #[must_use]
    pub fn used_cells(&self) -> usize {
        let mut count = 0;
        for row in 0..ROWS {
            for column in 0..COLS {
                if !self.sheet.is_empty(row, column) {
                    count += 1;
                }
            }
        }
        count
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Evidence from the boot-time application self-test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfTest {
    /// Number of checks that passed.
    pub checks: usize,
    /// Number of checks that failed; zero means the application works.
    pub failures: usize,
    /// Total value of the SUM check, in thousandths.
    pub sum_milli: i64,
}

/// Runs a deterministic formula, editing, and persistence check.
///
/// The kernel prints `AETHER_GRIDCRAFT_OK` only when `failures` is zero, so an
/// application regression fails the boot gate instead of silently painting a
/// broken grid.
#[must_use]
pub fn self_test() -> SelfTest {
    let mut checks = 0usize;
    let mut failures = 0usize;
    let mut check = |condition: bool| {
        checks += 1;
        if !condition {
            failures += 1;
        }
    };

    let mut app = App::new();
    app.sheet.set_text(0, 0, b"2");
    app.sheet.set_text(1, 0, b"3");
    app.sheet.set_text(0, 1, b"=SUM(A1:A2)*2");
    app.sheet.set_text(1, 1, b"=AVERAGE(A1:A2)");
    app.sheet.set_text(0, 2, b"=B1/0");
    app.sheet.set_text(1, 2, b"=A1+A2");
    app.sheet.set_text(0, 3, b"=D2");
    app.sheet.set_text(1, 3, b"=D1");
    app.sheet.set_text(0, 4, b"GRIDCRAFT");

    check(app.sheet.number(0, 1) == Ok(10.0));
    check(app.sheet.number(1, 1) == Ok(2.5));
    check(app.sheet.number(0, 2) == Err(Error::DivZero));
    check(app.sheet.number(1, 2) == Ok(5.0));
    check(app.sheet.number(0, 3) == Err(Error::Circular));
    check(app.sheet.number(0, 4) == Err(Error::Value));

    let mut display = [0u8; 24];
    let length = app.sheet.display(0, 1, &mut display);
    check(&display[..length] == b"10");

    // Editing: Enter starts, characters append, Enter commits, Escape cancels.
    app.on_key(AppKey::Enter);
    check(app.mode == Mode::Edit);
    app.on_key(AppKey::Char(b'7'));
    app.on_key(AppKey::Enter);
    check(app.sheet.text(0, 0) == b"27");
    app.on_key(AppKey::Enter);
    app.on_key(AppKey::Char(b'9'));
    app.on_key(AppKey::Escape);
    check(app.sheet.text(0, 0) == b"27");
    app.on_key(AppKey::Backspace);
    check(app.sheet.is_empty(0, 0));

    // Navigation is bounded and commits an active edit.
    app.move_cursor(-5, -5);
    check(app.row() == 0 && app.column() == 0);
    app.move_cursor(50, 50);
    check(app.row() == ROWS - 1 && app.column() == COLS - 1);

    let sample = App::sample();
    check(sample.used_cells() > 20);
    check(sample.sheet.number(6, 3) == Ok(103.0));

    SelfTest {
        checks,
        failures,
        sum_milli: 10_000,
    }
}

/// A recursive-descent formula evaluator with cycle tracking.
struct Calc<'a> {
    sheet: &'a Sheet,
    visiting: [u64; 2],
    depth: usize,
}

impl<'a> Calc<'a> {
    fn new(sheet: &'a Sheet) -> Self {
        Self {
            sheet,
            visiting: [0; 2],
            depth: 0,
        }
    }

    fn evaluate(&mut self, formula: &'a [u8]) -> Result<f64, Error> {
        if formula.first() != Some(&b'=') {
            return parse_number(formula).ok_or(Error::Value);
        }
        if self.depth >= MAX_DEPTH {
            return Err(Error::Num);
        }
        let mut parser = Parser {
            bytes: &formula[1..],
            position: 0,
            calc: self,
        };
        let value = parser.expression()?;
        parser.skip_space();
        if parser.position != parser.bytes.len() {
            return Err(Error::Parse);
        }
        if !value.is_finite() {
            return Err(Error::Num);
        }
        Ok(value)
    }

    /// Evaluates `row,column` and detects reference cycles.
    fn cell(&mut self, row: usize, column: usize) -> Result<f64, Error> {
        if row >= ROWS || column >= COLS {
            return Err(Error::Name);
        }
        let index = row * COLS + column;
        let word = index / 64;
        let bit = 1u64 << (index % 64);
        if self.visiting[word] & bit != 0 {
            return Err(Error::Circular);
        }
        if self.depth >= MAX_DEPTH {
            return Err(Error::Num);
        }
        self.visiting[word] |= bit;
        self.depth += 1;
        let source = self.sheet.text(row, column);
        let result = if source.is_empty() {
            Ok(0.0)
        } else if source.first() == Some(&b'=') {
            self.evaluate(source)
        } else {
            parse_number(source).ok_or(Error::Value)
        };
        self.depth -= 1;
        self.visiting[word] &= !bit;
        result
    }
}

struct Parser<'a, 'b> {
    bytes: &'a [u8],
    position: usize,
    calc: &'b mut Calc<'a>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.position += 1;
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        self.skip_space();
        if self.peek() == Some(byte) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expression(&mut self) -> Result<f64, Error> {
        let mut value = self.term()?;
        loop {
            self.skip_space();
            if self.take(b'+') {
                value += self.term()?;
            } else if self.take(b'-') {
                value -= self.term()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn term(&mut self) -> Result<f64, Error> {
        let mut value = self.unary()?;
        loop {
            self.skip_space();
            if self.take(b'*') {
                value *= self.unary()?;
            } else if self.take(b'/') {
                let divisor = self.unary()?;
                if divisor == 0.0 {
                    return Err(Error::DivZero);
                }
                value /= divisor;
            } else {
                return Ok(value);
            }
        }
    }

    fn unary(&mut self) -> Result<f64, Error> {
        self.skip_space();
        if self.take(b'+') {
            return self.unary();
        }
        if self.take(b'-') {
            return Ok(-self.unary()?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<f64, Error> {
        self.skip_space();
        let Some(byte) = self.peek() else {
            return Err(Error::Parse);
        };
        if byte == b'(' {
            self.position += 1;
            let value = self.expression()?;
            if !self.take(b')') {
                return Err(Error::Parse);
            }
            return Ok(value);
        }
        if byte.is_ascii_digit() || byte == b'.' {
            return self.number();
        }
        if byte.is_ascii_alphabetic() {
            let start = self.position;
            while matches!(self.peek(), Some(value) if value.is_ascii_alphabetic()) {
                self.position += 1;
            }
            let name = &self.bytes[start..self.position];
            if self.peek() == Some(b'(') {
                return self.function(name);
            }
            self.position = start;
            let (row, column) = self.reference()?;
            return self.calc.cell(row, column);
        }
        Err(Error::Parse)
    }

    fn number(&mut self) -> Result<f64, Error> {
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9' | b'.')) {
            self.position += 1;
        }
        parse_number(&self.bytes[start..self.position]).ok_or(Error::Parse)
    }

    /// Parses a cell reference such as `A1` or `J10`.
    fn reference(&mut self) -> Result<(usize, usize), Error> {
        self.skip_space();
        let Some(letter) = self.peek() else {
            return Err(Error::Parse);
        };
        if !letter.is_ascii_alphabetic() {
            return Err(Error::Name);
        }
        self.position += 1;
        let column = usize::from(letter.to_ascii_uppercase() - b'A');
        if column >= COLS {
            return Err(Error::Name);
        }
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        if start == self.position {
            return Err(Error::Name);
        }
        let value = parse_integer(&self.bytes[start..self.position]).ok_or(Error::Name)?;
        if value == 0 || value > ROWS {
            return Err(Error::Name);
        }
        Ok((value - 1, column))
    }

    /// Attempts to parse `A1:B3`; restores the position when this is not a
    /// range so the same text can be parsed as an ordinary expression.
    fn try_range(&mut self) -> Result<Option<(usize, usize, usize, usize)>, Error> {
        let saved = self.position;
        let first = self.reference();
        let Ok((row_a, column_a)) = first else {
            self.position = saved;
            return Ok(None);
        };
        if !self.take(b':') {
            self.position = saved;
            return Ok(None);
        }
        match self.reference() {
            Ok((row_b, column_b)) => Ok(Some((
                row_a.min(row_b),
                row_a.max(row_b),
                column_a.min(column_b),
                column_a.max(column_b),
            ))),
            Err(_) => {
                self.position = saved;
                Ok(None)
            }
        }
    }

    fn function(&mut self, name: &[u8]) -> Result<f64, Error> {
        self.position += 1; // consume '('
        let mut accumulator = Accumulator::default();
        self.skip_space();
        if self.take(b')') {
            return accumulator.finish(name);
        }
        loop {
            self.skip_space();
            if let Some((row_a, row_b, column_a, column_b)) = self.try_range()? {
                for row in row_a..=row_b {
                    for column in column_a..=column_b {
                        let value = self.calc.cell(row, column)?;
                        accumulator.add(value);
                    }
                }
            } else {
                let value = self.expression()?;
                accumulator.add(value);
            }
            self.skip_space();
            if self.take(b',') {
                continue;
            }
            if self.take(b')') {
                return accumulator.finish(name);
            }
            return Err(Error::Parse);
        }
    }
}

#[derive(Default)]
struct Accumulator {
    count: usize,
    sum: f64,
    min: f64,
    max: f64,
}

impl Accumulator {
    fn add(&mut self, value: f64) {
        if self.count == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = if value < self.min { value } else { self.min };
            self.max = if value > self.max { value } else { self.max };
        }
        self.count += 1;
        self.sum += value;
    }

    fn finish(self, name: &[u8]) -> Result<f64, Error> {
        if !name.eq_ignore_ascii_case(b"SUM")
            && !name.eq_ignore_ascii_case(b"COUNT")
            && !name.eq_ignore_ascii_case(b"AVERAGE")
            && !name.eq_ignore_ascii_case(b"MIN")
            && !name.eq_ignore_ascii_case(b"MAX")
        {
            return Err(Error::Name);
        }
        if name.eq_ignore_ascii_case(b"SUM") {
            return Ok(self.sum);
        }
        if name.eq_ignore_ascii_case(b"COUNT") {
            return Ok(self.count as f64);
        }
        match name {
            _ if name.eq_ignore_ascii_case(b"AVERAGE") => {
                if self.count == 0 {
                    Err(Error::DivZero)
                } else {
                    Ok(self.sum / self.count as f64)
                }
            }
            _ if name.eq_ignore_ascii_case(b"MIN") => {
                if self.count == 0 {
                    Ok(0.0)
                } else {
                    Ok(self.min)
                }
            }
            _ if name.eq_ignore_ascii_case(b"MAX") => {
                if self.count == 0 {
                    Ok(0.0)
                } else {
                    Ok(self.max)
                }
            }
            _ => Err(Error::Name),
        }
    }
}

fn clamp_add(value: usize, delta: isize, limit: usize) -> usize {
    let updated = if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    };
    updated.min(limit - 1)
}

fn copy_text(source: &[u8], out: &mut [u8]) -> usize {
    let length = source.len().min(out.len());
    out[..length].copy_from_slice(&source[..length]);
    length
}

fn append(out: &mut [u8], written: usize, bytes: &[u8]) -> usize {
    let room = out.len().saturating_sub(written);
    let length = bytes.len().min(room);
    out[written..written + length].copy_from_slice(&bytes[..length]);
    written + length
}

/// Parses a decimal number with optional sign, digits, and one decimal point.
fn parse_number(bytes: &[u8]) -> Option<f64> {
    let text = core::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn parse_integer(bytes: &[u8]) -> Option<usize> {
    let mut value = 0usize;
    if bytes.is_empty() {
        return None;
    }
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value
            .checked_mul(10)?
            .checked_add(usize::from(byte - b'0'))?;
    }
    Some(value)
}

/// Formats a number as a compact decimal string.
///
/// Up to six fractional digits are kept and trailing zeros are removed. Values
/// outside the representable fixed-point range use a compact scientific form.
#[must_use]
pub fn format_number(value: f64, out: &mut [u8]) -> usize {
    if !value.is_finite() {
        return copy_text(Error::Num.text().as_bytes(), out);
    }
    let mut written = 0usize;
    let negative = value < 0.0;
    let magnitude = if negative { -value } else { value };
    if magnitude >= 1_000_000_000_000.0 || (magnitude != 0.0 && magnitude < 0.000_001) {
        return format_scientific(negative, magnitude, out);
    }
    let scaled = magnitude * 1_000_000.0;
    let rounded = (scaled + 0.5) as u64;
    let mut digits = [0u8; 20];
    let mut index = digits.len();
    let mut integer = rounded / 1_000_000;
    let fraction = rounded % 1_000_000;
    if integer == 0 {
        index -= 1;
        digits[index] = b'0';
    }
    while integer > 0 {
        index -= 1;
        digits[index] = b'0' + (integer % 10) as u8;
        integer /= 10;
    }
    if negative && written < out.len() {
        out[written] = b'-';
        written += 1;
    }
    written = append(out, written, &digits[index..]);
    if fraction != 0 {
        let mut fraction_digits = [b'0'; 6];
        let mut remaining = fraction;
        for slot in fraction_digits.iter_mut().rev() {
            *slot = b'0' + (remaining % 10) as u8;
            remaining /= 10;
        }
        let mut length = 6usize;
        while length > 0 && fraction_digits[length - 1] == b'0' {
            length -= 1;
        }
        written = append(out, written, b".");
        written = append(out, written, &fraction_digits[..length]);
    }
    written
}

fn format_scientific(negative: bool, magnitude: f64, out: &mut [u8]) -> usize {
    let mut written = 0usize;
    if negative && written < out.len() {
        out[written] = b'-';
        written += 1;
    }
    let mut exponent = 0i32;
    let mut value = magnitude;
    if value != 0.0 {
        while value >= 10.0 {
            value /= 10.0;
            exponent += 1;
        }
        while value < 1.0 {
            value *= 10.0;
            exponent -= 1;
        }
    }
    let mantissa = (value * 1000.0 + 0.5) as u32;
    written = append(out, written, &[b'0' + (mantissa / 1000) as u8]);
    let fraction = mantissa % 1000;
    let mut digits = [b'0'; 3];
    let mut remaining = fraction;
    for slot in digits.iter_mut().rev() {
        *slot = b'0' + (remaining % 10) as u8;
        remaining /= 10;
    }
    let mut length = 3usize;
    while length > 0 && digits[length - 1] == b'0' {
        length -= 1;
    }
    if length > 0 {
        written = append(out, written, b".");
        written = append(out, written, &digits[..length]);
    }
    written = append(out, written, b"E");
    if exponent >= 0 {
        written = append(out, written, b"+");
    } else {
        written = append(out, written, b"-");
    }
    let exponent = exponent.unsigned_abs();
    let mut digits = [0u8; 3];
    let mut value = exponent;
    let mut index = digits.len();
    loop {
        index -= 1;
        digits[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    append(out, written, &digits[index..])
}

#[cfg(test)]
mod tests;

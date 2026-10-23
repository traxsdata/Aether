// SPDX-License-Identifier: Apache-2.0
//! `htop` (and `top`): a live, interactive process viewer.
//!
//! The screen follows htop's layout: CPU and memory meters with the task count
//! and uptime beside them, a process table sorted by CPU use, and the F-key
//! bar at the bottom. CPU shares are measured between two samples of each
//! task's tick counter, so the first frame shows zeros as htop's does.
//!
//! When standard output is not the terminal (`top -b`, or `htop | grep sh`),
//! one snapshot is printed as plain text instead.

use crate::rt::{self, Args, Frame, FullScreen, Key};
use crate::system::{EMPTY_RECORD, MAX_PROCESSES, listed, owner, state_letter, system_info};
use aether_abi::user::{PROCESS_FLAG_SESSION, ProcessKind, ProcessRecord, SystemInfo, errno};
use aether_sdk as sdk;
use core::fmt::Write as _;

/// Column the table sorts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Cpu,
    Memory,
    Pid,
    Time,
}

impl SortBy {
    fn next(self) -> Self {
        match self {
            Self::Cpu => Self::Memory,
            Self::Memory => Self::Pid,
            Self::Pid => Self::Time,
            Self::Time => Self::Cpu,
        }
    }
}

/// One measured row of the table.
#[derive(Clone, Copy)]
struct Row {
    record: ProcessRecord,
    /// CPU share since the last sample, in tenths of a percent.
    cpu: u64,
}

const EMPTY_ROW: Row = Row {
    record: EMPTY_RECORD,
    cpu: 0,
};

/// A prompt shown in the footer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Browse,
    ConfirmKill(u32),
    Help,
}

struct Top {
    rows: [Row; MAX_PROCESSES],
    count: usize,
    /// Previous `(pid, cpu_ticks)` samples.
    previous: [(u32, u64); MAX_PROCESSES],
    previous_count: usize,
    previous_uptime: u64,
    previous_idle: u64,
    info: SystemInfo,
    /// Whole-system CPU busy share, tenths of a percent.
    busy: u64,
    sort: SortBy,
    selected: usize,
    scroll: usize,
    only_session: bool,
    mode: Mode,
    message: [u8; 80],
    message_len: usize,
    columns: usize,
    lines: usize,
}

impl Top {
    fn new() -> Self {
        let (columns, lines) = sdk::terminal_size().map_or((80, 24), |(columns, rows)| {
            (usize::from(columns).max(40), usize::from(rows).max(10))
        });
        Self {
            rows: [EMPTY_ROW; MAX_PROCESSES],
            count: 0,
            previous: [(0, 0); MAX_PROCESSES],
            previous_count: 0,
            previous_uptime: 0,
            previous_idle: 0,
            info: SystemInfo::default(),
            busy: 0,
            sort: SortBy::Cpu,
            selected: 0,
            scroll: 0,
            only_session: false,
            mode: Mode::Browse,
            message: [0; 80],
            message_len: 0,
            columns,
            lines,
        }
    }

    fn say(&mut self, text: &str) {
        let length = text.len().min(self.message.len());
        self.message[..length].copy_from_slice(&text.as_bytes()[..length]);
        self.message_len = length;
    }

    fn message(&self) -> &str {
        core::str::from_utf8(&self.message[..self.message_len]).unwrap_or("")
    }

    /// Takes a new sample and recomputes every share.
    fn sample(&mut self) {
        let selected_pid = self.selected_pid();
        let mut records = [EMPTY_RECORD; MAX_PROCESSES];
        let count = sdk::process_list(&mut records).unwrap_or(0);
        self.info = system_info();
        let elapsed = self.info.uptime_ticks.saturating_sub(self.previous_uptime);
        let idle = self.info.idle_ticks.saturating_sub(self.previous_idle);
        self.busy = if self.previous_uptime == 0 || elapsed == 0 {
            0
        } else {
            elapsed.saturating_sub(idle) * 1000 / elapsed
        };
        self.count = 0;
        for record in &records[..count] {
            // The idle task's time is the meter's idle share, so it is not a
            // row, as Linux htop shows no idle task.
            let idle = record.kind == ProcessKind::Idle as u8;
            if idle
                || !listed(record)
                || (self.only_session && record.flags & PROCESS_FLAG_SESSION == 0)
            {
                continue;
            }
            let before = self.previous[..self.previous_count]
                .iter()
                .find(|(pid, _)| *pid == record.pid)
                .map(|&(_, ticks)| ticks);
            let cpu = match before {
                Some(before) if elapsed > 0 => {
                    (record.cpu_ticks.saturating_sub(before) * 1000 / elapsed).min(1000)
                }
                _ => 0,
            };
            self.rows[self.count] = Row {
                record: *record,
                cpu,
            };
            self.count += 1;
        }
        self.previous_count = count;
        for (slot, record) in self.previous.iter_mut().zip(&records[..count]) {
            *slot = (record.pid, record.cpu_ticks);
        }
        self.previous_uptime = self.info.uptime_ticks;
        self.previous_idle = self.info.idle_ticks;
        self.sort_rows();
        // Keep the cursor on the same process when it moves in the order.
        if let Some(pid) = selected_pid
            && let Some(index) = self.rows[..self.count]
                .iter()
                .position(|row| row.record.pid == pid)
        {
            self.selected = index;
        }
        self.selected = self.selected.min(self.count.saturating_sub(1));
    }

    fn sort_rows(&mut self) {
        let sort = self.sort;
        let key = |row: &Row| -> (u64, u64) {
            let pid = u64::from(row.record.pid);
            match sort {
                // Descending by the measure, then ascending pid.
                SortBy::Cpu => (u64::MAX - row.cpu, pid),
                SortBy::Memory => (u64::MAX - u64::from(row.record.memory_kib), pid),
                SortBy::Time => (u64::MAX - row.record.cpu_ticks, pid),
                SortBy::Pid => (pid, 0),
            }
        };
        // Insertion sort: the table holds a few dozen rows.
        for index in 1..self.count {
            let mut position = index;
            while position > 0 && key(&self.rows[position - 1]) > key(&self.rows[position]) {
                self.rows.swap(position - 1, position);
                position -= 1;
            }
        }
    }

    fn selected_pid(&self) -> Option<u32> {
        (self.selected < self.count).then(|| self.rows[self.selected].record.pid)
    }

    /// Rows the process table has room for.
    fn table_height(&self) -> usize {
        self.lines.saturating_sub(6)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.count == 0 {
            return;
        }
        let target = self.selected as isize + delta;
        self.selected = target.clamp(0, self.count as isize - 1) as usize;
    }

    /// Handles a key; returns `false` to quit.
    fn key(&mut self, key: Key) -> bool {
        let page = self.table_height().max(1) as isize;
        match self.mode {
            Mode::ConfirmKill(pid) => {
                self.mode = Mode::Browse;
                if matches!(key, Key::Char(b'y' | b'Y') | Key::Enter) {
                    match sdk::kill(u64::from(pid)) {
                        Ok(()) => self.say("Process killed."),
                        Err(errno::EACCES) => {
                            self.say("Not permitted: only programs started in this terminal can be killed.");
                        }
                        Err(error) => self.say(sdk::error_message(error)),
                    }
                    self.sample();
                } else {
                    self.say("Kill cancelled.");
                }
                return true;
            }
            Mode::Help => {
                self.mode = Mode::Browse;
                return !matches!(key, Key::Char(b'q') | Key::Function(10) | Key::Ctrl(0x03));
            }
            Mode::Browse => {}
        }
        self.message_len = 0;
        match key {
            Key::Char(b'q' | b'Q') | Key::Function(10) | Key::Ctrl(0x03) => return false,
            Key::Up => self.move_selection(-1),
            Key::Down => self.move_selection(1),
            Key::PageUp => self.move_selection(-page),
            Key::PageDown | Key::Char(b' ') => self.move_selection(page),
            Key::Home => self.selected = 0,
            Key::End => self.selected = self.count.saturating_sub(1),
            Key::Function(9) | Key::Char(b'k') => {
                if let Some(pid) = self.selected_pid() {
                    self.mode = Mode::ConfirmKill(pid);
                }
            }
            Key::Function(6) | Key::Char(b's' | b'>' | b'<') => {
                self.sort = self.sort.next();
                self.sort_rows();
            }
            Key::Char(b'P') => {
                self.sort = SortBy::Cpu;
                self.sort_rows();
            }
            Key::Char(b'M') => {
                self.sort = SortBy::Memory;
                self.sort_rows();
            }
            Key::Char(b'T') => {
                self.sort = SortBy::Time;
                self.sort_rows();
            }
            Key::Char(b'N') => {
                self.sort = SortBy::Pid;
                self.sort_rows();
            }
            Key::Char(b'u') | Key::Function(4) => {
                self.only_session = !self.only_session;
                self.sample();
            }
            Key::Function(1) | Key::Char(b'h' | b'?') => self.mode = Mode::Help,
            Key::Function(5) | Key::Ctrl(0x0c) => self.sample(),
            _ => {}
        }
        true
    }

    fn draw(&mut self, frame: &mut Frame) {
        frame.str("\x1b[H\x1b[0m");
        if self.mode == Mode::Help {
            self.draw_help(frame);
            return;
        }
        let half = self.columns / 2;
        let meter_width = half.saturating_sub(2);
        // CPU meter.
        let busy = self.busy;
        meter(frame, "CPU", meter_width, busy, |text| {
            let _ = write!(text, "{}.{}%", busy / 10, busy % 10);
        });
        let running = self.rows[..self.count]
            .iter()
            .filter(|row| state_letter(row.record.state) == 'R')
            .count();
        let _ = write!(
            frame,
            "  \x1b[1;36mTasks: \x1b[1;37m{}\x1b[0;36m, \x1b[1;32m{running}\x1b[0;36m running\x1b[0m\x1b[K\r\n",
            self.info.tasks
        );
        // Memory meter.
        let total = self.info.memory_total_kib.max(1);
        let used = total.saturating_sub(self.info.memory_free_kib);
        let (total_bytes, used_bytes) = (total * 1024, used * 1024);
        meter(frame, "Mem", meter_width, used * 1000 / total, |text| {
            let _ = write!(text, "{}/{}", rt::Human(used_bytes), rt::Human(total_bytes));
        });
        let _ = write!(
            frame,
            "  \x1b[1;36mUptime: \x1b[1;37m{}\x1b[0m\x1b[K\r\n",
            rt::Duration {
                ticks: self.info.uptime_ticks,
                hz: u64::from(self.info.tick_hz),
            }
        );
        // Swap meter: there is no swap.
        meter(frame, "Swp", meter_width, 0, |text| {
            text.str("0K/0K");
        });
        let fs_total = u64::from(self.info.fs_total_bytes).max(1);
        let fs_used = u64::from(self.info.fs_used_bytes);
        let _ = write!(
            frame,
            "  \x1b[1;36mDisk: \x1b[1;37m{}\x1b[0;36m/{} used\x1b[0m\x1b[K\r\n\x1b[K\r\n",
            rt::Human(fs_used),
            rt::Human(fs_total)
        );
        // Table header, highlighting the sort column.
        frame.str("\x1b[30;42m");
        let header = [
            ("  PID", SortBy::Pid),
            (" USER    ", SortBy::Pid),
            (" S", SortBy::Pid),
            ("  CPU%", SortBy::Cpu),
            ("   MEM", SortBy::Memory),
            ("    TIME+", SortBy::Time),
            ("  Command", SortBy::Pid),
        ];
        let mut width = 0;
        for (index, (label, column)) in header.iter().enumerate() {
            let highlight = *column == self.sort && (index != 1 && index != 2 && index != 6);
            if highlight {
                frame.str("\x1b[30;46m");
            }
            frame.str(label);
            if highlight {
                frame.str("\x1b[30;42m");
            }
            width += label.len();
        }
        frame.pad(self.columns.saturating_sub(width));
        frame.str("\x1b[0m\r\n");
        // Process rows.
        let height = self.table_height();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        let hz = u64::from(self.info.tick_hz.max(1));
        for line in 0..height {
            let index = self.scroll + line;
            if index >= self.count {
                frame.str("\x1b[K\r\n");
                continue;
            }
            let row = self.rows[index];
            let record = &row.record;
            let selected = index == self.selected;
            if selected {
                frame.str("\x1b[30;46m");
            }
            let centis = record.cpu_ticks * 100 / hz;
            let mut cells = Frame::new();
            let _ = write!(
                cells,
                "{:>5} {:<8} {} {:>3}.{} {:>5} {:>3}:{:02}.{:02}  ",
                record.pid,
                owner(record),
                state_letter(record.state),
                row.cpu / 10,
                row.cpu % 10,
                rt::Human(u64::from(record.memory_kib) * 1024),
                centis / 6000,
                centis / 100 % 60,
                centis % 100,
            );
            let used = cells.len();
            frame.bytes(cells.as_bytes());
            if !selected && record.flags & PROCESS_FLAG_SESSION != 0 {
                frame.str("\x1b[1;32m");
            }
            let name = record.name();
            let room = self.columns.saturating_sub(used);
            let name = &name[..name.len().min(room)];
            frame.str(name);
            frame.pad(room - name.len());
            frame.str("\x1b[0m\r\n");
        }
        self.draw_footer(frame);
    }

    fn draw_footer(&self, frame: &mut Frame) {
        match self.mode {
            Mode::ConfirmKill(pid) => {
                let name = self.rows[..self.count]
                    .iter()
                    .find(|row| row.record.pid == pid)
                    .map_or("?", |row| row.record.name());
                let _ = write!(
                    frame,
                    "\x1b[30;43m Kill process {pid} ({name})? [y/N] \x1b[0m\x1b[K"
                );
                return;
            }
            Mode::Help | Mode::Browse => {}
        }
        if self.message_len > 0 {
            let _ = write!(frame, "\x1b[1;33m {}\x1b[0m\x1b[K", self.message());
            return;
        }
        let keys = [
            ("F1", "Help   "),
            (
                "F4",
                if self.only_session {
                    "All    "
                } else {
                    "Mine   "
                },
            ),
            ("F5", "Update "),
            ("F6", "SortBy "),
            ("F9", "Kill   "),
            ("F10", "Quit   "),
        ];
        let mut width = 0;
        for (key, label) in keys {
            if width + key.len() + label.len() > self.columns {
                break;
            }
            let _ = write!(frame, "\x1b[0m{key}\x1b[30;46m{label}");
            width += key.len() + label.len();
        }
        frame.str("\x1b[30;46m");
        frame.pad(self.columns.saturating_sub(width + 1));
        frame.str("\x1b[0m");
    }

    fn draw_help(&self, frame: &mut Frame) {
        frame.str(
            "\x1b[1mhtop for Aether\x1b[0m - interactive process viewer\x1b[K\r\n\x1b[K\r\n\
             \x1b[1;36m CPU\x1b[0m busy share of the processor since the last update\x1b[K\r\n\
             \x1b[1;36m Mem\x1b[0m physical memory in use\x1b[K\r\n\
             \x1b[1;32m green\x1b[0m commands were started from this terminal\x1b[K\r\n\x1b[K\r\n\
             \x1b[1m Up/Down PgUp/PgDn Home/End\x1b[0m  select a process\x1b[K\r\n\
             \x1b[1m F9 k\x1b[0m   kill the selected process (only your own)\x1b[K\r\n\
             \x1b[1m F6 s\x1b[0m   change the sort column\x1b[K\r\n\
             \x1b[1m P M T N\x1b[0m  sort by CPU, memory, time, or PID\x1b[K\r\n\
             \x1b[1m F4 u\x1b[0m   show only this terminal's programs, or everything\x1b[K\r\n\
             \x1b[1m F5\x1b[0m     update now\x1b[K\r\n\
             \x1b[1m F10 q\x1b[0m  quit\x1b[K\r\n\x1b[K\r\n\
             States: R running or ready, S sleeping, Z exited.\x1b[K\r\n\
             Owners: kernel, service, app, and user (this terminal).\x1b[K\r\n\x1b[K\r\n\
             Press any key to return.\x1b[J",
        );
    }
}

/// Draws an htop meter: `Label[|||||      text]`.
fn meter(
    frame: &mut Frame,
    label: &str,
    width: usize,
    permille: u64,
    text: impl FnOnce(&mut Frame),
) {
    let mut caption = Frame::new();
    text(&mut caption);
    let inner = width.saturating_sub(label.len() + 4);
    let caption_len = caption.len().min(inner);
    let filled = (permille.min(1000) as usize * inner).div_ceil(1000);
    let _ = write!(frame, "  \x1b[1;36m{label}\x1b[0;1m[");
    for column in 0..inner {
        let caption_start = inner - caption_len;
        if column >= caption_start {
            if column == caption_start {
                frame.str("\x1b[0;2m");
            }
            frame.bytes(&caption.as_bytes()[column - caption_start..=column - caption_start]);
        } else if column < filled {
            let color = if column * 4 < inner * 2 {
                "32"
            } else if column * 4 < inner * 3 {
                "33"
            } else {
                "31"
            };
            let _ = write!(frame, "\x1b[0;{color}m|");
        } else {
            frame.str(" ");
        }
    }
    frame.str("\x1b[0;1m]\x1b[0m");
}

/// Prints one plain-text snapshot, as `top -b -n 1` does.
fn batch() -> i64 {
    let mut top = Top::new();
    top.sample();
    let mut out = rt::Out::new();
    let hz = u64::from(top.info.tick_hz.max(1));
    outln!(
        out,
        "top - up {}, {} tasks, cpu busy {}%",
        rt::Duration {
            ticks: top.info.uptime_ticks,
            hz,
        },
        top.info.tasks,
        top.info.uptime_ticks.saturating_sub(top.info.idle_ticks) * 100
            / top.info.uptime_ticks.max(1)
    );
    outln!(
        out,
        "Mem: {} total, {} free",
        rt::Human(top.info.memory_total_kib * 1024),
        rt::Human(top.info.memory_free_kib * 1024)
    );
    outln!(out, "  PID USER     S   MEM     TIME+ COMMAND");
    top.sort = SortBy::Time;
    top.sort_rows();
    for row in &top.rows[..top.count] {
        let record = &row.record;
        let centis = record.cpu_ticks * 100 / hz;
        outln!(
            out,
            "{:>5} {:<8} {} {:>5} {:>3}:{:02}.{:02} {}",
            record.pid,
            owner(record),
            state_letter(record.state),
            rt::Human(u64::from(record.memory_kib) * 1024),
            centis / 6000,
            centis / 100 % 60,
            centis % 100,
            record.name()
        );
    }
    0
}

pub fn main(args: &Args) -> i64 {
    let (stdin_terminal, stdout_terminal) = rt::stdio_terminal();
    if !stdout_terminal || !stdin_terminal || args.iter().any(|arg| arg == "-b") {
        return batch();
    }
    let hz = rt::tick_hz();
    let _screen = FullScreen::enter();
    let mut top = Top::new();
    top.sample();
    let mut frame = Frame::new();
    let mut next_sample = sdk::uptime() + hz;
    let mut dirty = true;
    loop {
        if dirty {
            top.draw(&mut frame);
            frame.flush();
            dirty = false;
        }
        let mut input = [0u8; 32];
        match rt::try_read_raw(&mut input) {
            Some(0) => return 0,
            Some(count) => {
                let mut quit = false;
                rt::keys(&input[..count], |key| {
                    if !quit && !top.key(key) {
                        quit = true;
                    }
                });
                if quit {
                    return 0;
                }
                dirty = true;
            }
            None => {
                let _ = sdk::sleep(2);
            }
        }
        if sdk::uptime() >= next_sample {
            top.sample();
            next_sample = sdk::uptime() + hz;
            dirty = true;
        }
    }
}

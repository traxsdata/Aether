// SPDX-License-Identifier: Apache-2.0
//! Aether Terminal: the signed-in user's command-line session.
//!
//! The desktop's Terminal window shows one session. Its shell and every
//! command it starts are native ring-3 programs from the embedded userland
//! image (see `user/src/bin/userland`). They run as the ordinary user: their
//! filesystem root is [`crate::fs::TERMINAL_ROOT`], they hold no capabilities
//! or portal permissions, and they can only list processes, never act on
//! anything outside the session. This module owns
//!
//! * the emulator grid that program output is written into,
//! * the keyboard input queue,
//! * the bounded pipes that connect pipeline stages, and
//! * the native system calls that implement standard input and output.
//!
//! All state is behind [`InterruptGuard`]: system calls arrive with
//! interrupts masked, and the desktop (idle task) masks them before touching
//! the session.

use crate::arch::x86_64::serial;
use crate::arch::x86_64::timer::{self, InterruptGuard};
use crate::graphics;
use crate::input::Key;
use crate::scheduler;
use crate::serial_line;
use crate::syscall::{copy_from_user, copy_to_user, user_range_writable};
use aether_abi::user::{
    ARGUMENT_BLOCK_BYTES, MAX_SYSCALL_IO, PIPE_READ, PIPE_WRITE, PROCESS_RECORD_BYTES,
    ProcessRecord, SYSTEM_INFO_BYTES, SystemInfo, errno, fixed_name,
};
use aether_memory::frame::PhysFrame;
use aether_term::{CELL_HEIGHT, CELL_WIDTH, COLS, ROWS, Terminal};
use aether_wm::{Modifiers, Rect};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Set when output changed the grid and cleared when it is painted. The timer
/// tick reads it without the session lock to give the desktop a turn while a
/// busy program writes; [`needs_paint`] stays the authoritative check.
static PAINT_HINT: AtomicBool = AtomicBool::new(false);

/// Returns `true` when terminal output is waiting to be painted.
pub fn paint_hint() -> bool {
    PAINT_HINT.load(Ordering::Relaxed)
}

/// Pipes available to the session.
const MAX_PIPES: usize = 8;
/// Bytes buffered per pipe.
const PIPE_BYTES: usize = 1024;
/// Bytes of keyboard input queued for programs.
const INPUT_BYTES: usize = 256;
/// Standard stream selector for the terminal itself.
const TERMINAL_STREAM: u8 = 0;

/// Terminal attachment and standard I/O routing of one task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskIo {
    /// The task belongs to the terminal session.
    pub session: bool,
    /// Standard input: 0 for the terminal, otherwise a pipe id.
    pub stdin: u8,
    /// Standard output: 0 for the terminal, otherwise a pipe id.
    pub stdout: u8,
    /// Pipe read ends held explicitly (bit `id - 1`).
    pub read_ends: u8,
    /// Pipe write ends held explicitly (bit `id - 1`).
    pub write_ends: u8,
}

impl TaskIo {
    /// A task outside the terminal session.
    pub const NONE: Self = Self {
        session: false,
        stdin: TERMINAL_STREAM,
        stdout: TERMINAL_STREAM,
        read_ends: 0,
        write_ends: 0,
    };
}

#[derive(Clone, Copy)]
struct Pipe {
    used: bool,
    data: [u8; PIPE_BYTES],
    head: usize,
    len: usize,
    readers: u8,
    writers: u8,
}

impl Pipe {
    const EMPTY: Self = Self {
        used: false,
        data: [0; PIPE_BYTES],
        head: 0,
        len: 0,
        readers: 0,
        writers: 0,
    };
}

struct Session {
    terminal: Terminal,
    input: [u8; INPUT_BYTES],
    input_head: usize,
    input_len: usize,
    /// Process id of the session's shell, or 0 when none runs.
    leader: usize,
    /// The window is open and owns a session (running or ended).
    open: bool,
    pipes: [Pipe; MAX_PIPES],
    painted: u64,
    spawned: usize,
    /// Process that switched the terminal to raw input, or 0.
    raw_owner: usize,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: Every access holds an `InterruptGuard` on the single scheduling CPU.
unsafe impl<T> Sync for Global<T> {}

static SESSION: Global<Session> = Global(UnsafeCell::new(Session {
    terminal: Terminal::new(),
    input: [0; INPUT_BYTES],
    input_head: 0,
    input_len: 0,
    leader: 0,
    open: false,
    pipes: [Pipe::EMPTY; MAX_PIPES],
    painted: u64::MAX,
    spawned: 0,
    raw_owner: 0,
}));

fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> R {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the closure and no caller nests a
    // second session borrow.
    f(unsafe { &mut *SESSION.0.get() })
}

impl Session {
    fn push_input(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.input_len == INPUT_BYTES {
                return;
            }
            self.input[(self.input_head + self.input_len) % INPUT_BYTES] = byte;
            self.input_len += 1;
        }
    }

    fn pop_input(&mut self, out: &mut [u8]) -> usize {
        let count = self.input_len.min(out.len());
        for slot in out.iter_mut().take(count) {
            *slot = self.input[self.input_head];
            self.input_head = (self.input_head + 1) % INPUT_BYTES;
        }
        self.input_len -= count;
        count
    }

    fn output(&mut self, bytes: &[u8]) {
        self.terminal.write(bytes);
        PAINT_HINT.store(true, Ordering::Relaxed);
        let mut reply = [0u8; 32];
        let length = self.terminal.take_reply(&mut reply);
        self.push_input(&reply[..length]);
    }

    fn pipe(&mut self, id: u8) -> Option<&mut Pipe> {
        let index = usize::from(id).checked_sub(1)?;
        self.pipes.get_mut(index).filter(|pipe| pipe.used)
    }

    /// Drops one reference to a pipe end and frees the pipe when both sides
    /// are gone.
    fn release(&mut self, id: u8, write: bool) {
        if let Some(pipe) = self.pipe(id) {
            if write {
                pipe.writers = pipe.writers.saturating_sub(1);
            } else {
                pipe.readers = pipe.readers.saturating_sub(1);
            }
            if pipe.readers == 0 && pipe.writers == 0 {
                *pipe = Pipe::EMPTY;
            }
        }
    }

    fn retain(&mut self, id: u8, write: bool) {
        if let Some(pipe) = self.pipe(id) {
            if write {
                pipe.writers = pipe.writers.saturating_add(1);
            } else {
                pipe.readers = pipe.readers.saturating_add(1);
            }
        }
    }
}

const BANNER: &[u8] = b"\x1b[1;36mAether Terminal\x1b[0m\n\
Signed in as \x1b[1muser\x1b[0m. This session can reach your home folder only;\n\
system files and other applications are outside its authority.\n\
Type \x1b[1mhelp\x1b[0m for commands.\n\n";

/// Starts a new shell in the terminal window.
fn start_shell() {
    with_session(|session| {
        session.terminal.reset();
        session.input_len = 0;
        session.output(BANNER);
    });
    let io = TaskIo {
        session: true,
        ..TaskIo::NONE
    };
    match scheduler::spawn_terminal_program(0, b"/\0sh\0", io) {
        Ok(pid) => {
            with_session(|session| {
                session.leader = pid;
                session.spawned += 1;
            });
            serial_line!("terminal: session started shell pid={pid}");
        }
        Err(error) => {
            with_session(|session| {
                session.output(b"\x1b[31mterminal: could not start the shell\x1b[0m\n");
            });
            serial_line!("terminal: shell spawn failed error={error}");
        }
    }
}

/// Keeps the session in step with the Terminal window.
///
/// Called by the desktop every tick: opening the window starts a shell, and
/// closing it ends every program of the session.
pub fn sync_window(open: bool) {
    let was_open = with_session(|session| core::mem::replace(&mut session.open, open));
    if open && !was_open {
        start_shell();
    } else if !open && was_open {
        let killed = scheduler::kill_session(0, 128 + 1);
        with_session(|session| {
            session.leader = 0;
            session.input_len = 0;
        });
        serial_line!("terminal: window closed; ended {killed} program(s)");
    }
}

/// Returns `true` when the grid changed since it was last painted.
pub fn needs_paint() -> bool {
    with_session(|session| session.terminal.generation() != session.painted)
}

/// Records the current grid as painted without drawing it.
pub fn mark_painted() {
    PAINT_HINT.store(false, Ordering::Relaxed);
    with_session(|session| session.painted = session.terminal.generation());
}

/// Forwards a key pressed while the Terminal window has focus.
pub fn key(key: Key, modifiers: Modifiers) {
    let mut bytes = [0u8; 8];
    let length = aether_term::encode_key(key, modifiers, &mut bytes);
    if length == 0 {
        return;
    }
    let (leader, ended) = with_session(|session| (session.leader, session.leader == 0));
    if ended {
        if key == Key::Enter {
            start_shell();
        }
        return;
    }
    // Ctrl+C interrupts the running job; the shell itself only sees it at its
    // prompt, where it discards the line being edited. A full-screen program
    // in raw mode (an editor) receives it as an ordinary key.
    let raw = with_session(|session| session.raw_owner != 0);
    if bytes[..length] == [0x03] && !raw && scheduler::session_has_jobs(leader) {
        let killed = scheduler::kill_session(leader, 128 + 2);
        with_session(|session| {
            session.input_len = 0;
            session.output(b"^C\n");
        });
        serial_line!("terminal: interrupt ended {killed} program(s)");
        return;
    }
    with_session(|session| session.push_input(&bytes[..length]));
}

/// Releases a retired program's pipe ends and ends the session when its
/// shell exits.
pub fn task_exited(pid: usize, io: TaskIo, status: i64) {
    if !io.session {
        return;
    }
    let leader_exited = with_session(|session| {
        if io.stdin != TERMINAL_STREAM {
            session.release(io.stdin, false);
        }
        if io.stdout != TERMINAL_STREAM {
            session.release(io.stdout, true);
        }
        for bit in 0..MAX_PIPES as u8 {
            if io.read_ends & (1 << bit) != 0 {
                session.release(bit + 1, false);
            }
            if io.write_ends & (1 << bit) != 0 {
                session.release(bit + 1, true);
            }
        }
        // A full-screen program that was killed or faulted cannot restore the
        // screen itself, so the session does it for the shell.
        if status > 128 && session.terminal.alternate_screen() {
            session.output(b"\x1b[0m\x1b[?1049l\x1b[?25h");
        }
        if status > 128 && status != 128 + 2 && status != 128 + 9 {
            let message: &[u8] = if status == 128 + 7 {
                b"\x1b[31mBus error\x1b[0m\n"
            } else {
                b"\x1b[31mSegmentation fault\x1b[0m (the program was stopped; the system is fine)\n"
            };
            session.output(message);
        }
        if session.raw_owner == pid {
            session.raw_owner = 0;
        }
        if session.leader == pid {
            session.leader = 0;
            session.output(
                b"\x1b[0m\n\x1b[2m[shell exited \xc2\xb7 press Enter to start a new one]\x1b[0m\n",
            );
            true
        } else {
            false
        }
    });
    if leader_exited {
        // The shell's jobs go with it. Retiring them re-enters this function,
        // which is safe: the session lock is not held here.
        let _ = scheduler::kill_session(pid, 128 + 1);
    }
}

/// Paints the grid into the Terminal window's content rectangle.
pub fn paint(content: Rect) {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked, so no system call can write the grid
    // while it is sampled.
    let session = unsafe { &mut *SESSION.0.get() };
    let padding = aether_wm::layout::TERMINAL_PADDING;
    graphics::fill_rect(
        content.x,
        content.y,
        content.width,
        content.height,
        aether_term::DEFAULT_BG,
    );
    let origin_x = content.x + padding;
    let origin_y = content.y + padding;
    let (cursor_x, cursor_y) = session.terminal.cursor();
    let show_cursor = session.terminal.cursor_visible() && session.leader != 0;
    let columns = (((content.width - padding) / CELL_WIDTH as i32).max(0) as usize).min(COLS);
    let rows = (((content.height - padding) / CELL_HEIGHT as i32).max(0) as usize).min(ROWS);
    for row in 0..rows {
        for column in 0..columns {
            let cell = session.terminal.cell(column, row);
            let (mut fg, mut bg) = cell.colors();
            if show_cursor && column == cursor_x && row == cursor_y {
                core::mem::swap(&mut fg, &mut bg);
            }
            if cell.code == b' ' && bg == aether_term::DEFAULT_BG {
                continue;
            }
            graphics::draw_glyph_cell(
                origin_x + (column * CELL_WIDTH) as i32,
                origin_y + (row * CELL_HEIGHT) as i32,
                aether_term::glyph(cell.code),
                fg,
                bg,
                cell.attr & aether_term::ATTR_UNDERLINE != 0,
            );
        }
    }
    PAINT_HINT.store(false, Ordering::Relaxed);
    session.painted = session.terminal.generation();
}

/// Writes standard output for the current task. Returns `None` when the task
/// is not a terminal program, so the caller keeps the serial console path.
pub fn write_stdout(bytes: &[u8]) -> Option<i64> {
    let io = scheduler::current_io();
    if !io.session {
        return None;
    }
    if io.stdout == TERMINAL_STREAM {
        with_session(|session| session.output(bytes));
        return Some(bytes.len() as i64);
    }
    Some(pipe_write(io.stdout, bytes))
}

fn pipe_write(id: u8, bytes: &[u8]) -> i64 {
    with_session(|session| {
        let Some(pipe) = session.pipe(id) else {
            return errno::EBADF;
        };
        if pipe.readers == 0 {
            return errno::EPIPE;
        }
        let room = PIPE_BYTES - pipe.len;
        if room == 0 {
            return errno::EAGAIN;
        }
        let count = room.min(bytes.len());
        for &byte in &bytes[..count] {
            pipe.data[(pipe.head + pipe.len) % PIPE_BYTES] = byte;
            pipe.len += 1;
        }
        count as i64
    })
}

fn pipe_read(id: u8, out: &mut [u8]) -> i64 {
    with_session(|session| {
        let Some(pipe) = session.pipe(id) else {
            return errno::EBADF;
        };
        if pipe.len == 0 {
            return if pipe.writers == 0 { 0 } else { errno::EAGAIN };
        }
        let count = pipe.len.min(out.len());
        for slot in out.iter_mut().take(count) {
            *slot = pipe.data[pipe.head];
            pipe.head = (pipe.head + 1) % PIPE_BYTES;
        }
        pipe.len -= count;
        count as i64
    })
}

fn bit(id: u8) -> u8 {
    1 << (id - 1)
}

fn valid_pipe(id: u64) -> Option<u8> {
    u8::try_from(id)
        .ok()
        .filter(|&id| id >= 1 && usize::from(id) <= MAX_PIPES)
}

/// Copies a kernel result buffer to user memory.
fn copy_out(pml4: PhysFrame, pointer: u64, bytes: &[u8]) -> i64 {
    match copy_to_user(pml4, pointer, bytes) {
        Some(()) => bytes.len() as i64,
        None => errno::EINVAL,
    }
}

/// `read_input`: standard input from the terminal or a pipe.
pub fn sys_read_input(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    let io = scheduler::current_io();
    if !io.session {
        return errno::EACCES;
    }
    if !user_range_writable(pml4, pointer, length) {
        return errno::EINVAL;
    }
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let out = &mut storage[..length as usize];
    let count = if io.stdin == TERMINAL_STREAM {
        let count = with_session(|session| session.pop_input(out));
        if count == 0 {
            return errno::EAGAIN;
        }
        count as i64
    } else {
        pipe_read(io.stdin, out)
    };
    if count <= 0 {
        return count;
    }
    copy_out(pml4, pointer, &out[..count as usize])
}

/// `terminal_size`: `(columns << 16) | rows` for session programs.
pub fn sys_terminal_size() -> i64 {
    if scheduler::current_io().session {
        ((COLS as i64) << 16) | ROWS as i64
    } else {
        errno::EACCES
    }
}

/// `terminal_mode`: query, or with `set == 1` change, raw input handling.
pub fn sys_terminal_mode(set: u64, mode: u64) -> i64 {
    let io = scheduler::current_io();
    if !io.session {
        return errno::EACCES;
    }
    if set > 1 || mode & !aether_abi::user::TERMINAL_RAW != 0 {
        return errno::EINVAL;
    }
    let pid = scheduler::current_task();
    let raw = with_session(|session| {
        let raw = session.raw_owner != 0;
        if set == 1 {
            if mode & aether_abi::user::TERMINAL_RAW != 0 {
                session.raw_owner = pid;
            } else if session.raw_owner == pid {
                session.raw_owner = 0;
            }
        }
        raw
    });
    let mut result = u64::from(raw);
    if io.stdin == TERMINAL_STREAM {
        result |= aether_abi::user::STDIN_IS_TERMINAL;
    }
    if io.stdout == TERMINAL_STREAM {
        result |= aether_abi::user::STDOUT_IS_TERMINAL;
    }
    result as i64
}

/// `pipe_create`: a new pipe whose two ends the caller holds.
pub fn sys_pipe_create() -> i64 {
    if !scheduler::current_io().session {
        return errno::EACCES;
    }
    let id = with_session(|session| {
        let index = session.pipes.iter().position(|pipe| !pipe.used)?;
        session.pipes[index] = Pipe {
            used: true,
            readers: 1,
            writers: 1,
            ..Pipe::EMPTY
        };
        Some(index as u8 + 1)
    });
    let Some(id) = id else {
        return errno::ENOSPC;
    };
    scheduler::with_current_io(|io| {
        io.read_ends |= bit(id);
        io.write_ends |= bit(id);
    });
    i64::from(id)
}

/// `pipe_read`: read from a pipe whose read end the caller holds.
pub fn sys_pipe_read(pml4: PhysFrame, id: u64, pointer: u64, length: u64) -> i64 {
    let Some(id) = valid_pipe(id) else {
        return errno::EBADF;
    };
    if scheduler::current_io().read_ends & bit(id) == 0 {
        return errno::EBADF;
    }
    if !user_range_writable(pml4, pointer, length) {
        return errno::EINVAL;
    }
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let out = &mut storage[..length as usize];
    let count = pipe_read(id, out);
    if count <= 0 {
        return count;
    }
    copy_out(pml4, pointer, &out[..count as usize])
}

/// `pipe_write`: write to a pipe whose write end the caller holds.
pub fn sys_pipe_write(pml4: PhysFrame, id: u64, pointer: u64, length: u64) -> i64 {
    let Some(id) = valid_pipe(id) else {
        return errno::EBADF;
    };
    if scheduler::current_io().write_ends & bit(id) == 0 {
        return errno::EBADF;
    }
    if length as usize > MAX_SYSCALL_IO {
        return errno::EINVAL;
    }
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let input = &mut storage[..length as usize];
    if copy_from_user(pml4, pointer, input).is_none() {
        return errno::EINVAL;
    }
    pipe_write(id, input)
}

/// `pipe_close`: release one end the caller holds.
pub fn sys_pipe_close(id: u64, end: u64) -> i64 {
    let Some(id) = valid_pipe(id) else {
        return errno::EBADF;
    };
    let write = match end {
        PIPE_READ => false,
        PIPE_WRITE => true,
        _ => return errno::EINVAL,
    };
    let held = scheduler::with_current_io(|io| {
        let ends = if write {
            &mut io.write_ends
        } else {
            &mut io.read_ends
        };
        let held = *ends & bit(id) != 0;
        *ends &= !bit(id);
        held
    });
    if !held {
        return errno::EBADF;
    }
    with_session(|session| session.release(id, write));
    0
}

/// `spawn`: start a terminal program as the caller's child.
pub fn sys_spawn(pml4: PhysFrame, pointer: u64, length: u64, stdin: u64, stdout: u64) -> i64 {
    let caller = scheduler::current_io();
    if !caller.session {
        return errno::EACCES;
    }
    if length == 0 || length as usize > ARGUMENT_BLOCK_BYTES {
        return errno::EINVAL;
    }
    let mut storage = [0u8; ARGUMENT_BLOCK_BYTES];
    let args = &mut storage[..length as usize];
    if copy_from_user(pml4, pointer, args).is_none() {
        return errno::EINVAL;
    }
    // A child's standard stream must be the terminal or a pipe end the
    // caller holds; it cannot name a pipe it was never given.
    let stream = |value: u64, ends: u8| -> Option<u8> {
        if value == 0 {
            return Some(TERMINAL_STREAM);
        }
        valid_pipe(value).filter(|&id| ends & bit(id) != 0)
    };
    let (Some(stdin), Some(stdout)) = (
        stream(stdin, caller.read_ends),
        stream(stdout, caller.write_ends),
    ) else {
        return errno::EBADF;
    };
    let io = TaskIo {
        session: true,
        stdin,
        stdout,
        read_ends: 0,
        write_ends: 0,
    };
    match scheduler::spawn_terminal_program(scheduler::current_task(), args, io) {
        Ok(pid) => {
            with_session(|session| {
                if stdin != TERMINAL_STREAM {
                    session.retain(stdin, false);
                }
                if stdout != TERMINAL_STREAM {
                    session.retain(stdout, true);
                }
                session.spawned += 1;
            });
            pid as i64
        }
        Err(error) => error,
    }
}

/// `arguments`: the caller's argument block.
pub fn sys_arguments(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    if !user_range_writable(pml4, pointer, length) {
        return errno::EINVAL;
    }
    let mut storage = [0u8; ARGUMENT_BLOCK_BYTES];
    let count = scheduler::current_args(&mut storage).min(length as usize);
    copy_out(pml4, pointer, &storage[..count])
}

/// `process_list`: one record per task.
pub fn sys_process_list(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    if !scheduler::current_io().session {
        return errno::EACCES;
    }
    if !user_range_writable(pml4, pointer, length) {
        return errno::EINVAL;
    }
    let mut records = [ProcessRecord {
        pid: 0,
        parent: 0,
        state: 0,
        kind: 0,
        flags: 0,
        memory_kib: 0,
        cpu_ticks: 0,
        name: [0; aether_abi::user::PROCESS_NAME_BYTES],
    }; MAX_SYSCALL_IO / PROCESS_RECORD_BYTES];
    let room = (length as usize / PROCESS_RECORD_BYTES).min(records.len());
    let count = scheduler::process_records(&mut records[..room]);
    let mut storage = [0u8; MAX_SYSCALL_IO];
    for (index, record) in records[..count].iter().enumerate() {
        let start = index * PROCESS_RECORD_BYTES;
        storage[start..start + PROCESS_RECORD_BYTES].copy_from_slice(&record.encode());
    }
    match copy_to_user(pml4, pointer, &storage[..count * PROCESS_RECORD_BYTES]) {
        Some(()) => count as i64,
        None => errno::EINVAL,
    }
}

/// `system_info`: uptime, memory, task, and filesystem counters.
pub fn sys_system_info(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    if (length as usize) < SYSTEM_INFO_BYTES {
        return errno::EINVAL;
    }
    let (total_frames, free_frames) = crate::memory::with_frame_allocator(|allocator| {
        let stats = allocator.stats();
        (stats.total_frames, stats.free_frames)
    })
    .unwrap_or((0, 0));
    let (tasks, max_tasks, idle_ticks) = scheduler::task_summary();
    let usage = crate::fs::usage();
    let block = aether_fs::BLOCK_SIZE as u32;
    let info = SystemInfo {
        uptime_ticks: scheduler::ticks(),
        tick_hz: timer::TIMER_HZ,
        cpu_count: 1,
        memory_total_kib: match crate::memory::USABLE_BYTES
            .load(core::sync::atomic::Ordering::Relaxed)
        {
            0 => total_frames * 4,
            bytes => bytes / 1024,
        },
        memory_free_kib: free_frames * 4,
        idle_ticks,
        tasks: tasks as u32,
        max_tasks: max_tasks as u32,
        fs_total_bytes: usage.map_or(0, |usage| usage.blocks as u32 * block),
        fs_used_bytes: usage.map_or(0, |usage| usage.used_blocks as u32 * block),
        fs_inodes: usage.map_or(0, |usage| usage.inodes as u32),
        fs_inodes_used: usage.map_or(0, |usage| usage.used_inodes as u32),
        version: fixed_name(env!("CARGO_PKG_VERSION")),
        commit: fixed_name(option_env!("AETHER_GIT_COMMIT").unwrap_or("unknown")),
    };
    copy_out(pml4, pointer, &info.encode())
}

/// Console diagnostics for the boot tests: `term open|close|type|dump|keys`.
pub fn console_command(argument: &str) {
    let (verb, rest) = argument.split_once(' ').unwrap_or((argument, ""));
    match verb {
        "open" => {
            crate::desktop::open_terminal();
            serial_line!(
                "term: open leader={}",
                with_session(|session| session.leader)
            );
        }
        "close" => {
            crate::desktop::close_terminal();
            serial_line!("term: closed");
        }
        "type" => {
            // `\n` is Enter, `\e` Escape, `\c` Ctrl+C, `\xNN` any byte.
            let mut bytes = [0u8; 128];
            let mut length = 0;
            let mut source = rest.bytes();
            while let Some(byte) = source.next() {
                let value = if byte == b'\\' {
                    match source.next() {
                        Some(b'n') => b'\r',
                        Some(b'e') => 0x1b,
                        Some(b's') => b' ',
                        Some(b'x') => {
                            let high = source.next().and_then(hex);
                            let low = source.next().and_then(hex);
                            match (high, low) {
                                (Some(high), Some(low)) => high << 4 | low,
                                _ => continue,
                            }
                        }
                        Some(b'c') => {
                            key(
                                Key::Char(b'c'),
                                Modifiers {
                                    ctrl: true,
                                    ..Modifiers::default()
                                },
                            );
                            continue;
                        }
                        Some(other) => other,
                        None => break,
                    }
                } else {
                    byte
                };
                if length < bytes.len() {
                    bytes[length] = value;
                    length += 1;
                }
            }
            with_session(|session| session.push_input(&bytes[..length]));
            serial_line!("term: typed {length} byte(s)");
        }
        "enter" => {
            key(Key::Enter, Modifiers::default());
            serial_line!("term: enter");
        }
        "dump" => dump(),
        _ => serial_line!("term: usage: term open|close|type <text>|enter|dump"),
    }
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|value| value as u8)
}

fn dump() {
    let (rows, cursor, alternate, leader, spawned) = with_session(|session| {
        let mut rows = [[0u8; COLS]; ROWS];
        let mut lengths = [0usize; ROWS];
        for (row, line) in rows.iter_mut().enumerate() {
            lengths[row] = session.terminal.row_text(row, line);
        }
        (
            (rows, lengths),
            session.terminal.cursor(),
            session.terminal.alternate_screen(),
            session.leader,
            session.spawned,
        )
    });
    let ((lines, lengths), (x, y)) = (rows, cursor);
    serial_line!(
        "term: screen cursor={x},{y} alternate={alternate} leader={leader} spawned={spawned}"
    );
    for (line, length) in lines.iter().zip(lengths) {
        serial::write_str("term| ");
        serial::write_bytes(&line[..length]);
        serial::write_str("\n");
    }
    serial_line!("term: end");
}

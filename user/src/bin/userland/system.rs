// SPDX-License-Identifier: Apache-2.0
//! System commands: `ps`, `kill`, `uptime`, `free`, `df`, `uname`, `clear`,
//! `whoami`, `id`, `hostname`, `sleep`, `env`, and `help`.

use crate::files::Options;
use crate::rt::{self, Args, Out};
use aether_abi::user::{
    PROCESS_FLAG_SESSION, PROCESS_NAME_BYTES, ProcessKind, ProcessRecord, ProcessState, SystemInfo,
};
use aether_sdk as sdk;

/// Most processes a listing holds (the kernel's task table is smaller).
pub const MAX_PROCESSES: usize = 96;

pub const EMPTY_RECORD: ProcessRecord = ProcessRecord {
    pid: 0,
    parent: 0,
    state: 0,
    kind: 0,
    flags: 0,
    memory_kib: 0,
    cpu_ticks: 0,
    name: [0; PROCESS_NAME_BYTES],
};

/// One-letter state, as `ps` prints it.
pub fn state_letter(state: u8) -> char {
    match state {
        x if x == ProcessState::Running as u8 => 'R',
        x if x == ProcessState::Ready as u8 => 'R',
        x if x == ProcessState::Blocked as u8 => 'S',
        x if x == ProcessState::Exited as u8 => 'Z',
        _ => 'X',
    }
}

/// The identity a process runs as.
pub fn owner(record: &ProcessRecord) -> &'static str {
    match record.kind {
        x if x == ProcessKind::Terminal as u8 => "user",
        x if x == ProcessKind::Linux as u8 || x == ProcessKind::LinuxThread as u8 => "app",
        x if x == ProcessKind::Native as u8 => "app",
        x if x == ProcessKind::Service as u8 => "service",
        _ => "kernel",
    }
}

/// Whether a listing shows `record`. Boot-time tasks that finished keep
/// their table slot, but they are not processes any more; a terminal program
/// that exited still shows as a zombie until the shell collects it.
pub fn listed(record: &ProcessRecord) -> bool {
    let finished =
        record.state == ProcessState::Exited as u8 || record.state == ProcessState::Faulted as u8;
    !finished || record.kind == ProcessKind::Terminal as u8
}

pub fn system_info() -> SystemInfo {
    sdk::system_info().unwrap_or_default()
}

fn field(bytes: &[u8]) -> &str {
    let length = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..length]).unwrap_or("?")
}

pub fn ps(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    // `ps` shows the session; `-e`/`-A`/`aux` show every task.
    let all = options.has(b'e')
        || options.has(b'A')
        || options
            .operands()
            .iter()
            .any(|operand| operand.contains('a'));
    let mut records = [EMPTY_RECORD; MAX_PROCESSES];
    let count = sdk::process_list(&mut records).unwrap_or(0);
    let info = system_info();
    let hz = u64::from(info.tick_hz.max(1));
    outln!(out, "  PID  PPID USER     S   RSS      TIME CMD");
    for record in &records[..count] {
        if !listed(record) || (!all && record.flags & PROCESS_FLAG_SESSION == 0) {
            continue;
        }
        let seconds = record.cpu_ticks / hz;
        out!(
            out,
            "{:>5} {:>5} {:<8} {} {:>5} {:>2}:{:02}:{:02} ",
            record.pid,
            record.parent,
            owner(record),
            state_letter(record.state),
            if record.memory_kib == 0 {
                rt::Human(0)
            } else {
                rt::Human(u64::from(record.memory_kib) * 1024)
            },
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
        );
        outln!(out, "{}", record.name());
    }
    0
}

pub fn kill(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut status = 0;
    let mut any = false;
    for operand in args.iter() {
        // Signals are not modelled: every kill terminates, as SIGKILL does.
        if operand.starts_with('-') {
            continue;
        }
        any = true;
        let Ok(pid) = operand.parse::<u64>() else {
            outln!(out, "kill: {operand}: arguments must be process IDs");
            status = 1;
            continue;
        };
        if let Err(error) = sdk::kill(pid) {
            out!(out, "kill: ({pid}) - ");
            if error == aether_abi::user::errno::EACCES {
                outln!(
                    out,
                    "Operation not permitted (not in this terminal session)"
                );
            } else {
                outln!(out, "{}", sdk::error_message(error));
            }
            status = 1;
        }
    }
    if !any {
        outln!(out, "usage: kill PID...");
        return 2;
    }
    status
}

/// CPU busy share over the whole uptime, in percent.
fn load(info: &SystemInfo) -> u64 {
    let total = info.uptime_ticks.max(1);
    total.saturating_sub(info.idle_ticks) * 100 / total
}

pub fn uptime(args: &Args) -> i64 {
    let mut out = Out::new();
    let info = system_info();
    let duration = rt::Duration {
        ticks: info.uptime_ticks,
        hz: u64::from(info.tick_hz),
    };
    if args.get(0) == "-p" {
        outln!(out, "up {duration}");
        return 0;
    }
    outln!(
        out,
        " up {duration},  1 user,  {} tasks,  cpu busy {}%",
        info.tasks,
        load(&info)
    );
    0
}

pub fn free(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let info = system_info();
    let used = info.memory_total_kib.saturating_sub(info.memory_free_kib);
    outln!(out, "               total        used        free");
    if options.has(b'h') {
        outln!(
            out,
            "Mem:     {:>11} {:>11} {:>11}",
            rt::Human(info.memory_total_kib * 1024),
            rt::Human(used * 1024),
            rt::Human(info.memory_free_kib * 1024)
        );
    } else {
        let scale = if options.has(b'm') { 1024 } else { 1 };
        outln!(
            out,
            "Mem:     {:>11} {:>11} {:>11}",
            info.memory_total_kib / scale,
            used / scale,
            info.memory_free_kib / scale
        );
    }
    outln!(out, "Swap:              0           0           0");
    0
}

pub fn df(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let info = system_info();
    let total = u64::from(info.fs_total_bytes);
    let used = u64::from(info.fs_used_bytes);
    let percent = (used * 100).checked_div(total).unwrap_or(0);
    if options.has(b'i') {
        outln!(out, "Filesystem     Inodes IUsed IFree IUse% Mounted on");
        let free = info.fs_inodes.saturating_sub(info.fs_inodes_used);
        let percent = info.fs_inodes_used * 100 / info.fs_inodes.max(1);
        outln!(
            out,
            "aetherfs   {:>10} {:>5} {:>5} {:>4}% /",
            info.fs_inodes,
            info.fs_inodes_used,
            free,
            percent
        );
        return 0;
    }
    outln!(out, "Filesystem       Size   Used  Avail Use% Mounted on");
    outln!(
        out,
        "aetherfs     {:>8} {:>6} {:>6} {:>3}% /",
        rt::Human(total),
        rt::Human(used),
        rt::Human(total - used),
        percent
    );
    0
}

pub fn uname(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let info = system_info();
    let all = options.has(b'a');
    let none = !b"snrvm".iter().any(|&letter| options.has(letter));
    let mut first = true;
    let mut put = |out: &mut Out, wanted: bool, text: &str| {
        if wanted || all {
            if !first {
                out.str(" ");
            }
            out.str(text);
            first = false;
        }
    };
    put(&mut out, options.has(b's') || none, "Aether");
    put(&mut out, options.has(b'n'), "aether");
    put(&mut out, options.has(b'r'), field(&info.version));
    let mut version = [0u8; 24];
    version[..3].copy_from_slice(b"#1 ");
    let commit = field(&info.commit).as_bytes();
    let length = 3 + commit.len().min(version.len() - 3);
    version[3..length].copy_from_slice(&commit[..length - 3]);
    put(&mut out, options.has(b'v'), field(&version[..length]));
    put(&mut out, options.has(b'm'), "x86_64");
    outln!(out);
    0
}

pub fn clear(_args: &Args) -> i64 {
    rt::write_all(b"\x1b[H\x1b[2J");
    0
}

pub fn whoami(_args: &Args) -> i64 {
    rt::write_all(b"user\n");
    0
}

pub fn id(_args: &Args) -> i64 {
    rt::write_all(b"uid=1000(user) gid=1000(user) groups=1000(user) scope=home elevated=no\n");
    0
}

pub fn hostname(_args: &Args) -> i64 {
    rt::write_all(b"aether\n");
    0
}

pub fn sleep(args: &Args) -> i64 {
    let mut out = Out::new();
    let info = system_info();
    let hz = u64::from(info.tick_hz.max(1));
    let text = args.get(0);
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let Ok(seconds) = whole.parse::<u64>() else {
        outln!(out, "sleep: invalid time interval '{text}'");
        return 1;
    };
    let tenths = fraction
        .bytes()
        .next()
        .map_or(0, |digit| u64::from(digit.wrapping_sub(b'0')).min(9));
    let deadline = sdk::uptime() + seconds * hz + tenths * hz / 10;
    while sdk::uptime() < deadline {
        let _ = sdk::sleep(1);
    }
    0
}

pub fn env(args: &Args) -> i64 {
    let mut out = Out::new();
    outln!(out, "USER=user");
    outln!(out, "HOME=/");
    outln!(out, "PWD={}", args.cwd());
    outln!(out, "SHELL=/bin/sh");
    outln!(out, "TERM=xterm");
    outln!(out, "HOSTNAME=aether");
    0
}

pub fn help(_args: &Args) -> i64 {
    let mut out = Out::new();
    out.str(
        "\x1b[1mAether Terminal\x1b[0m: Rust reimplementations of the usual Linux tools.\n\n\
         \x1b[1mFiles\x1b[0m    ls cat cp mv rm mkdir rmdir touch stat tree find du pwd\n\
         \x1b[1mText\x1b[0m     echo head tail wc grep sort uniq tee seq yes basename dirname\n\
         \x1b[1mSystem\x1b[0m   ps kill uptime free df uname whoami id hostname sleep env clear\n\
         \x1b[1mApps\x1b[0m     htop (top)  nano\n\
         \x1b[1mShell\x1b[0m    cd pwd exit history; pipes |, redirection > >> <, ; && ||,\n\
         \x20        $? and quotes. Tab completes, Up/Down recall, Ctrl+C interrupts.\n\n\
         You are signed in as an ordinary user: / is your home folder, and only\n\
         programs started in this terminal can be killed from it.\n",
    );
    0
}

// SPDX-License-Identifier: Apache-2.0
//! Console shell.
//!
//! The shell runs in the idle task at ring 0. It is a **diagnostic surface
//! with the kernel's ambient authority**: filesystem commands act through the
//! filesystem service's kernel console root and can read, replace, and delete
//! any file, and `portal revoke` changes grant state. This is why the shell
//! is not the ordinary-user terminal the specification requires: it provides
//! no elevation boundary, no per-user identity, and no scoping. The
//! specification's user terminal is a planned user-space component; until it
//! exists, administrative commands must be treated as system administration.
//!
//! Commands are intentionally small and bounded; the shell never accepts a
//! capability from an application and cannot inspect another process's
//! capability table.

use crate::arch::x86_64::serial;
use crate::serial_line;
use crate::{apps, fs, scheduler, supervisor, syscall};

const MAX_LINE: usize = 128;

static mut LINE: [u8; MAX_LINE] = [0; MAX_LINE];
static mut LINE_LEN: usize = 0;

/// Prints the boot greeting and the shell-ready marker.
pub fn init() {
    serial_line!("Aether console shell. Type 'help' for commands.");
    serial_line!("AETHER_SHELL_OK");
    prompt();
}

fn prompt() {
    serial::write_str("\naether> ");
}

/// Polls the UART and executes complete lines.
///
/// Called from the idle loop with interrupts enabled. It performs bounded work
/// per call and never blocks on input.
pub fn poll() {
    let mut consumed = 0usize;
    while consumed < 32 {
        let Some(byte) = serial::try_read() else {
            break;
        };
        consumed += 1;
        match byte {
            b'\r' | b'\n' => {
                serial::write_byte(b'\r');
                serial::write_byte(b'\n');
                execute_line();
                prompt();
            }
            0x08 | 0x7f => {
                // SAFETY: The static buffer is only touched from this loop.
                let length = unsafe { LINE_LEN };
                if length > 0 {
                    // SAFETY: `length` is bounded by the buffer size.
                    unsafe { LINE_LEN = length - 1 };
                    serial::write_str("\x08 \x08");
                }
            }
            byte if byte.is_ascii_graphic() || byte == b' ' => {
                // SAFETY: The buffer and length are single-threaded shell state.
                let length = unsafe { LINE_LEN };
                if length < MAX_LINE {
                    // SAFETY: Bounds checked above.
                    unsafe {
                        LINE[length] = byte;
                        LINE_LEN = length + 1;
                    }
                    serial::write_byte(byte);
                }
            }
            _ => {}
        }
    }
}

fn execute_line() {
    // SAFETY: The line buffer and length are single-threaded shell state.
    let length = unsafe { LINE_LEN };
    let line =
        unsafe { core::slice::from_raw_parts(core::ptr::addr_of!(LINE).cast::<u8>(), length) };
    unsafe { LINE_LEN = 0 };
    let line = core::str::from_utf8(line).unwrap_or("").trim();
    if line.is_empty() {
        return;
    }
    let (command, argument) = match line.split_once(' ') {
        Some((command, argument)) => (command, argument.trim()),
        None => (line, ""),
    };

    match command {
        "help" => print_help(),
        "ls" => list(if argument.is_empty() { "/" } else { argument }),
        "cat" => cat(argument),
        "write" => write(argument),
        "rm" => remove(argument),
        "apps" => apps::print_table(),
        "services" => supervisor::print_security(),
        "portal" => portal_command(argument),
        "security" => security(),
        "input" => serial_line!("input: dropped={}", crate::input::dropped()),
        "mem" => memory(),
        "uptime" => serial_line!("uptime: {} ticks", scheduler::ticks()),
        "recovery" => recovery(),
        "packages" => crate::packaging::print_status(),
        "runtime" => crate::runtime::print_status(),
        "updates" => crate::updates::print_status(),
        "desktop" => desktop(),
        "clear" => serial::write_str("\x1b[2J\x1b[H"),
        _ => serial_line!("shell: unknown command '{command}'"),
    }
}

fn print_help() {
    serial_line!("commands:");
    serial_line!("  ls [path]        list a directory");
    serial_line!("  cat <path>       print a file");
    serial_line!("  write <path> <text>  replace a file");
    serial_line!("  rm <path>        delete a file");
    serial_line!("  apps             application lifecycle table");
    serial_line!("  services         service profiles and states");
    serial_line!("  portal           list file portal grants");
    serial_line!("  portal revoke N  revoke grant N (open handles stay valid)");
    serial_line!("  security         permission and sandbox status");
    serial_line!("  input            keyboard queue status");
    serial_line!("  mem              memory pressure (MiB free/total)");
    serial_line!("  uptime           timer ticks");
    serial_line!("  recovery         system image and rollback status");
    serial_line!("  packages         installed package and verification status");
    serial_line!("  runtime          per-application runtime profiles");
    serial_line!("  updates          signed update and rollback status");
    serial_line!("  desktop          compositor and desktop status");
    serial_line!("  clear            clear the terminal");
}

fn list(path: &str) {
    let mut buffer = [0u8; 512];
    match fs::list_path(path, &mut buffer) {
        Ok(length) => serial::write_bytes(&buffer[..length]),
        Err(error) => serial_line!("ls: {error:?}"),
    }
}

fn cat(path: &str) {
    if path.is_empty() {
        serial_line!("cat: missing path");
        return;
    }
    let mut buffer = [0u8; 512];
    match fs::read_path(path, &mut buffer) {
        Ok(length) => {
            serial::write_bytes(&buffer[..length]);
            if !buffer[..length].ends_with(b"\n") {
                serial::write_str("\n");
            }
        }
        Err(error) => serial_line!("cat: {error:?}"),
    }
}

fn write(arguments: &str) {
    let Some((path, text)) = arguments.split_once(' ') else {
        serial_line!("write: usage: write <path> <text>");
        return;
    };
    match fs::write_path(path, text.as_bytes()) {
        Ok(()) => serial_line!("write: ok path={path} bytes={}", text.len()),
        Err(error) => serial_line!("write: {error:?}"),
    }
}

fn remove(path: &str) {
    if path.is_empty() {
        serial_line!("rm: missing path");
        return;
    }
    match fs::remove_path(path) {
        Ok(()) => serial_line!("rm: ok path={path}"),
        Err(error) => serial_line!("rm: {error:?}"),
    }
}

fn portal_command(arguments: &str) {
    let mut parts = arguments.split_whitespace();
    match parts.next() {
        None => crate::portal::print_grants(),
        Some("revoke") => {
            let Some(index) = parts.next().and_then(|value| value.parse::<usize>().ok()) else {
                serial_line!("portal: usage: portal [revoke <index>]");
                return;
            };
            if parts.next().is_some() {
                serial_line!("portal: usage: portal [revoke <index>]");
                return;
            }
            match crate::portal::revoke_index(index) {
                Ok(()) => serial_line!("portal: revoked grant {index}"),
                Err(_) => serial_line!("portal: no active grant {index}"),
            }
        }
        Some(_) => serial_line!("portal: usage: portal [revoke <index>]"),
    }
}

fn security() {
    serial_line!(
        "security: open_files={} escapes_denied={} access_denied={} quota_denied={}",
        fs::open_count(),
        fs::escape_denials(),
        fs::access_denials(),
        fs::quota_denials()
    );
    serial_line!(
        "security: demo_root={} other_root={}",
        fs::DEMO_ROOT,
        fs::OTHER_ROOT
    );
    serial_line!(
        "security: portal_grants={} portal_uses={} portal_denied={}",
        crate::portal::grant_count(),
        crate::portal::grant_uses(),
        crate::portal::denials()
    );
    serial_line!(
        "security: user_exits={} user_faults={} ipc_denied={}",
        scheduler::user_exits(),
        scheduler::user_faults(),
        syscall::ipc_denials()
    );
    supervisor::print_security();
    crate::portal::print_grants();
    apps::print_table();
    crate::desktop::redraw();
    serial_line!("security: desktop_focus={}", crate::desktop::focus());
}

fn memory() {
    let (free, total) = scheduler::mem_status();
    serial_line!("mem: free={free}MiB total={total}MiB");
}

fn recovery() {
    crate::recovery::print_status();
    serial_line!("recovery: user data is outside the image set");
}

fn desktop() {
    crate::desktop::redraw();
    serial_line!(
        "desktop: focus={} checksum={:#010x} mode={}x{}",
        crate::desktop::focus(),
        crate::graphics::checksum(),
        crate::graphics::width(),
        crate::graphics::height()
    );
}

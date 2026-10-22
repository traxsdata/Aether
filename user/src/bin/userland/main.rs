// SPDX-License-Identifier: Apache-2.0
//! The Aether terminal userland: the shell and its commands.
//!
//! This is one multi-call image, as BusyBox and uutils are: the kernel embeds
//! it once and starts it for every terminal program, and the program name in
//! the argument block selects the entry point. Each command is still its own
//! ring-3 process with its own address space, started by the shell through
//! the `spawn` system call and connected to the terminal or a pipe.
//!
//! The commands reimplement the common Linux tools in `no_std` Rust over the
//! native Aether ABI. They run as the ordinary user, so they only see the
//! user's home folder and cannot act on processes outside the session.

#![no_std]
#![no_main]

use aether_sdk as sdk;

/// Writes formatted text to an [`rt::Out`].
macro_rules! out {
    ($out:expr, $($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = write!($out, $($arg)*);
    }};
}

/// Writes a formatted line to an [`rt::Out`].
macro_rules! outln {
    ($out:expr) => {{
        $out.str("\n");
    }};
    ($out:expr, $($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($out, $($arg)*);
    }};
}

mod files;
mod htop;
mod nano;
mod rt;
mod sh;
mod system;
mod text;

/// Runs the program named by the argument block and returns its status.
fn run(args: &rt::Args) -> i64 {
    match args.program() {
        "sh" => sh::main(args),
        "ls" => files::ls(args),
        "cat" => files::cat(args),
        "cp" => files::cp(args),
        "mv" => files::mv(args),
        "rm" => files::rm(args),
        "mkdir" => files::mkdir(args),
        "rmdir" => files::rmdir(args),
        "touch" => files::touch(args),
        "stat" => files::stat(args),
        "tree" => files::tree(args),
        "find" => files::find(args),
        "du" => files::du(args),
        "df" => system::df(args),
        "pwd" => files::pwd(args),
        "echo" => text::echo(args),
        "head" => text::head(args),
        "tail" => text::tail(args),
        "wc" => text::wc(args),
        "grep" => text::grep(args),
        "sort" => text::sort(args),
        "uniq" => text::uniq(args),
        "tee" => text::tee(args),
        "seq" => text::seq(args),
        "yes" => text::yes(args),
        "basename" => text::basename(args),
        "dirname" => text::dirname(args),
        "ps" => system::ps(args),
        "kill" => system::kill(args),
        "uptime" => system::uptime(args),
        "free" => system::free(args),
        "uname" => system::uname(args),
        "clear" => system::clear(args),
        "whoami" => system::whoami(args),
        "id" => system::id(args),
        "hostname" => system::hostname(args),
        "sleep" => system::sleep(args),
        "true" => 0,
        "false" => 1,
        "env" => system::env(args),
        "help" => system::help(args),
        "htop" | "top" => htop::main(args),
        "nano" => nano::main(args),
        _ => 127,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let args = rt::Args::load();
    let status = run(&args);
    sdk::exit(status as u64)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    rt::write_all(b"\x1b[0m\nuserland: internal error\n");
    sdk::exit(101)
}

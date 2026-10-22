// SPDX-License-Identifier: Apache-2.0
//! File commands: `ls`, `cat`, `cp`, `mv`, `rm`, `mkdir`, `rmdir`, `touch`,
//! `stat`, `tree`, `find`, `du`, and `pwd`.

use crate::rt::{self, Args, Kind, Listing, Out, Path};
use aether_abi::user::errno;
use aether_sdk as sdk;

/// Splits arguments into option letters (from `-abc` words) and operands.
pub struct Options<'a> {
    flags: [bool; 128],
    operands: [&'a str; 32],
    count: usize,
}

impl<'a> Options<'a> {
    /// Parses `args`; `--` ends option parsing, and a lone `-` is an operand.
    pub fn parse(args: &'a Args) -> Self {
        let mut options = Self {
            flags: [false; 128],
            operands: [""; 32],
            count: 0,
        };
        let mut literal = false;
        for argument in args.iter() {
            if !literal && argument == "--" {
                literal = true;
            } else if !literal && argument.len() > 1 && argument.starts_with('-') {
                for byte in argument.bytes().skip(1) {
                    options.flags[usize::from(byte & 0x7f)] = true;
                }
            } else if options.count < options.operands.len() {
                options.operands[options.count] = argument;
                options.count += 1;
            }
        }
        options
    }

    /// Returns `true` when `-letter` was given.
    pub fn has(&self, letter: u8) -> bool {
        self.flags[usize::from(letter & 0x7f)]
    }

    /// The operands in order.
    pub fn operands(&self) -> &[&'a str] {
        &self.operands[..self.count]
    }
}

/// Resolves an operand against the working directory or reports it.
fn resolve(out: &mut Out, args: &Args, operand: &str) -> Option<Path> {
    let path = Path::resolve(args.cwd(), operand);
    if path.is_none() {
        outln!(out, "{}: {operand}: path too long", args.program());
    }
    path
}

pub fn pwd(args: &Args) -> i64 {
    let mut out = Out::new();
    outln!(out, "{}", args.cwd());
    0
}

const BLUE: &str = "\x1b[1;34m";
const RESET: &str = "\x1b[0m";

fn print_entry(out: &mut Out, entry: &rt::Entry<'_>, long: bool, color: bool, human: bool) {
    if long {
        let kind = if entry.directory { 'd' } else { '-' };
        let mode = if entry.directory {
            "rwxr-xr-x"
        } else {
            "rw-r--r--"
        };
        if human {
            out!(
                out,
                "{kind}{mode} user user {:>6} ",
                rt::Human(entry.size as u64)
            );
        } else {
            out!(out, "{kind}{mode} user user {:>6} ", entry.size);
        }
    }
    if entry.directory && color {
        out!(out, "{BLUE}{}{RESET}", entry.name);
    } else {
        out.str(entry.name);
    }
    if entry.directory && !color {
        out.str("/");
    }
}

pub fn ls(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let long = options.has(b'l');
    let human = options.has(b'h');
    let color = rt::stdio_terminal().1;
    let one_per_line = options.has(b'1') || !color;
    let mut operands = options.operands();
    if operands.is_empty() {
        operands = &["."];
    }
    let mut status = 0;
    for (index, operand) in operands.iter().enumerate() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 2;
            continue;
        };
        match rt::kind(&path) {
            Ok(Kind::File(size)) => {
                let entry = rt::Entry {
                    name: operand,
                    directory: false,
                    size,
                };
                print_entry(&mut out, &entry, long, color, human);
                outln!(out);
                continue;
            }
            Ok(Kind::Directory) => {}
            Err(error) => {
                out!(out, "ls: cannot access '{operand}': ");
                outln!(out, "{}", sdk::error_message(error));
                status = 2;
                continue;
            }
        }
        let mut buffer = [0u8; 2048];
        let listing = match Listing::read(&path, &mut buffer) {
            Ok(listing) => listing,
            Err(error) => {
                rt::report(&mut out, "ls", operand, error);
                status = 2;
                continue;
            }
        };
        if operands.len() > 1 {
            if index > 0 {
                outln!(out);
            }
            outln!(out, "{operand}:");
        }
        // Directory order is creation order; sort names for a stable view.
        let mut entries = [rt::Entry {
            name: "",
            directory: false,
            size: 0,
        }; 64];
        let mut count = 0;
        for entry in listing.entries() {
            if count < entries.len() {
                entries[count] = entry;
                count += 1;
            }
        }
        let entries = &mut entries[..count];
        entries.sort_unstable_by(|a, b| a.name.cmp(b.name));
        if long {
            let total: usize = entries.iter().map(|entry| entry.size.div_ceil(512)).sum();
            outln!(out, "total {total}");
        }
        if long || one_per_line {
            for entry in entries.iter() {
                print_entry(&mut out, entry, long, color, human);
                outln!(out);
            }
        } else {
            let width = entries
                .iter()
                .map(|entry| entry.name.len())
                .max()
                .unwrap_or(0)
                + 2;
            let columns = (80 / width.max(1)).max(1);
            for (position, entry) in entries.iter().enumerate() {
                print_entry(&mut out, entry, false, color, false);
                if (position + 1) % columns == 0 || position + 1 == entries.len() {
                    outln!(out);
                } else {
                    for _ in entry.name.len()..width {
                        out.str(" ");
                    }
                }
            }
        }
    }
    status
}

/// Copies standard input to standard output.
fn copy_stdin(out: &mut Out) {
    let mut stdin = rt::Stdin::new();
    let mut buffer = [0u8; 512];
    loop {
        let count = stdin.read(&mut buffer);
        if count == 0 {
            return;
        }
        out.bytes(&buffer[..count]);
        out.flush();
    }
}

pub fn cat(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let number = options.has(b'n');
    if options.operands().is_empty() {
        copy_stdin(&mut out);
        return 0;
    }
    let mut status = 0;
    let mut line = 1;
    for operand in options.operands() {
        if *operand == "-" {
            copy_stdin(&mut out);
            continue;
        }
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let mut data = [0u8; rt::MAX_FILE];
        match rt::read_file(&path, &mut data) {
            Ok(length) if number => {
                for text in data[..length].split_inclusive(|&byte| byte == b'\n') {
                    out!(out, "{line:>6}\t");
                    out.bytes(text);
                    line += 1;
                }
            }
            Ok(length) => out.bytes(&data[..length]),
            Err(error) => {
                rt::report(&mut out, "cat", operand, error);
                status = 1;
            }
        }
    }
    status
}

/// Resolves a copy or move destination: an existing directory receives the
/// source's name.
fn destination(source: &Path, target: &Path) -> Option<Path> {
    if rt::kind(target) == Ok(Kind::Directory) {
        let mut inside = *target;
        inside.push(source.name())?;
        Some(inside)
    } else {
        Some(*target)
    }
}

fn transfer(args: &Args, moving: bool) -> i64 {
    let mut out = Out::new();
    let program = args.program();
    let options = Options::parse(args);
    let operands = options.operands();
    if operands.len() < 2 {
        outln!(out, "usage: {program} SOURCE... DEST");
        return 1;
    }
    let Some(target) = resolve(&mut out, args, operands[operands.len() - 1]) else {
        return 1;
    };
    if operands.len() > 2 && rt::kind(&target) != Ok(Kind::Directory) {
        outln!(
            out,
            "{program}: target '{}' is not a directory",
            target.as_str()
        );
        return 1;
    }
    let mut status = 0;
    for operand in &operands[..operands.len() - 1] {
        let Some(source) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let Some(dest) = destination(&source, &target) else {
            outln!(out, "{program}: {operand}: path too long");
            status = 1;
            continue;
        };
        let result = if moving {
            if rt::kind(&dest).is_ok() && dest.as_str() != source.as_str() {
                // `mv` replaces an existing file, as on Linux.
                if rt::kind(&dest) == Ok(Kind::Directory) {
                    Err(errno::EISDIR)
                } else {
                    sdk::remove(dest.as_str())
                        .and_then(|()| sdk::rename(source.as_str(), dest.as_str()))
                }
            } else {
                sdk::rename(source.as_str(), dest.as_str())
            }
        } else {
            copy_file(&source, &dest, options.has(b'r') || options.has(b'R'))
        };
        if let Err(error) = result {
            rt::report(&mut out, program, operand, error);
            status = 1;
        }
    }
    status
}

fn copy_file(source: &Path, dest: &Path, recursive: bool) -> Result<(), i64> {
    match rt::kind(source)? {
        Kind::File(_) => {
            let mut data = [0u8; rt::MAX_FILE];
            let length = rt::read_file(source, &mut data)?;
            rt::write_file(dest, &data[..length])
        }
        Kind::Directory if !recursive => Err(errno::EISDIR),
        Kind::Directory => {
            if dest.as_str().starts_with(source.as_str())
                && dest.as_str().as_bytes().get(source.as_str().len()) == Some(&b'/')
            {
                return Err(errno::EINVAL);
            }
            match sdk::make_dir(dest.as_str()) {
                Ok(()) | Err(errno::EEXIST) => {}
                Err(error) => return Err(error),
            }
            let mut buffer = [0u8; 2048];
            let listing = Listing::read(source, &mut buffer)?;
            for entry in listing.entries() {
                let mut from = *source;
                let mut to = *dest;
                from.push(entry.name).ok_or(errno::EINVAL)?;
                to.push(entry.name).ok_or(errno::EINVAL)?;
                copy_file(&from, &to, true)?;
            }
            Ok(())
        }
    }
}

pub fn cp(args: &Args) -> i64 {
    transfer(args, false)
}

pub fn mv(args: &Args) -> i64 {
    transfer(args, true)
}

fn remove_tree(path: &Path) -> Result<(), i64> {
    if rt::kind(path)? == Kind::Directory {
        // Remove children first. A listing is re-read after each pass, since
        // removal changes the directory being listed.
        loop {
            let mut buffer = [0u8; 2048];
            let mut child = None;
            {
                let listing = Listing::read(path, &mut buffer)?;
                if let Some(entry) = listing.entries().next() {
                    let mut next = *path;
                    next.push(entry.name).ok_or(errno::EINVAL)?;
                    child = Some(next);
                }
            }
            match child {
                Some(child) => remove_tree(&child)?,
                None => break,
            }
        }
    }
    sdk::remove(path.as_str())
}

pub fn rm(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let recursive = options.has(b'r') || options.has(b'R');
    let force = options.has(b'f');
    if options.operands().is_empty() && !force {
        outln!(out, "usage: rm [-rf] FILE...");
        return 1;
    }
    let mut status = 0;
    for operand in options.operands() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        if path.is_root() {
            outln!(out, "rm: refusing to remove '/'");
            status = 1;
            continue;
        }
        let result = match rt::kind(&path) {
            Ok(Kind::Directory) if !recursive => Err(errno::EISDIR),
            Ok(_) if recursive => remove_tree(&path),
            Ok(_) => sdk::remove(path.as_str()),
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                if options.has(b'v') {
                    outln!(out, "removed '{operand}'");
                }
            }
            Err(errno::ENOENT) if force => {}
            Err(error) => {
                out!(out, "rm: cannot remove '{operand}': ");
                outln!(out, "{}", sdk::error_message(error));
                status = 1;
            }
        }
    }
    status
}

pub fn mkdir(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let parents = options.has(b'p');
    if options.operands().is_empty() {
        outln!(out, "usage: mkdir [-p] DIRECTORY...");
        return 1;
    }
    let mut status = 0;
    for operand in options.operands() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let result = if parents {
            let mut partial = Path::root();
            let mut result = Ok(());
            for component in path.as_str().split('/').filter(|part| !part.is_empty()) {
                if partial.push(component).is_none() {
                    result = Err(errno::EINVAL);
                    break;
                }
                match sdk::make_dir(partial.as_str()) {
                    Ok(()) => {}
                    Err(errno::EEXIST) if rt::kind(&partial) == Ok(Kind::Directory) => {}
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            }
            result
        } else {
            sdk::make_dir(path.as_str())
        };
        if let Err(error) = result {
            out!(out, "mkdir: cannot create directory '{operand}': ");
            outln!(out, "{}", sdk::error_message(error));
            status = 1;
        }
    }
    status
}

pub fn rmdir(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut status = 0;
    for operand in args.iter() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let result = match rt::kind(&path) {
            Ok(Kind::Directory) if path.is_root() => Err(errno::EACCES),
            Ok(Kind::Directory) => sdk::remove(path.as_str()),
            Ok(Kind::File(_)) => Err(errno::ENOTDIR),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            out!(out, "rmdir: failed to remove '{operand}': ");
            outln!(out, "{}", sdk::error_message(error));
            status = 1;
        }
    }
    status
}

pub fn touch(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut status = 0;
    if args.len() == 0 {
        outln!(out, "usage: touch FILE...");
        return 1;
    }
    for operand in args.iter() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        // The volume keeps no timestamps; touching creates missing files.
        match sdk::open(path.as_str(), sdk::WRITE | sdk::CREATE) {
            Ok(handle) => {
                let _ = sdk::close(handle);
            }
            Err(errno::EISDIR) => {}
            Err(error) => {
                rt::report(&mut out, "touch", operand, error);
                status = 1;
            }
        }
    }
    status
}

pub fn stat(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut status = 0;
    for operand in args.iter() {
        let Some(path) = resolve(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        match rt::kind(&path) {
            Ok(kind) => {
                let (size, description) = match kind {
                    Kind::File(size) => (size, "regular file"),
                    Kind::Directory => (0, "directory"),
                };
                outln!(out, "  File: {}", path.as_str());
                outln!(
                    out,
                    "  Size: {size:<10} Blocks: {:<6} {description}",
                    size.div_ceil(512)
                );
                outln!(
                    out,
                    "Access: {}  Uid: user  Scope: home folder",
                    if kind == Kind::Directory {
                        "drwxr-xr-x"
                    } else {
                        "-rw-r--r--"
                    }
                );
            }
            Err(error) => {
                rt::report(&mut out, "stat", operand, error);
                status = 1;
            }
        }
    }
    status
}

fn tree_walk(
    out: &mut Out,
    path: &Path,
    prefix: &mut [u8; 64],
    depth: usize,
    counts: &mut (usize, usize),
) {
    let mut buffer = [0u8; 2048];
    let Ok(listing) = Listing::read(path, &mut buffer) else {
        return;
    };
    let mut entries = [rt::Entry {
        name: "",
        directory: false,
        size: 0,
    }; 64];
    let mut count = 0;
    for entry in listing.entries() {
        if count < entries.len() {
            entries[count] = entry;
            count += 1;
        }
    }
    entries[..count].sort_unstable_by(|a, b| a.name.cmp(b.name));
    for (index, entry) in entries[..count].iter().enumerate() {
        let last = index + 1 == count;
        out.bytes(&prefix[..depth * 4]);
        out.str(if last { "└── " } else { "├── " });
        if entry.directory {
            outln!(out, "{BLUE}{}{RESET}", entry.name);
            counts.0 += 1;
            if depth * 4 + 4 <= prefix.len() {
                let marker: &[u8] = if last { b"    " } else { b"|   " };
                prefix[depth * 4..depth * 4 + 4].copy_from_slice(marker);
                let mut child = *path;
                if child.push(entry.name).is_some() {
                    tree_walk(out, &child, prefix, depth + 1, counts);
                }
            }
        } else {
            outln!(out, "{}", entry.name);
            counts.1 += 1;
        }
    }
}

pub fn tree(args: &Args) -> i64 {
    let mut out = Out::new();
    let operand = if args.len() > 0 { args.get(0) } else { "." };
    let Some(path) = resolve(&mut out, args, operand) else {
        return 1;
    };
    if rt::kind(&path) != Ok(Kind::Directory) {
        outln!(out, "{operand} [error opening dir]");
        return 1;
    }
    outln!(out, "{BLUE}{operand}{RESET}");
    let mut prefix = [b' '; 64];
    let mut counts = (0, 0);
    tree_walk(&mut out, &path, &mut prefix, 0, &mut counts);
    outln!(
        out,
        "\n{} director{}, {} file{}",
        counts.0,
        if counts.0 == 1 { "y" } else { "ies" },
        counts.1,
        if counts.1 == 1 { "" } else { "s" }
    );
    0
}

fn find_walk(out: &mut Out, path: &Path, shown: &str, pattern: Option<&str>, only: Option<u8>) {
    let mut buffer = [0u8; 2048];
    let Ok(listing) = Listing::read(path, &mut buffer) else {
        return;
    };
    for entry in listing.entries() {
        let mut child = *path;
        if child.push(entry.name).is_none() {
            continue;
        }
        let kind_ok = match only {
            Some(b'f') => !entry.directory,
            Some(b'd') => entry.directory,
            _ => true,
        };
        let name_ok =
            pattern.is_none_or(|pattern| rt::glob(pattern.as_bytes(), entry.name.as_bytes()));
        if kind_ok && name_ok {
            if shown == "/" {
                outln!(out, "/{}", entry.name);
            } else {
                outln!(out, "{shown}/{}", entry.name);
            }
        }
        if entry.directory {
            let mut label = [0u8; rt::MAX_PATH * 2];
            let text = if shown == "/" {
                concat(&mut label, "", entry.name)
            } else {
                concat(&mut label, shown, entry.name)
            };
            find_walk(out, &child, text, pattern, only);
        }
    }
}

fn concat<'a>(buffer: &'a mut [u8], first: &str, second: &str) -> &'a str {
    let mut length = 0;
    for part in [first.as_bytes(), b"/", second.as_bytes()] {
        let take = part.len().min(buffer.len() - length);
        buffer[length..length + take].copy_from_slice(&part[..take]);
        length += take;
    }
    core::str::from_utf8(&buffer[..length]).unwrap_or("?")
}

pub fn find(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut start = ".";
    let mut pattern = None;
    let mut only = None;
    let mut index = 0;
    while index < args.len() {
        match args.get(index) {
            "-name" | "-iname" => {
                pattern = Some(args.get(index + 1));
                index += 1;
            }
            "-type" => {
                only = args.get(index + 1).bytes().next();
                index += 1;
            }
            other if index == 0 && !other.starts_with('-') => start = other,
            other => {
                outln!(out, "find: unknown predicate '{other}'");
                return 1;
            }
        }
        index += 1;
    }
    let Some(path) = resolve(&mut out, args, start) else {
        return 1;
    };
    match rt::kind(&path) {
        Ok(Kind::Directory) => {
            if pattern.is_none() && only != Some(b'f') {
                outln!(out, "{start}");
            }
            find_walk(
                &mut out,
                &path,
                start.trim_end_matches('/').max("/"),
                pattern,
                only,
            );
            0
        }
        Ok(Kind::File(_)) => {
            outln!(out, "{start}");
            0
        }
        Err(error) => {
            rt::report(&mut out, "find", start, error);
            1
        }
    }
}

fn disk_usage(path: &Path) -> usize {
    match rt::kind(path) {
        Ok(Kind::File(size)) => size.div_ceil(512) * 512,
        Ok(Kind::Directory) => {
            let mut buffer = [0u8; 2048];
            let mut total = 512;
            if let Ok(listing) = Listing::read(path, &mut buffer) {
                for entry in listing.entries() {
                    let mut child = *path;
                    if child.push(entry.name).is_some() {
                        total += disk_usage(&child);
                    }
                }
            }
            total
        }
        Err(_) => 0,
    }
}

pub fn du(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let human = options.has(b'h');
    let mut operands = options.operands();
    if operands.is_empty() {
        operands = &["."];
    }
    for operand in operands {
        let Some(path) = resolve(&mut out, args, operand) else {
            continue;
        };
        let bytes = disk_usage(&path) as u64;
        if human {
            outln!(out, "{}\t{operand}", rt::Human(bytes));
        } else {
            outln!(out, "{}\t{operand}", bytes.div_ceil(1024));
        }
    }
    0
}

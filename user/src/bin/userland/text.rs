// SPDX-License-Identifier: Apache-2.0
//! Text commands: `echo`, `head`, `tail`, `wc`, `grep`, `sort`, `uniq`,
//! `tee`, `seq`, `yes`, `basename`, and `dirname`.

use crate::files::Options;
use crate::rt::{self, Args, Out, Path, Stdin};

/// Bytes of input the buffering commands (`sort`, `tail`) hold.
const TEXT_BYTES: usize = 32 * 1024;
/// Lines the buffering commands hold.
const MAX_LINES: usize = 2048;

/// A source of lines: a file (read whole) or standard input (streamed).
// There is no heap to box the file into; sources live briefly on the stack.
#[allow(clippy::large_enum_variant)]
enum Source {
    File {
        data: [u8; rt::MAX_FILE],
        len: usize,
        pos: usize,
    },
    Stdin(Stdin),
}

impl Source {
    fn open(out: &mut Out, args: &Args, operand: &str) -> Option<Self> {
        if operand == "-" {
            return Some(Self::Stdin(Stdin::new()));
        }
        let Some(path) = Path::resolve(args.cwd(), operand) else {
            outln!(out, "{}: {operand}: path too long", args.program());
            return None;
        };
        let mut data = [0u8; rt::MAX_FILE];
        match rt::read_file(&path, &mut data) {
            Ok(len) => Some(Self::File { data, len, pos: 0 }),
            Err(error) => {
                rt::report(out, args.program(), operand, error);
                None
            }
        }
    }

    /// Reads the next line (without newline) into `buffer`.
    fn line<'a>(&mut self, buffer: &'a mut [u8; 1024]) -> Option<&'a [u8]> {
        match self {
            Self::File { data, len, pos } => {
                if *pos >= *len {
                    return None;
                }
                let rest = &data[*pos..*len];
                let end = rest
                    .iter()
                    .position(|&byte| byte == b'\n')
                    .unwrap_or(rest.len());
                let take = end.min(buffer.len());
                buffer[..take].copy_from_slice(&rest[..take]);
                *pos += end + 1;
                Some(&buffer[..take])
            }
            Self::Stdin(stdin) => stdin.read_line(buffer),
        }
    }

    /// Reads the next raw bytes; 0 means the end.
    fn read(&mut self, buffer: &mut [u8]) -> usize {
        match self {
            Self::File { data, len, pos } => {
                let count = (*len - *pos).min(buffer.len());
                buffer[..count].copy_from_slice(&data[*pos..*pos + count]);
                *pos += count;
                count
            }
            Self::Stdin(stdin) => stdin.read(buffer),
        }
    }
}

/// Runs `each` over every line of every operand (standard input when none).
fn for_lines<'a>(
    out: &mut Out,
    args: &Args,
    operands: &[&'a str],
    mut each: impl FnMut(&mut Out, &'a str, &[u8]) -> bool,
) -> i64 {
    let stdin_only: &[&'a str] = &["-"];
    let operands = if operands.is_empty() {
        stdin_only
    } else {
        operands
    };
    let mut status = 0;
    for operand in operands {
        let Some(mut source) = Source::open(out, args, operand) else {
            status = 1;
            continue;
        };
        let mut buffer = [0u8; 1024];
        while let Some(line) = source.line(&mut buffer) {
            if !each(out, operand, line) {
                return status;
            }
        }
    }
    status
}

pub fn echo(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut newline = true;
    let mut escapes = false;
    let mut first = 0;
    while first < args.len() {
        match args.get(first) {
            "-n" => newline = false,
            "-e" => escapes = true,
            "-ne" | "-en" => {
                newline = false;
                escapes = true;
            }
            _ => break,
        }
        first += 1;
    }
    for index in first..args.len() {
        if index > first {
            out.str(" ");
        }
        let text = args.get(index);
        if escapes {
            let mut bytes = text.bytes();
            while let Some(byte) = bytes.next() {
                if byte != b'\\' {
                    out.bytes(&[byte]);
                    continue;
                }
                match bytes.next() {
                    Some(b'n') => out.str("\n"),
                    Some(b't') => out.str("\t"),
                    Some(b'e') => out.str("\x1b"),
                    Some(b'\\') => out.str("\\"),
                    Some(b'c') => return 0,
                    Some(other) => out.bytes(&[b'\\', other]),
                    None => out.str("\\"),
                }
            }
        } else {
            out.str(text);
        }
    }
    if newline {
        out.str("\n");
    }
    0
}

/// Parses `-n N` / `-N` for `head` and `tail`; returns the count and the
/// remaining operands.
fn line_count<'a>(args: &'a Args, operands: &mut [&'a str; 16]) -> (usize, usize) {
    let mut count = 10;
    let mut total = 0;
    let mut index = 0;
    while index < args.len() {
        let argument = args.get(index);
        if argument == "-n" {
            count = rt::parse_number(args.get(index + 1))
                .unwrap_or(10)
                .unsigned_abs() as usize;
            index += 1;
        } else if let Some(value) = argument.strip_prefix("-n") {
            count = rt::parse_number(value).unwrap_or(10).unsigned_abs() as usize;
        } else if argument.len() > 1
            && argument.starts_with('-')
            && let Some(value) = rt::parse_number(&argument[1..])
        {
            count = value.unsigned_abs() as usize;
        } else if total < operands.len() {
            operands[total] = argument;
            total += 1;
        }
        index += 1;
    }
    (count, total)
}

pub fn head(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut operands = [""; 16];
    let (count, total) = line_count(args, &mut operands);
    let mut printed = 0;
    let mut current = "";
    let many = total > 1;
    for_lines(&mut out, args, &operands[..total], |out, name, line| {
        if name != current {
            if many {
                outln!(
                    out,
                    "{}==> {name} <==",
                    if current.is_empty() { "" } else { "\n" }
                );
            }
            current = name;
            printed = 0;
        }
        if printed < count {
            out.bytes(line);
            out.str("\n");
            printed += 1;
        }
        // Stop reading standard input as soon as enough lines arrived.
        printed < count || many
    })
}

pub fn tail(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut operands = [""; 16];
    let (count, total) = line_count(args, &mut operands);
    let stdin_only = ["-"];
    let operands: &[&str] = if total == 0 {
        &stdin_only
    } else {
        &operands[..total]
    };
    let mut status = 0;
    for (index, operand) in operands.iter().enumerate() {
        // Keep a ring of the last `count` lines.
        let mut text = [0u8; TEXT_BYTES];
        let mut lines = [(0usize, 0usize); MAX_LINES];
        let keep = count.min(MAX_LINES);
        let mut stored = 0usize;
        let mut cursor = 0usize;
        let Some(mut source) = Source::open(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let mut buffer = [0u8; 1024];
        while let Some(line) = source.line(&mut buffer) {
            if keep == 0 {
                continue;
            }
            if cursor + line.len() > text.len() {
                // Compact: move the kept lines to the front.
                let mut packed = 0;
                let first = stored.saturating_sub(keep);
                for slot in first..stored {
                    let (start, length) = lines[slot % keep];
                    text.copy_within(start..start + length, packed);
                    lines[slot % keep] = (packed, length);
                    packed += length;
                }
                cursor = packed;
                if cursor + line.len() > text.len() {
                    continue;
                }
            }
            text[cursor..cursor + line.len()].copy_from_slice(line);
            lines[stored % keep] = (cursor, line.len());
            cursor += line.len();
            stored += 1;
        }
        if operands.len() > 1 {
            outln!(
                out,
                "{}==> {operand} <==",
                if index == 0 { "" } else { "\n" }
            );
        }
        for slot in stored.saturating_sub(keep)..stored {
            let (start, length) = lines[slot % keep];
            out.bytes(&text[start..start + length]);
            out.str("\n");
        }
    }
    status
}

pub fn wc(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let all = !options.has(b'l') && !options.has(b'w') && !options.has(b'c');
    let columns = [
        all || options.has(b'l'),
        all || options.has(b'w'),
        all || options.has(b'c'),
    ];
    let stdin_only: &[&str] = &["-"];
    let operands = options.operands();
    let inputs = if operands.is_empty() {
        stdin_only
    } else {
        operands
    };
    // Counts are printed after all inputs are read, so the columns can be
    // sized to the largest number as GNU wc does.
    let mut results = [(0usize, 0usize, 0usize, false); 32];
    let mut status = 0;
    let mut totals = (0, 0, 0);
    let mut reads_stdin = false;
    for (slot, operand) in results.iter_mut().zip(inputs) {
        reads_stdin |= *operand == "-";
        let Some(mut source) = Source::open(&mut out, args, operand) else {
            status = 1;
            continue;
        };
        let (mut lines, mut words, mut bytes, mut in_word) = (0, 0, 0, false);
        let mut buffer = [0u8; 512];
        loop {
            let count = source.read(&mut buffer);
            if count == 0 {
                break;
            }
            for &byte in &buffer[..count] {
                lines += usize::from(byte == b'\n');
                let space = byte.is_ascii_whitespace();
                words += usize::from(!space && !in_word);
                in_word = !space;
            }
            bytes += count;
        }
        *slot = (lines, words, bytes, true);
        totals = (totals.0 + lines, totals.1 + words, totals.2 + bytes);
    }
    let shown = columns.iter().filter(|&&shown| shown).count();
    let width = if shown == 1 && inputs.len() == 1 {
        1
    } else if reads_stdin {
        7
    } else {
        let largest = totals.0.max(totals.1).max(totals.2);
        let mut digits = 1;
        let mut value = largest;
        while value >= 10 {
            value /= 10;
            digits += 1;
        }
        digits
    };
    let print = |out: &mut Out, counts: [usize; 3], label: &str| {
        let mut first = true;
        for (count, _) in counts.iter().zip(columns).filter(|(_, shown)| *shown) {
            if !first {
                out.str(" ");
            }
            out!(out, "{count:>width$}");
            first = false;
        }
        if label == "-" {
            outln!(out);
        } else {
            outln!(out, " {label}");
        }
    };
    for (result, operand) in results.iter().zip(inputs) {
        if result.3 {
            print(&mut out, [result.0, result.1, result.2], operand);
        }
    }
    if inputs.len() > 1 {
        print(&mut out, [totals.0, totals.1, totals.2], "total");
    }
    status
}

fn contains(haystack: &[u8], needle: &[u8], ignore_case: bool) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|window| {
        if ignore_case {
            window.eq_ignore_ascii_case(needle)
        } else {
            window == needle
        }
    })
}

pub fn grep(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let operands = options.operands();
    let Some((&pattern, files)) = operands.split_first() else {
        outln!(out, "usage: grep [-invc] PATTERN [FILE...]");
        return 2;
    };
    let ignore_case = options.has(b'i');
    let invert = options.has(b'v');
    let count_only = options.has(b'c');
    let numbers = options.has(b'n');
    let color = rt::stdio_terminal().1 && !invert;
    let names = files.len() > 1;
    let mut matches = 0usize;
    let mut file_matches = 0usize;
    let mut number = 0usize;
    let mut current = "";
    let status = for_lines(&mut out, args, files, |out, name, line| {
        if name != current {
            if count_only && !current.is_empty() {
                if names {
                    out!(out, "{current}:");
                }
                outln!(out, "{file_matches}");
            }
            current = name;
            number = 0;
            file_matches = 0;
        }
        number += 1;
        let found = contains(line, pattern.as_bytes(), ignore_case);
        if found.is_some() == invert {
            return true;
        }
        matches += 1;
        file_matches += 1;
        if count_only {
            return true;
        }
        if names {
            out!(out, "\x1b[35m{name}\x1b[0m:");
        }
        if numbers {
            out!(out, "\x1b[32m{number}\x1b[0m:");
        }
        match found {
            Some(start) if color && !pattern.is_empty() => {
                let end = start + pattern.len();
                out.bytes(&line[..start]);
                out.str("\x1b[1;31m");
                out.bytes(&line[start..end]);
                out.str("\x1b[0m");
                out.bytes(&line[end..]);
            }
            _ => out.bytes(line),
        }
        out.str("\n");
        true
    });
    if count_only {
        if names {
            out!(out, "{current}:");
        }
        outln!(out, "{file_matches}");
    }
    if status != 0 {
        2
    } else if matches > 0 {
        0
    } else {
        1
    }
}

/// Collects every input line into `text` and `lines`; returns the count.
fn collect(
    out: &mut Out,
    args: &Args,
    operands: &[&str],
    text: &mut [u8],
    lines: &mut [(usize, usize)],
) -> (usize, i64) {
    let mut count = 0;
    let mut used = 0;
    let status = for_lines(out, args, operands, |_, _, line| {
        if count < lines.len() && used + line.len() <= text.len() {
            text[used..used + line.len()].copy_from_slice(line);
            lines[count] = (used, line.len());
            used += line.len();
            count += 1;
        }
        true
    });
    (count, status)
}

pub fn sort(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let mut text = [0u8; TEXT_BYTES];
    let mut lines = [(0usize, 0usize); MAX_LINES];
    let (count, status) = collect(&mut out, args, options.operands(), &mut text, &mut lines);
    let lines = &mut lines[..count];
    let numeric = options.has(b'n');
    let fold = options.has(b'f');
    let key = |&(start, length): &(usize, usize)| &text[start..start + length];
    // Ties keep input order, as GNU sort's stable mode does.
    lines.sort_unstable_by(|a, b| {
        let (left, right) = (key(a), key(b));
        let ordering = if numeric {
            let value = |bytes: &[u8]| {
                core::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.trim().parse::<i64>().ok())
                    .unwrap_or(0)
            };
            value(left).cmp(&value(right))
        } else if fold {
            left.iter()
                .map(u8::to_ascii_lowercase)
                .cmp(right.iter().map(u8::to_ascii_lowercase))
        } else {
            left.cmp(right)
        };
        let ordering = if options.has(b'r') {
            ordering.reverse()
        } else {
            ordering
        };
        ordering.then(a.0.cmp(&b.0))
    });
    let mut previous: Option<&[u8]> = None;
    for line in lines.iter() {
        let bytes = key(line);
        if options.has(b'u') && previous == Some(bytes) {
            continue;
        }
        out.bytes(bytes);
        out.str("\n");
        previous = Some(bytes);
    }
    status
}

pub fn uniq(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let counts = options.has(b'c');
    let mut previous = [0u8; 1024];
    let mut previous_len = usize::MAX;
    let mut repeats = 0usize;
    let flush = |out: &mut Out, line: &[u8], repeats: usize| {
        if counts {
            out!(out, "{repeats:>7} ");
        }
        out.bytes(line);
        out.str("\n");
    };
    let status = for_lines(
        &mut out,
        args,
        &options.operands()[..options.operands().len().min(1)],
        |out, _, line| {
            if previous_len != usize::MAX && &previous[..previous_len] == line {
                repeats += 1;
                return true;
            }
            if previous_len != usize::MAX {
                flush(out, &previous[..previous_len], repeats);
            }
            previous[..line.len()].copy_from_slice(line);
            previous_len = line.len();
            repeats = 1;
            true
        },
    );
    if previous_len != usize::MAX {
        flush(&mut out, &previous[..previous_len], repeats);
    }
    status
}

pub fn tee(args: &Args) -> i64 {
    let mut out = Out::new();
    let options = Options::parse(args);
    let mut saved = [0u8; rt::MAX_FILE];
    let mut length = 0;
    let mut stdin = Stdin::new();
    let mut buffer = [0u8; 512];
    loop {
        let count = stdin.read(&mut buffer);
        if count == 0 {
            break;
        }
        out.bytes(&buffer[..count]);
        out.flush();
        let take = count.min(saved.len() - length);
        saved[length..length + take].copy_from_slice(&buffer[..take]);
        length += take;
    }
    let mut status = 0;
    for operand in options.operands() {
        let Some(path) = Path::resolve(args.cwd(), operand) else {
            status = 1;
            continue;
        };
        let mut data = [0u8; rt::MAX_FILE];
        let mut total = 0;
        if options.has(b'a') {
            total = rt::read_file(&path, &mut data).unwrap_or(0);
        }
        let take = length.min(data.len() - total);
        data[total..total + take].copy_from_slice(&saved[..take]);
        total += take;
        if let Err(error) = rt::write_file(&path, &data[..total]) {
            rt::report(&mut out, "tee", operand, error);
            status = 1;
        }
    }
    status
}

pub fn seq(args: &Args) -> i64 {
    let mut out = Out::new();
    let numbers: [Option<i64>; 3] = [
        rt::parse_number(args.get(0)),
        rt::parse_number(args.get(1)),
        rt::parse_number(args.get(2)),
    ];
    let (first, step, last) = match (args.len(), numbers) {
        (1, [Some(last), ..]) => (1, 1, last),
        (2, [Some(first), Some(last), _]) => (first, 1, last),
        (3, [Some(first), Some(step), Some(last)]) if step != 0 => (first, step, last),
        _ => {
            outln!(out, "usage: seq [FIRST [STEP]] LAST");
            return 1;
        }
    };
    let mut value = first;
    while (step > 0 && value <= last) || (step < 0 && value >= last) {
        outln!(out, "{value}");
        value += step;
    }
    0
}

pub fn yes(args: &Args) -> i64 {
    let mut out = Out::new();
    let mut line = [0u8; 256];
    let mut length = 0;
    if args.len() == 0 {
        line[0] = b'y';
        length = 1;
    }
    for (index, argument) in args.iter().enumerate() {
        for &byte in argument.as_bytes().iter().chain(if index + 1 < args.len() {
            b" ".iter()
        } else {
            b"".iter()
        }) {
            if length < line.len() - 1 {
                line[length] = byte;
                length += 1;
            }
        }
    }
    line[length] = b'\n';
    // Runs until the reader goes away (EPIPE ends the process) or Ctrl+C.
    loop {
        out.bytes(&line[..=length]);
    }
}

pub fn basename(args: &Args) -> i64 {
    let mut out = Out::new();
    let path = args.get(0).trim_end_matches('/');
    let name = path.rsplit('/').next().unwrap_or("");
    let name = if name.is_empty() { "/" } else { name };
    let name = match args.get(1) {
        "" => name,
        suffix => name
            .strip_suffix(suffix)
            .filter(|stem| !stem.is_empty())
            .unwrap_or(name),
    };
    outln!(out, "{name}");
    0
}

pub fn dirname(args: &Args) -> i64 {
    let mut out = Out::new();
    let path = args.get(0).trim_end_matches('/');
    let parent = match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((parent, _)) => parent,
        None if args.get(0).starts_with('/') => "/",
        None => ".",
    };
    outln!(out, "{parent}");
    0
}

// SPDX-License-Identifier: Apache-2.0
//! `sh`: the Aether Terminal shell.
//!
//! A small POSIX-flavoured shell: an interactive line editor with history
//! and Tab completion, quoting, `$?`, pipelines (`|`), redirection (`>`,
//! `>>`, `<`), and command lists (`;`, `&&`, `||`). Every command runs as its
//! own ring-3 process started through the kernel's `spawn` call. Pipeline
//! stages are joined by kernel pipes; redirection is done by the shell, which
//! pumps a file into or out of a pipe while the job runs.

use crate::rt::{self, Args, Kind, Out, Path};
use aether_abi::user::{PIPE_READ, PIPE_WRITE, TERMINAL_PROGRAMS, errno};
use aether_sdk as sdk;

const LINE_BYTES: usize = 256;
const HISTORY: usize = 32;
const MAX_WORDS: usize = 32;
const MAX_STAGES: usize = 4;
const BUILTINS: [&str; 4] = ["cd", "exit", "history", "pwd"];

struct Shell {
    cwd: Path,
    status: i64,
    history: [[u8; LINE_BYTES]; HISTORY],
    history_len: [usize; HISTORY],
    history_count: usize,
    exit: Option<i64>,
}

pub fn main(args: &Args) -> i64 {
    let mut shell = Shell {
        cwd: Path::resolve("/", args.cwd()).unwrap_or(Path::root()),
        status: 0,
        history: [[0; LINE_BYTES]; HISTORY],
        history_len: [0; HISTORY],
        history_count: 0,
        exit: None,
    };
    // `sh -c COMMAND` and `sh SCRIPT` run non-interactively.
    if args.get(0) == "-c" {
        shell.execute(args.get(1).as_bytes());
        return shell.exit.unwrap_or(shell.status);
    }
    if args.len() > 0 {
        return shell.script(args.get(0));
    }
    let mut out = Out::new();
    loop {
        let mut line = [0u8; LINE_BYTES];
        let Some(length) = shell.read_line(&mut line) else {
            outln!(out, "exit");
            return shell.status;
        };
        out.flush();
        let text = &line[..length];
        if !text.iter().all(u8::is_ascii_whitespace) {
            shell.remember(text);
            shell.execute(text);
        }
        if let Some(status) = shell.exit {
            return status;
        }
    }
}

impl Shell {
    fn script(&mut self, operand: &str) -> i64 {
        let mut out = Out::new();
        let Some(path) = Path::resolve(self.cwd.as_str(), operand) else {
            return 2;
        };
        let mut data = [0u8; rt::MAX_FILE];
        let length = match rt::read_file(&path, &mut data) {
            Ok(length) => length,
            Err(error) => {
                rt::report(&mut out, "sh", operand, error);
                return 127;
            }
        };
        out.flush();
        for line in data[..length].split(|&byte| byte == b'\n') {
            let line = match line.iter().position(|&byte| byte == b'#') {
                Some(comment) => &line[..comment],
                None => line,
            };
            self.execute(line);
            if let Some(status) = self.exit {
                return status;
            }
        }
        self.status
    }

    fn remember(&mut self, line: &[u8]) {
        let length = line.len().min(LINE_BYTES);
        if self.history_count > 0 {
            let last = (self.history_count - 1) % HISTORY;
            if self.history[last][..self.history_len[last]] == line[..length] {
                return;
            }
        }
        let slot = self.history_count % HISTORY;
        self.history[slot][..length].copy_from_slice(&line[..length]);
        self.history_len[slot] = length;
        self.history_count += 1;
    }

    fn prompt(&self, out: &mut Out) -> usize {
        out!(
            out,
            "\x1b[1;32muser@aether\x1b[0m:\x1b[1;34m{}\x1b[0m$ ",
            self.cwd.as_str()
        );
        "user@aether:".len() + self.cwd.as_str().len() + 2
    }

    /// Reads one edited line. `None` means Ctrl+D on an empty line.
    fn read_line(&mut self, line: &mut [u8; LINE_BYTES]) -> Option<usize> {
        let mut editor = Editor {
            line,
            len: 0,
            cursor: 0,
            prompt_width: 0,
        };
        let mut out = Out::new();
        editor.prompt_width = self.prompt(&mut out);
        out.flush();
        let mut browse = self.history_count;
        let mut pending = [0u8; 32];
        let mut escape: Option<([u8; 4], usize)> = None;
        loop {
            let count = rt::read_raw(&mut pending);
            if count == 0 {
                return None;
            }
            for &byte in &pending[..count] {
                if let Some((mut sequence, mut length)) = escape.take() {
                    if length < sequence.len() {
                        sequence[length] = byte;
                        length += 1;
                    }
                    let finished = length > 1 && (byte.is_ascii_alphabetic() || byte == b'~');
                    if !finished && length < sequence.len() {
                        escape = Some((sequence, length));
                        continue;
                    }
                    match &sequence[..length] {
                        b"[A" => self.recall(&mut editor, &mut browse, -1, &mut out),
                        b"[B" => self.recall(&mut editor, &mut browse, 1, &mut out),
                        b"[C" => editor.right(&mut out),
                        b"[D" => editor.left(&mut out),
                        b"[H" | b"OH" | b"[1~" => editor.home(&mut out),
                        b"[F" | b"OF" | b"[4~" => editor.end(&mut out),
                        b"[3~" => editor.delete(&mut out),
                        _ => {}
                    }
                    continue;
                }
                match byte {
                    0x1b => escape = Some(([0; 4], 0)),
                    b'\r' | b'\n' => {
                        editor.end(&mut out);
                        outln!(out);
                        out.flush();
                        return Some(editor.len);
                    }
                    0x03 => {
                        outln!(out, "^C");
                        editor.len = 0;
                        editor.cursor = 0;
                        self.status = 130;
                        editor.prompt_width = self.prompt(&mut out);
                        browse = self.history_count;
                    }
                    0x04 => {
                        if editor.len == 0 {
                            out.flush();
                            return None;
                        }
                        editor.delete(&mut out);
                    }
                    0x7f | 0x08 => editor.backspace(&mut out),
                    0x01 => editor.home(&mut out),
                    0x05 => editor.end(&mut out),
                    0x02 => editor.left(&mut out),
                    0x06 => editor.right(&mut out),
                    0x0b => {
                        editor.len = editor.cursor;
                        out.str("\x1b[K");
                    }
                    0x15 => {
                        let removed = editor.cursor;
                        editor.line.copy_within(editor.cursor..editor.len, 0);
                        editor.len -= removed;
                        editor.cursor = 0;
                        editor.redraw(&mut out, removed);
                    }
                    0x17 => editor.delete_word(&mut out),
                    0x0c => {
                        out.str("\x1b[H\x1b[2J");
                        editor.prompt_width = self.prompt(&mut out);
                        editor.redraw(&mut out, 0);
                    }
                    0x10 => self.recall(&mut editor, &mut browse, -1, &mut out),
                    0x0e => self.recall(&mut editor, &mut browse, 1, &mut out),
                    b'\t' => self.complete(&mut editor, &mut out),
                    0x20..=0x7e => editor.insert(byte, &mut out),
                    _ => {}
                }
            }
            out.flush();
        }
    }

    fn recall(&self, editor: &mut Editor<'_>, browse: &mut usize, step: isize, out: &mut Out) {
        let oldest = self.history_count.saturating_sub(HISTORY);
        let next = browse.checked_add_signed(step).unwrap_or(0);
        if next < oldest || next > self.history_count {
            return;
        }
        *browse = next;
        let previous = editor.len;
        if next == self.history_count {
            editor.len = 0;
        } else {
            let slot = next % HISTORY;
            let length = self.history_len[slot];
            editor.line[..length].copy_from_slice(&self.history[slot][..length]);
            editor.len = length;
        }
        editor.cursor = editor.len;
        editor.redraw(out, previous.saturating_sub(editor.len));
    }

    /// Tab completion: program names for the first word, paths otherwise.
    fn complete(&self, editor: &mut Editor<'_>, out: &mut Out) {
        let before = &editor.line[..editor.cursor];
        let start = before
            .iter()
            .rposition(|&byte| {
                byte == b' ' || byte == b'|' || byte == b';' || byte == b'<' || byte == b'>'
            })
            .map_or(0, |index| index + 1);
        let word = core::str::from_utf8(&before[start..]).unwrap_or("");
        let first_word = before[..start].iter().all(|&byte| byte == b' ');
        let mut candidates: [(&str, bool); 64] = [("", false); 64];
        let mut count = 0;
        let mut buffer = [0u8; 2048];
        let (directory_part, name_part) = match word.rfind('/') {
            Some(index) => (&word[..=index], &word[index + 1..]),
            None => ("", word),
        };
        if first_word && directory_part.is_empty() {
            for name in TERMINAL_PROGRAMS.iter().chain(BUILTINS.iter()) {
                if name.starts_with(word)
                    && count < candidates.len()
                    && !candidates[..count].iter().any(|(seen, _)| seen == name)
                {
                    candidates[count] = (name, false);
                    count += 1;
                }
            }
        } else if let Some(directory) = Path::resolve(
            self.cwd.as_str(),
            if directory_part.is_empty() {
                "."
            } else {
                directory_part
            },
        ) && let Ok(listing) = rt::Listing::read(&directory, &mut buffer)
        {
            for entry in listing.entries() {
                if entry.name.starts_with(name_part) && count < candidates.len() {
                    candidates[count] = (entry.name, entry.directory);
                    count += 1;
                }
            }
        }
        let candidates = &mut candidates[..count];
        candidates.sort_unstable_by(|a, b| a.0.cmp(b.0));
        match candidates {
            [] => out.str("\x07"),
            [(name, directory)] => {
                for &byte in &name.as_bytes()[name_part.len()..] {
                    editor.insert(byte, out);
                }
                editor.insert(if *directory { b'/' } else { b' ' }, out);
            }
            [first, rest @ ..] => {
                let mut common = first.0.len();
                for (name, _) in rest.iter() {
                    common = common.min(
                        first
                            .0
                            .bytes()
                            .zip(name.bytes())
                            .take_while(|(a, b)| a == b)
                            .count(),
                    );
                }
                if common > name_part.len() {
                    for &byte in &first.0.as_bytes()[name_part.len()..common] {
                        editor.insert(byte, out);
                    }
                    return;
                }
                outln!(out);
                for (index, (name, directory)) in candidates.iter().enumerate() {
                    out!(out, "{name}{}", if *directory { "/" } else { "" });
                    let width = name.len() + usize::from(*directory);
                    if (index + 1) % 6 == 0 || index + 1 == candidates.len() {
                        outln!(out);
                    } else {
                        for _ in width..13 {
                            out.str(" ");
                        }
                    }
                }
                editor.prompt_width = self.prompt(out);
                editor.redraw(out, 0);
            }
        }
    }

    /// Parses and runs one command line.
    fn execute(&mut self, line: &[u8]) {
        let mut words = [0u8; 1024];
        let mut tokens = [Token::Word(0, 0); 64];
        let count = match self.tokenize(line, &mut words, &mut tokens) {
            Ok(count) => count,
            Err(message) => {
                let mut out = Out::new();
                outln!(out, "sh: {message}");
                self.status = 2;
                return;
            }
        };
        let tokens = &tokens[..count];
        let mut start = 0;
        let mut skip = false;
        while start < tokens.len() {
            let end = tokens[start..]
                .iter()
                .position(|token| matches!(token, Token::Semi | Token::And | Token::Or))
                .map_or(tokens.len(), |offset| start + offset);
            if !skip && end > start {
                self.pipeline(&words, &tokens[start..end]);
                if self.exit.is_some() {
                    return;
                }
            }
            skip = match tokens.get(end) {
                Some(Token::And) => self.status != 0,
                Some(Token::Or) => self.status == 0,
                _ => false,
            };
            start = end + 1;
        }
    }

    fn tokenize(
        &self,
        line: &[u8],
        words: &mut [u8; 1024],
        tokens: &mut [Token; 64],
    ) -> Result<usize, &'static str> {
        let mut used = 0usize;
        let mut count = 0usize;
        let mut index = 0usize;
        let mut push = |token: Token, count: &mut usize| -> Result<(), &'static str> {
            if *count == tokens.len() {
                return Err("too many words");
            }
            tokens[*count] = token;
            *count += 1;
            Ok(())
        };
        while index < line.len() {
            let byte = line[index];
            match byte {
                b' ' | b'\t' => index += 1,
                b'|' if line.get(index + 1) == Some(&b'|') => {
                    push(Token::Or, &mut count)?;
                    index += 2;
                }
                b'&' if line.get(index + 1) == Some(&b'&') => {
                    push(Token::And, &mut count)?;
                    index += 2;
                }
                b'&' => return Err("background jobs are not supported"),
                b'|' => {
                    push(Token::Pipe, &mut count)?;
                    index += 1;
                }
                b';' => {
                    push(Token::Semi, &mut count)?;
                    index += 1;
                }
                b'>' if line.get(index + 1) == Some(&b'>') => {
                    push(Token::Append, &mut count)?;
                    index += 2;
                }
                b'>' => {
                    push(Token::Output, &mut count)?;
                    index += 1;
                }
                b'<' => {
                    push(Token::Input, &mut count)?;
                    index += 1;
                }
                _ => {
                    let start = used;
                    let mut quote = 0u8;
                    let mut first = true;
                    while index < line.len() {
                        let byte = line[index];
                        if quote == 0 && b" \t|;&<>".contains(&byte) {
                            break;
                        }
                        index += 1;
                        let mut emit: &[u8] = core::slice::from_ref(&line[index - 1]);
                        let mut status_text = [0u8; 20];
                        match byte {
                            b'\'' | b'"' if quote == 0 => {
                                quote = byte;
                                first = false;
                                continue;
                            }
                            _ if byte == quote => {
                                quote = 0;
                                continue;
                            }
                            b'\\' if quote != b'\'' && index < line.len() => {
                                emit = core::slice::from_ref(&line[index]);
                                index += 1;
                            }
                            b'$' if quote != b'\'' && line.get(index) == Some(&b'?') => {
                                index += 1;
                                let length = format_number(self.status, &mut status_text);
                                emit = &status_text[..length];
                            }
                            b'~' if quote == 0
                                && first
                                && matches!(line.get(index), None | Some(b'/' | b' ')) =>
                            {
                                emit = b"/";
                                if line.get(index) == Some(&b'/') {
                                    index += 1;
                                }
                            }
                            _ => {}
                        }
                        first = false;
                        if used + emit.len() > words.len() {
                            return Err("line too long");
                        }
                        words[used..used + emit.len()].copy_from_slice(emit);
                        used += emit.len();
                    }
                    if quote != 0 {
                        return Err("unterminated quote");
                    }
                    push(Token::Word(start as u16, (used - start) as u16), &mut count)?;
                }
            }
        }
        Ok(count)
    }

    /// Runs one pipeline: `a | b | c` with optional `<` and `>`/`>>`.
    fn pipeline(&mut self, words: &[u8], tokens: &[Token]) {
        let mut out = Out::new();
        let mut stages: [Stage; MAX_STAGES] = [Stage::EMPTY; MAX_STAGES];
        let mut stage_count = 1;
        let mut input: Option<&str> = None;
        let mut output: Option<(&str, bool)> = None;
        let text = |start: u16, length: u16| {
            core::str::from_utf8(&words[usize::from(start)..usize::from(start + length)])
                .unwrap_or("")
        };
        let mut index = 0;
        while index < tokens.len() {
            match tokens[index] {
                Token::Word(start, length) => {
                    let stage = &mut stages[stage_count - 1];
                    if stage.count == MAX_WORDS {
                        outln!(out, "sh: too many arguments");
                        self.status = 2;
                        return;
                    }
                    stage.words[stage.count] = (start, length);
                    stage.count += 1;
                }
                Token::Pipe => {
                    if stages[stage_count - 1].count == 0 || stage_count == MAX_STAGES {
                        outln!(
                            out,
                            "sh: syntax error near '|' (at most {MAX_STAGES} stages)"
                        );
                        self.status = 2;
                        return;
                    }
                    stage_count += 1;
                }
                Token::Input | Token::Output | Token::Append => {
                    let Some(Token::Word(start, length)) = tokens.get(index + 1) else {
                        outln!(out, "sh: syntax error: redirection needs a file name");
                        self.status = 2;
                        return;
                    };
                    let name = text(*start, *length);
                    match tokens[index] {
                        Token::Input => input = Some(name),
                        Token::Append => output = Some((name, true)),
                        _ => output = Some((name, false)),
                    }
                    index += 1;
                }
                _ => {}
            }
            index += 1;
        }
        let stages = &stages[..stage_count];
        if stages.iter().any(|stage| stage.count == 0) {
            if stage_count > 1 || input.is_some() || output.is_some() {
                outln!(out, "sh: syntax error: empty command");
                self.status = 2;
            }
            return;
        }
        let name = |stage: &Stage, word: usize| {
            let (start, length) = stage.words[word];
            text(start, length)
        };

        // Builtins change the shell itself, so they run in-process.
        if stage_count == 1 && input.is_none() && output.is_none() {
            let stage = &stages[0];
            match name(stage, 0) {
                "cd" => {
                    self.status =
                        self.cd(&mut out, if stage.count > 1 { name(stage, 1) } else { "/" });
                    return;
                }
                "exit" => {
                    self.exit = Some(if stage.count > 1 {
                        name(stage, 1).parse().unwrap_or(2)
                    } else {
                        self.status
                    });
                    return;
                }
                "history" => {
                    let oldest = self.history_count.saturating_sub(HISTORY);
                    for number in oldest..self.history_count {
                        let slot = number % HISTORY;
                        out!(out, "{:>5}  ", number + 1);
                        out.bytes(&self.history[slot][..self.history_len[slot]]);
                        outln!(out);
                    }
                    self.status = 0;
                    return;
                }
                _ => {}
            }
        }

        // Read the input file before starting anything.
        let mut input_data = [0u8; rt::MAX_FILE];
        let mut input_len = 0;
        if let Some(file) = input {
            let result = Path::resolve(self.cwd.as_str(), file)
                .ok_or(errno::EINVAL)
                .and_then(|path| rt::read_file(&path, &mut input_data));
            match result {
                Ok(length) => input_len = length,
                Err(error) => {
                    rt::report(&mut out, "sh", file, error);
                    self.status = 1;
                    return;
                }
            }
        }
        let output_path = match output {
            Some((file, _)) => match Path::resolve(self.cwd.as_str(), file) {
                Some(path) if rt::kind(&path) != Ok(Kind::Directory) => Some(path),
                _ => {
                    rt::report(&mut out, "sh", file, errno::EISDIR);
                    self.status = 1;
                    return;
                }
            },
            None => None,
        };
        out.flush();

        // Every pipe the command line needs is created up front. Running out
        // fails the whole command: a redirection must never fall back to the
        // terminal, or `cmd > file` would print instead of writing the file.
        let mut pipes = [0u64; MAX_STAGES];
        let mut input_pipe = None;
        let mut output_pipe = None;
        let wanted =
            (stage_count - 1) + usize::from(input.is_some()) + usize::from(output.is_some());
        let mut created = [0u64; MAX_STAGES + 2];
        for index in 0..wanted {
            match sdk::pipe_create() {
                Ok(pipe) => created[index] = pipe,
                Err(error) => {
                    for &pipe in &created[..index] {
                        let _ = sdk::pipe_close(pipe, PIPE_READ);
                        let _ = sdk::pipe_close(pipe, PIPE_WRITE);
                    }
                    rt::report(&mut out, "sh", "pipe", error);
                    self.status = 1;
                    return;
                }
            }
        }
        let mut next = created[..wanted].iter().copied();
        for slot in pipes.iter_mut().take(stage_count - 1) {
            *slot = next.next().unwrap_or(0);
        }
        if input.is_some() {
            input_pipe = next.next();
        }
        if output.is_some() {
            output_pipe = next.next();
        }

        let mut pids = [0u64; MAX_STAGES];
        let mut statuses = [None::<i64>; MAX_STAGES];
        for (position, stage) in stages.iter().enumerate() {
            let stdin = if position == 0 {
                input_pipe.unwrap_or(0)
            } else {
                pipes[position - 1]
            };
            let stdout = if position + 1 == stage_count {
                output_pipe.unwrap_or(0)
            } else {
                pipes[position]
            };
            let mut block = [0u8; aether_abi::user::ARGUMENT_BLOCK_BYTES];
            let mut length = 0;
            let mut fits = true;
            for part in core::iter::once(self.cwd.as_str())
                .chain((0..stage.count).map(|word| name(stage, word)))
            {
                if length + part.len() + 1 > block.len() {
                    fits = false;
                    break;
                }
                block[length..length + part.len()].copy_from_slice(part.as_bytes());
                length += part.len();
                block[length] = 0;
                length += 1;
            }
            let program = name(stage, 0);
            let result = if !fits {
                Err(errno::EINVAL)
            } else if BUILTINS.contains(&program) && program != "pwd" {
                Err(errno::ENOSYS)
            } else {
                sdk::spawn(&block[..length], stdin, stdout)
            };
            match result {
                Ok(pid) => pids[position] = pid,
                Err(error) => {
                    statuses[position] = Some(127);
                    match error {
                        errno::ENOENT => outln!(out, "sh: {program}: command not found"),
                        errno::EAGAIN => outln!(out, "sh: {program}: no free process slot"),
                        errno::ENOSYS => outln!(out, "sh: {program}: builtin cannot be piped"),
                        errno::EINVAL => outln!(out, "sh: {program}: argument list too long"),
                        _ => rt::report(&mut out, "sh", program, error),
                    }
                    out.flush();
                }
            }
        }
        // Drop the shell's own ends so readers see end of input once their
        // writers exit.
        for &pipe in pipes.iter().take(stage_count - 1) {
            let _ = sdk::pipe_close(pipe, PIPE_READ);
            let _ = sdk::pipe_close(pipe, PIPE_WRITE);
        }
        if let Some(pipe) = input_pipe {
            let _ = sdk::pipe_close(pipe, PIPE_READ);
        }
        if let Some(pipe) = output_pipe {
            let _ = sdk::pipe_close(pipe, PIPE_WRITE);
        }

        // Pump redirections and collect exit statuses.
        let mut fed = 0;
        let mut input_open = input_pipe;
        let mut output_open = output_pipe;
        let mut captured = [0u8; rt::MAX_FILE];
        let mut captured_len = 0;
        let mut overflow = false;
        loop {
            let mut progress = false;
            if let Some(pipe) = input_open {
                if fed < input_len {
                    match sdk::pipe_write(pipe, &input_data[fed..input_len]) {
                        Ok(count) => {
                            fed += count;
                            progress |= count > 0;
                        }
                        Err(errno::EAGAIN) => {}
                        Err(_) => fed = input_len,
                    }
                }
                if fed >= input_len {
                    let _ = sdk::pipe_close(pipe, PIPE_WRITE);
                    input_open = None;
                }
            }
            if let Some(pipe) = output_open {
                let mut chunk = [0u8; 512];
                match sdk::pipe_read(pipe, &mut chunk) {
                    Ok(0) => {
                        let _ = sdk::pipe_close(pipe, PIPE_READ);
                        output_open = None;
                    }
                    Ok(count) => {
                        let room = captured.len() - captured_len;
                        if count > room {
                            overflow = true;
                        }
                        let take = count.min(room);
                        captured[captured_len..captured_len + take].copy_from_slice(&chunk[..take]);
                        captured_len += take;
                        progress = true;
                    }
                    Err(errno::EAGAIN) => {}
                    Err(_) => output_open = None,
                }
            }
            for position in 0..stage_count {
                if statuses[position].is_none() {
                    match sdk::wait(pids[position]) {
                        Ok(status) => {
                            statuses[position] = Some(status);
                            progress = true;
                        }
                        Err(errno::EAGAIN) => {}
                        Err(_) => statuses[position] = Some(127),
                    }
                }
            }
            let done = statuses[..stage_count].iter().all(Option::is_some);
            if done && output_open.is_none() {
                break;
            }
            if done && let Some(pipe) = output_open {
                // Every writer exited; drain what is left, then stop.
                let mut chunk = [0u8; 512];
                if let Ok(count @ 1..) = sdk::pipe_read(pipe, &mut chunk) {
                    let take = count.min(captured.len() - captured_len);
                    overflow |= take < count;
                    captured[captured_len..captured_len + take].copy_from_slice(&chunk[..take]);
                    captured_len += take;
                    continue;
                }
                let _ = sdk::pipe_close(pipe, PIPE_READ);
                break;
            }
            if !progress {
                let _ = sdk::sleep();
            }
        }
        if let Some(pipe) = input_open {
            let _ = sdk::pipe_close(pipe, PIPE_WRITE);
        }
        self.status = statuses[stage_count - 1].unwrap_or(0);

        if let (Some(path), Some((file, append))) = (output_path, output) {
            let mut data = [0u8; rt::MAX_FILE];
            let mut length = 0;
            if append {
                length = rt::read_file(&path, &mut data).unwrap_or(0);
            }
            let take = captured_len.min(data.len() - length);
            overflow |= take < captured_len;
            data[length..length + take].copy_from_slice(&captured[..take]);
            length += take;
            if let Err(error) = rt::write_file(&path, &data[..length]) {
                rt::report(&mut out, "sh", file, error);
                self.status = 1;
            } else if overflow {
                outln!(
                    out,
                    "sh: {file}: output truncated to {} bytes (volume file limit)",
                    rt::MAX_FILE
                );
            }
        }
    }

    fn cd(&mut self, out: &mut Out, target: &str) -> i64 {
        let Some(path) = Path::resolve(self.cwd.as_str(), target) else {
            outln!(out, "cd: {target}: path too long");
            return 1;
        };
        match rt::kind(&path) {
            Ok(Kind::Directory) => {
                self.cwd = path;
                0
            }
            Ok(Kind::File(_)) => {
                outln!(out, "cd: {target}: Not a directory");
                1
            }
            Err(error) => {
                rt::report(out, "cd", target, error);
                1
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Token {
    Word(u16, u16),
    Pipe,
    Semi,
    And,
    Or,
    Input,
    Output,
    Append,
}

#[derive(Clone, Copy)]
struct Stage {
    words: [(u16, u16); MAX_WORDS],
    count: usize,
}

impl Stage {
    const EMPTY: Self = Self {
        words: [(0, 0); MAX_WORDS],
        count: 0,
    };
}

fn format_number(value: i64, out: &mut [u8; 20]) -> usize {
    let mut digits = [0u8; 20];
    let mut count = 0;
    let mut magnitude = value.unsigned_abs();
    loop {
        digits[count] = b'0' + (magnitude % 10) as u8;
        count += 1;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    let mut length = 0;
    if value < 0 {
        out[0] = b'-';
        length = 1;
    }
    for index in (0..count).rev() {
        out[length] = digits[index];
        length += 1;
    }
    length
}

/// The line being edited and its on-screen cursor.
struct Editor<'a> {
    line: &'a mut [u8; LINE_BYTES],
    len: usize,
    cursor: usize,
    prompt_width: usize,
}

impl Editor<'_> {
    /// Rewrites the text from the cursor's row start. `cleared` extra cells
    /// are blanked when the line got shorter.
    fn redraw(&mut self, out: &mut Out, cleared: usize) {
        let _ = cleared;
        out.str("\r");
        out!(out, "\x1b[{}C", self.prompt_width);
        out.bytes(&self.line[..self.len]);
        out.str("\x1b[K");
        let back = self.len - self.cursor;
        if back > 0 {
            out!(out, "\x1b[{back}D");
        }
    }

    fn insert(&mut self, byte: u8, out: &mut Out) {
        if self.len == LINE_BYTES || self.prompt_width + self.len + 1 >= 80 {
            out.str("\x07");
            return;
        }
        self.line
            .copy_within(self.cursor..self.len, self.cursor + 1);
        self.line[self.cursor] = byte;
        self.len += 1;
        self.cursor += 1;
        if self.cursor == self.len {
            out.bytes(&[byte]);
        } else {
            self.redraw(out, 0);
        }
    }

    fn backspace(&mut self, out: &mut Out) {
        if self.cursor == 0 {
            return;
        }
        self.line
            .copy_within(self.cursor..self.len, self.cursor - 1);
        self.len -= 1;
        self.cursor -= 1;
        self.redraw(out, 1);
    }

    fn delete(&mut self, out: &mut Out) {
        if self.cursor == self.len {
            return;
        }
        self.line
            .copy_within(self.cursor + 1..self.len, self.cursor);
        self.len -= 1;
        self.redraw(out, 1);
    }

    fn delete_word(&mut self, out: &mut Out) {
        let mut start = self.cursor;
        while start > 0 && self.line[start - 1] == b' ' {
            start -= 1;
        }
        while start > 0 && self.line[start - 1] != b' ' {
            start -= 1;
        }
        let removed = self.cursor - start;
        self.line.copy_within(self.cursor..self.len, start);
        self.len -= removed;
        self.cursor = start;
        self.redraw(out, removed);
    }

    fn left(&mut self, out: &mut Out) {
        if self.cursor > 0 {
            self.cursor -= 1;
            out.str("\x1b[D");
        }
    }

    fn right(&mut self, out: &mut Out) {
        if self.cursor < self.len {
            self.cursor += 1;
            out.str("\x1b[C");
        }
    }

    fn home(&mut self, out: &mut Out) {
        self.cursor = 0;
        self.redraw(out, 0);
    }

    fn end(&mut self, out: &mut Out) {
        self.cursor = self.len;
        self.redraw(out, 0);
    }
}

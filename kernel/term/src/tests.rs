// SPDX-License-Identifier: Apache-2.0

use super::*;

fn text(terminal: &Terminal, row: usize) -> String {
    let mut out = [0u8; COLS];
    let length = terminal.row_text(row, &mut out);
    String::from_utf8_lossy(&out[..length]).into_owned()
}

fn feed(bytes: &[u8]) -> Terminal {
    let mut terminal = Terminal::new();
    terminal.write(bytes);
    terminal
}

#[test]
fn prints_and_translates_newlines() {
    let terminal = feed(b"hello\nworld");
    assert_eq!(text(&terminal, 0), "hello");
    assert_eq!(text(&terminal, 1), "world");
    assert_eq!(terminal.cursor(), (5, 1));
}

#[test]
fn wraps_at_the_last_column_only_when_more_text_arrives() {
    let mut terminal = Terminal::new();
    terminal.write(&[b'x'; COLS]);
    assert_eq!(terminal.cursor(), (COLS - 1, 0));
    terminal.write(b"y");
    assert_eq!(text(&terminal, 1), "y");
}

#[test]
fn scrolls_when_the_bottom_line_feeds() {
    let mut terminal = Terminal::new();
    for line in 0..ROWS + 2 {
        terminal.write(format!("line {line}\n").as_bytes());
    }
    assert_eq!(text(&terminal, 0), "line 3");
    assert_eq!(text(&terminal, ROWS - 2), format!("line {}", ROWS + 1));
    assert_eq!(text(&terminal, ROWS - 1), "");
}

#[test]
fn cursor_addressing_and_erase() {
    let mut terminal = feed(b"abcdef\x1b[1;3H\x1b[K");
    assert_eq!(text(&terminal, 0), "ab");
    terminal.write(b"\x1b[2J\x1b[5;10HX");
    assert_eq!(text(&terminal, 0), "");
    assert_eq!(text(&terminal, 4), "         X");
    terminal.write(b"\x1b[H\x1b[3BZ");
    assert_eq!(text(&terminal, 3), "Z");
}

#[test]
fn sgr_colours_and_reset() {
    let terminal = feed(b"\x1b[1;31mR\x1b[0mN\x1b[44;97mB\x1b[38;5;196mE");
    let red = terminal.cell(0, 0);
    assert_eq!((red.fg, red.attr & ATTR_BOLD), (1, ATTR_BOLD));
    assert_eq!(red.colors().0, PALETTE[9]);
    assert_eq!(
        terminal.cell(1, 0),
        Cell {
            code: b'N',
            ..Cell::BLANK
        }
    );
    let blue = terminal.cell(2, 0);
    assert_eq!((blue.fg, blue.bg), (15, 4));
    // 256-colour red maps to a red palette entry, and the parameters after
    // `38;5` are not misread as attributes.
    let extended = terminal.cell(3, 0);
    assert!(matches!(extended.fg, 1 | 9));
    assert_eq!(extended.bg, 4);
}

#[test]
fn inverse_swaps_colours() {
    let terminal = feed(b"\x1b[7mI");
    assert_eq!(terminal.cell(0, 0).colors(), (DEFAULT_BG, DEFAULT_FG));
}

#[test]
fn alternate_screen_restores_the_main_screen() {
    let mut terminal = feed(b"$ htop");
    terminal.write(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen");
    assert!(terminal.alternate_screen());
    assert_eq!(text(&terminal, 0), "full screen");
    terminal.write(b"\x1b[?1049l");
    assert!(!terminal.alternate_screen());
    assert_eq!(text(&terminal, 0), "$ htop");
    assert_eq!(terminal.cursor(), (6, 0));
}

#[test]
fn cursor_visibility() {
    let mut terminal = feed(b"\x1b[?25l");
    assert!(!terminal.cursor_visible());
    terminal.write(b"\x1b[?25h");
    assert!(terminal.cursor_visible());
}

#[test]
fn scroll_region_limits_scrolling() {
    let mut terminal = Terminal::new();
    terminal.write(b"header\x1b[2;4r");
    terminal.write(b"\x1b[2Ha\nb\nc\nd");
    assert_eq!(text(&terminal, 0), "header");
    assert_eq!(text(&terminal, 1), "b");
    assert_eq!(text(&terminal, 2), "c");
    assert_eq!(text(&terminal, 3), "d");
    assert_eq!(text(&terminal, 4), "");
}

#[test]
fn insert_and_delete_characters_and_lines() {
    let mut terminal = feed(b"abcdef\x1b[1;2H\x1b[2P");
    assert_eq!(text(&terminal, 0), "adef");
    terminal.write(b"\x1b[2@");
    assert_eq!(text(&terminal, 0), "a  def");
    terminal.write(b"\x1b[2;1Hsecond\x1b[1;1H\x1b[L");
    assert_eq!(text(&terminal, 0), "");
    assert_eq!(text(&terminal, 1), "a  def");
    terminal.write(b"\x1b[M");
    assert_eq!(text(&terminal, 0), "a  def");
    assert_eq!(text(&terminal, 1), "second");
}

#[test]
fn cursor_position_report_is_queued_as_input() {
    let mut terminal = feed(b"\x1b[12;34H\x1b[6n");
    let mut reply = [0u8; 16];
    let length = terminal.take_reply(&mut reply);
    assert_eq!(&reply[..length], b"\x1b[12;34R");
    assert_eq!(terminal.take_reply(&mut reply), 0);
}

#[test]
fn utf8_maps_known_glyphs_and_replaces_others() {
    let terminal = feed("█│é".as_bytes());
    assert_eq!(terminal.cell(0, 0).code, glyph_code(0x2588));
    assert!(terminal.cell(0, 0).code >= 128);
    assert_eq!(terminal.cell(1, 0).code, glyph_code(0x2502));
    assert_eq!(terminal.cell(2, 0).code, REPLACEMENT);
    assert_eq!(glyph(terminal.cell(0, 0).code)[8], 0xff);
}

#[test]
fn hostile_sequences_stay_inside_the_grid() {
    let mut terminal = Terminal::new();
    terminal.write(b"\x1b[9999;9999H\x1b[99999C\x1b[65535;65535;1;2;3;4;5;6;7;8;9m");
    terminal.write(b"\x1b]0;title\x07\x1b]2;x\x1b\\\x1b[999L\x1b[999M\x1b[999P\x1b[999@");
    terminal.write(b"\x1b[0;0r\x1b[50;2r\x1b[999S\x1b[999T\x1b[999X\xff\xc3");
    terminal.write(&(0u8..=255).collect::<Vec<_>>());
    let (x, y) = terminal.cursor();
    assert!(x < COLS && y < ROWS);
}

#[test]
fn osc_title_is_ignored() {
    let terminal = feed(b"\x1b]0;window title\x07ok");
    assert_eq!(text(&terminal, 0), "ok");
}

#[test]
fn reset_clears_everything() {
    let mut terminal = feed(b"\x1b[31mtext\x1b[?25l");
    let before = terminal.generation();
    terminal.write(b"\x1bc");
    assert_eq!(text(&terminal, 0), "");
    assert!(terminal.cursor_visible());
    assert!(terminal.generation() > before);
}

#[test]
fn keys_encode_like_xterm() {
    let mut out = [0u8; 8];
    let none = Modifiers::default();
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let mut encode = |key, modifiers| {
        let length = encode_key(key, modifiers, &mut out);
        out[..length].to_vec()
    };
    assert_eq!(encode(Key::Char(b'a'), none), b"a");
    assert_eq!(encode(Key::Char(b'a'), shift), b"A");
    assert_eq!(encode(Key::Char(b'1'), shift), b"!");
    assert_eq!(encode(Key::Char(b'c'), ctrl), [3]);
    assert_eq!(encode(Key::Char(b'x'), ctrl), [0x18]);
    assert_eq!(encode(Key::Enter, none), b"\r");
    assert_eq!(encode(Key::Backspace, none), [0x7f]);
    assert_eq!(encode(Key::Up, none), b"\x1b[A");
    assert_eq!(encode(Key::Delete, none), b"\x1b[3~");
    assert_eq!(encode(Key::Function(10), none), b"\x1b[21~");
    assert_eq!(encode(Key::Function(13), none), b"");
}

#[test]
fn random_streams_never_panic() {
    // Deterministic xorshift stream biased toward escape-sequence bytes.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let alphabet = b"\x1b[;?0123456789HJKLMPSTXmhlrsu@ABCDEFGd\n\r\x08\t]\x07\\abc\xe2\x96\x88";
    let mut terminal = Terminal::new();
    for _ in 0..20_000 {
        let mut chunk = [0u8; 64];
        for byte in &mut chunk {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = alphabet[(state % alphabet.len() as u64) as usize];
        }
        terminal.write(&chunk);
        let (x, y) = terminal.cursor();
        assert!(x < COLS && y < ROWS);
    }
}

#[test]
fn unterminated_osc_gives_the_screen_back() {
    // A program that opens a command and never ends it must not swallow all
    // later output.
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(core::iter::repeat_n(b'x', 600));
    bytes.extend_from_slice(b"\rvisible");
    let terminal = feed(&bytes);
    assert!((0..ROWS).any(|row| text(&terminal, row).starts_with("visible")));
}

#[test]
fn cancel_and_escape_end_an_osc() {
    let terminal = feed(b"\x1b]0;abc\x18one\r\n\x1b]0;def\x1b[1mtwo");
    assert_eq!(text(&terminal, 0), "one");
    assert_eq!(text(&terminal, 1), "two");
}

// SPDX-License-Identifier: Apache-2.0
//! Host tests for the GridCraft application model.

use crate::{App, AppKey, COLS, Error, Mode, ROWS, Sheet, format_number, self_test};

fn number(sheet: &Sheet, row: usize, column: usize) -> Result<f64, Error> {
    sheet.number(row, column)
}

#[test]
fn arithmetic_precedence_and_parentheses() {
    let mut sheet = Sheet::new();
    sheet.set_text(0, 0, b"=1+2*3");
    sheet.set_text(0, 1, b"=(1+2)*3");
    sheet.set_text(0, 2, b"=-2*-3");
    sheet.set_text(0, 3, b"=10/4");
    assert_eq!(number(&sheet, 0, 0), Ok(7.0));
    assert_eq!(number(&sheet, 0, 1), Ok(9.0));
    assert_eq!(number(&sheet, 0, 2), Ok(6.0));
    assert_eq!(number(&sheet, 0, 3), Ok(2.5));
}

#[test]
fn references_ranges_and_functions() {
    let mut sheet = Sheet::new();
    let values: [&[u8]; 3] = [b"2", b"3", b"4"];
    for (row, value) in values.iter().enumerate() {
        sheet.set_text(row, 0, value);
    }
    sheet.set_text(0, 1, b"=SUM(A1:A3)");
    sheet.set_text(1, 1, b"=AVERAGE(A1:A3)");
    sheet.set_text(2, 1, b"=MIN(A1:A3)");
    sheet.set_text(3, 1, b"=MAX(A1:A3)");
    sheet.set_text(4, 1, b"=COUNT(A1:A3)");
    sheet.set_text(5, 1, b"=SUM(A1:A3, 6)");
    sheet.set_text(6, 1, b"=sum(a1:a3)");
    assert_eq!(number(&sheet, 0, 1), Ok(9.0));
    assert_eq!(number(&sheet, 1, 1), Ok(3.0));
    assert_eq!(number(&sheet, 2, 1), Ok(2.0));
    assert_eq!(number(&sheet, 3, 1), Ok(4.0));
    assert_eq!(number(&sheet, 4, 1), Ok(3.0));
    assert_eq!(number(&sheet, 5, 1), Ok(15.0));
    assert_eq!(number(&sheet, 6, 1), Ok(9.0));
}

#[test]
fn formula_errors_are_stable() {
    let mut sheet = Sheet::new();
    sheet.set_text(0, 0, b"=1/0");
    sheet.set_text(1, 0, b"=NOPE(1)");
    sheet.set_text(2, 0, b"=A11");
    sheet.set_text(3, 0, b"=1+");
    sheet.set_text(4, 0, b"hi");
    sheet.set_text(0, 1, b"=B2");
    sheet.set_text(1, 1, b"=B1");
    assert_eq!(number(&sheet, 0, 0), Err(Error::DivZero));
    assert_eq!(number(&sheet, 1, 0), Err(Error::Name));
    assert_eq!(number(&sheet, 2, 0), Err(Error::Name));
    assert_eq!(number(&sheet, 3, 0), Err(Error::Parse));
    assert_eq!(number(&sheet, 4, 0), Err(Error::Value));
    assert_eq!(number(&sheet, 0, 1), Err(Error::Circular));
    assert_eq!(number(&sheet, 1, 1), Err(Error::Circular));

    let mut display = [0u8; 16];
    let length = sheet.display(0, 0, &mut display);
    assert_eq!(&display[..length], b"ERR:DIV/0");
}

#[test]
fn editing_commits_and_cancels() {
    let mut app = App::new();
    app.on_key(AppKey::Enter);
    assert_eq!(app.mode(), Mode::Edit);
    for character in b"=1+2" {
        app.on_key(AppKey::Char(*character));
    }
    app.on_key(AppKey::Enter);
    assert_eq!(app.mode(), Mode::Ready);
    assert_eq!(app.sheet.text(0, 0), b"=1+2");
    let mut display = [0u8; 8];
    let length = app.sheet.display(0, 0, &mut display);
    assert_eq!(&display[..length], b"3");

    app.on_key(AppKey::Enter);
    app.on_key(AppKey::Char(b'9'));
    app.on_key(AppKey::Escape);
    assert_eq!(app.sheet.text(0, 0), b"=1+2");

    app.on_key(AppKey::Enter);
    app.on_key(AppKey::Backspace);
    app.on_key(AppKey::Enter);
    assert_eq!(app.sheet.text(0, 0), b"=1+");
}

#[test]
fn selection_is_bounded_and_commits() {
    let mut app = App::new();
    app.on_key(AppKey::Enter);
    app.on_key(AppKey::Char(b'1'));
    app.on_key(AppKey::Down);
    assert_eq!(app.row(), 1);
    assert_eq!(app.sheet.text(0, 0), b"1");
    app.on_key(AppKey::Up);
    app.on_key(AppKey::Left);
    assert_eq!((app.row(), app.column()), (0, 0));
    for _ in 0..ROWS + 2 {
        app.on_key(AppKey::Down);
    }
    for _ in 0..COLS + 2 {
        app.on_key(AppKey::Right);
    }
    assert_eq!((app.row(), app.column()), (ROWS - 1, COLS - 1));
}

#[test]
fn backspace_clears_a_selected_cell() {
    let mut app = App::new();
    app.sheet.set_text(0, 0, b"text");
    app.on_key(AppKey::Backspace);
    assert!(app.sheet.is_empty(0, 0));
}

#[test]
fn number_formatting_is_compact_and_stable() {
    let mut buffer = [0u8; 24];
    for (value, expected) in [
        (0.0, "0"),
        (1.0, "1"),
        (1.5, "1.5"),
        (-2.25, "-2.25"),
        (0.000_000_1, "1E-7"),
        (12_345_678.0, "12345678"),
    ] {
        let length = format_number(value, &mut buffer);
        assert_eq!(&buffer[..length], expected.as_bytes());
    }
}

#[test]
fn sample_workbook_calculates() {
    let app = App::sample();
    assert!(app.used_cells() >= 26);
    assert_eq!(app.sheet.number(1, 3), Ok(9.0));
    assert_eq!(app.sheet.number(6, 3), Ok(103.0));
    assert_eq!(app.sheet.number(1, 4), Ok(9.0 / 103.0 * 100.0));
}

#[test]
fn csv_export_contains_used_rows() {
    let app = App::sample();
    let mut buffer = [0u8; 512];
    let length = app.sheet.write_csv(&mut buffer);
    let text = core::str::from_utf8(&buffer[..length]).unwrap_or("");
    assert!(text.starts_with("ITEM,QTY,PRICE,TOTAL,SHARE\n"));
    assert!(text.contains("TOTAL,,,=SUM(D2:D6)"));
}

#[test]
fn address_uses_spreadsheet_notation() {
    let mut app = App::new();
    app.move_cursor(9, 9);
    let mut address = [0u8; 8];
    let length = app.address(&mut address);
    assert_eq!(&address[..length], b"J10");
}

#[test]
fn boot_self_test_passes() {
    let report = self_test();
    assert_eq!(report.failures, 0);
    assert!(report.checks >= 10);
}

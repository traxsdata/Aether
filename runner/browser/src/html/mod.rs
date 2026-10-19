//! html - bytes to a document tree: charset detection and decoding, the
//! tokenizer, and the tree builder.

pub mod entities;
pub mod tokenizer;
pub mod treebuilder;

use crate::dom::Document;

pub use treebuilder::parse;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Windows1252,
    Utf16Le,
    Utf16Be,
}

impl Charset {
    pub fn name(self) -> &'static str {
        match self {
            Charset::Utf8 => "UTF-8",
            Charset::Windows1252 => "windows-1252",
            Charset::Utf16Le => "UTF-16LE",
            Charset::Utf16Be => "UTF-16BE",
        }
    }

    /// A WHATWG encoding label, for the encodings we decode. Labels of
    /// encodings we don't have give None (the caller falls back).
    pub fn from_label(label: &str) -> Option<Charset> {
        let l = label.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
        Some(match l.as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "x-unicode20utf8" => Charset::Utf8,
            "windows-1252" | "cp1252" | "x-cp1252" | "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "latin1" | "l1" | "us-ascii"
            | "ascii" | "ansi_x3.4-1968" | "iso-ir-100" | "cp819" | "ibm819" | "csisolatin1" | "iso-8859-15" | "iso8859-15"
            | "latin9" | "l9" | "iso_8859-15" => Charset::Windows1252,
            "utf-16le" | "utf-16" | "unicode" | "ucs-2" | "csunicode" => Charset::Utf16Le,
            "utf-16be" | "unicodefffe" => Charset::Utf16Be,
            _ => return None,
        })
    }
}

/// The charset parameter of a Content-Type header value.
pub fn content_type_charset(ct: &str) -> Option<Charset> {
    for part in ct.split(';').skip(1) {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().eq_ignore_ascii_case("charset") {
                return Charset::from_label(v);
            }
        }
    }
    None
}

/// Looks for <meta charset> or <meta http-equiv=content-type> in the first
/// 1024 bytes (the spec's prescan, simplified).
fn prescan(bytes: &[u8]) -> Option<Charset> {
    let head = &bytes[..bytes.len().min(1024)];
    let text: String = head.iter().map(|&b| if b < 0x80 { (b as char).to_ascii_lowercase() } else { '?' }).collect();
    let mut from = 0;
    while let Some(i) = text[from..].find("<meta") {
        let at = from + i;
        let end = text[at..].find('>').map_or(text.len(), |j| at + j);
        let tag = &text[at..end];
        if let Some(c) = tag.find("charset") {
            let rest = tag[c + 7..].trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v: String = rest
                    .trim_start()
                    .trim_start_matches(['"', '\''])
                    .chars()
                    .take_while(|c| !matches!(c, '"' | '\'' | ' ' | ';' | '/' | '>'))
                    .collect();
                if let Some(cs) = Charset::from_label(&v) {
                    // A page can't claim to be UTF-16 from inside itself.
                    return Some(match cs {
                        Charset::Utf16Le | Charset::Utf16Be => Charset::Utf8,
                        c => c,
                    });
                }
            }
        }
        from = end;
    }
    None
}

/// Decodes `bytes`: a byte order mark wins, then the HTTP charset, then a
/// <meta> declaration, then UTF-8 if the bytes are valid UTF-8, and
/// windows-1252 otherwise (the web's legacy default).
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, Charset) {
    let cs = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (decode_as(&bytes[3..], Charset::Utf8), Charset::Utf8);
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        return (decode_as(&bytes[2..], Charset::Utf16Le), Charset::Utf16Le);
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        return (decode_as(&bytes[2..], Charset::Utf16Be), Charset::Utf16Be);
    } else if let Some(cs) = content_type.and_then(content_type_charset) {
        cs
    } else if let Some(cs) = prescan(bytes) {
        cs
    } else if std::str::from_utf8(bytes).is_ok() {
        Charset::Utf8
    } else {
        Charset::Windows1252
    };
    (decode_as(bytes, cs), cs)
}

pub fn decode_as(bytes: &[u8], cs: Charset) -> String {
    match cs {
        Charset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Charset::Windows1252 => bytes.iter().map(|&b| windows_1252(b)).collect(),
        Charset::Utf16Le | Charset::Utf16Be => {
            let units = bytes.chunks_exact(2).map(|p| if cs == Charset::Utf16Le { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) });
            char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
        }
    }
}

pub fn windows_1252(b: u8) -> char {
    const HIGH: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F,
        0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&b) {
        char::from_u32(HIGH[(b - 0x80) as usize] as u32).unwrap_or('\u{FFFD}')
    } else {
        b as char
    }
}

/// Bytes to a document, with the charset used.
pub fn parse_bytes(bytes: &[u8], content_type: Option<&str>) -> (Document, Charset) {
    let (text, cs) = decode(bytes, content_type);
    (parse(&text), cs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charsets() {
        assert_eq!(decode("héllo".as_bytes(), None), ("héllo".into(), Charset::Utf8));
        assert_eq!(decode(b"h\xe9llo \x93q\x94", None), ("héllo \u{201C}q\u{201D}".into(), Charset::Windows1252));
        assert_eq!(decode(b"<meta charset=\"iso-8859-1\">\xe9", None).1, Charset::Windows1252);
        assert_eq!(decode(b"<meta http-equiv=Content-Type content='text/html; charset=windows-1252'>", None).1, Charset::Windows1252);
        assert_eq!(decode("é".as_bytes(), Some("text/html; charset=ISO-8859-1")).0, "Ã©");
        assert_eq!(decode(b"\xef\xbb\xbfok", Some("text/html; charset=latin1")).0, "ok");
        assert_eq!(decode(b"\xff\xfeh\0i\0", None).0, "hi");
        assert_eq!(decode(b"\xfe\xff\0h\0i\xd8\x00", None).0, "hi\u{FFFD}");
        // Unknown labels fall back to sniffing.
        assert_eq!(decode("ü".as_bytes(), Some("text/html; charset=klingon")).1, Charset::Utf8);
    }
}

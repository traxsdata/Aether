//! font.rs - Facet at any size: æther's own stroke font, rasterized from its
//! outlines (font_data.rs, generated from tools/mkfont.py) with the same
//! geometry mkfont.py uses for include/font_data.h, plus bold (a thicker
//! stroke), italic (a shear) and proportional spacing for body text.
//!
//! Characters Facet doesn't have are drawn by composing accents onto base
//! letters (é, ü, ñ, č ...), from a few extra glyphs designed for the browser
//! in the same style (curly quotes, dashes, bullets, arrows, currency), from
//! ASCII stand-ins ("(c)" for ©), or as an outlined box.
//!
//! Sizes: a CSS font-size of `fs` px draws Facet in a cell `fs * 1.25` px
//! tall, so its cap height is 0.7 em and its x-height a little under 0.5 em,
//! like ordinary text faces.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::font_data::FACET;

/// Cell height per CSS px of font-size.
pub const CELL: f32 = 1.25;
/// Grid geometry, as in mkfont.py's rasterize().
const PAD: f32 = 0.19; // of the cell width (half the height)
const SY: f32 = 0.093; // of the cell height per grid unit
const OY: f32 = 0.035; // of the cell height
/// Proportional spacing, in grid units.
const GAP: f32 = 1.35;
const SPACE: f32 = 2.6;

type Outline = Vec<Vec<(f32, f32)>>;

struct Glyphs {
    map: HashMap<char, Outline>,
    /// Horizontal ink extent of each glyph, in grid units.
    extent: HashMap<char, (f32, f32)>,
}

fn parse_spec(spec: &str) -> Outline {
    spec.split('|')
        .filter(|p| !p.trim().is_empty())
        .map(|part| {
            part.split_whitespace()
                .filter_map(|pt| {
                    let (x, y) = pt.split_once(',')?;
                    Some((x.parse().ok()?, y.parse().ok()?))
                })
                .collect()
        })
        .collect()
}

/// Extra glyphs designed for the browser in Facet's style (same grid).
const EXTRA: &[(char, &str)] = &[
    ('\u{2018}', "2,3 2,2 2.7,1"),                       // ‘
    ('\u{2019}', "2,1 2,2 1.3,3"),                       // ’
    ('\u{201C}', "1.3,3 1.3,2 2,1|2.9,3 2.9,2 3.6,1"),   // “
    ('\u{201D}', "1.1,1 1.1,2 0.4,3|2.7,1 2.7,2 2,3"),   // ”
    ('\u{2013}', "0,4.5 4,4.5"),                         // –
    ('\u{2014}', "-0.8,4.5 4.8,4.5"),                    // —
    ('\u{2022}', "1.3,3.9 2.7,3.9|1.1,4.5 2.9,4.5|1.3,5.1 2.7,5.1"), // •
    ('\u{2026}', "0.3,6.95 0.3,7|2,6.95 2,7|3.7,6.95 3.7,7"), // …
    ('\u{00B7}', "2,4.45 2,4.55"),                       // ·
    ('\u{20AC}', "4,2 3,1 1.5,1 0.5,2 0.5,6 1.5,7 3,7 4,6|-0.3,3.5 2.6,3.5|-0.3,4.7 2.6,4.7"), // €
    ('\u{00A3}', "3.8,2 3,1 2,1 1,2 1,7|0,7 4,7|0,4 2.8,4"), // £
    ('\u{00A5}', "0,1 2,4 4,1|2,4 2,7|0.8,4.6 3.2,4.6|0.8,5.7 3.2,5.7"), // ¥
    ('\u{00A2}', "4,3 1,3 0,4 0,6 1,7 4,7|2.4,2 2.4,8"), // ¢
    ('\u{00B0}', "1.5,1 2.5,1 2.5,2 1.5,2 1.5,1"),        // °
    ('\u{00B1}', "2,2.5 2,5.5|0,4 4,4|0,7 4,7"),          // ±
    ('\u{00D7}', "0.5,3.5 3.5,6.5|3.5,3.5 0.5,6.5"),      // ×
    ('\u{00F7}', "0,4.5 4,4.5|2,2.95 2,3.05|2,5.95 2,6.05"), // ÷
    ('\u{00AB}', "2,3 0,4.75 2,6.5|4,3 2,4.75 4,6.5"),    // «
    ('\u{00BB}', "0,3 2,4.75 0,6.5|2,3 4,4.75 2,6.5"),    // »
    ('\u{00A1}', "2,2.95 2,3|2,4.5 2,8.5"),               // ¡
    ('\u{00BF}', "4,8 3,9 1,9 0,8 0,7 2,5.5 2,5|2,2.95 2,3"), // ¿
    ('\u{2192}', "0,4.5 4,4.5|2.5,3 4,4.5 2.5,6"),       // →
    ('\u{2190}', "4,4.5 0,4.5|1.5,3 0,4.5 1.5,6"),       // ←
    ('\u{2191}', "2,7 2,2|0.5,3.5 2,2 3.5,3.5"),         // ↑
    ('\u{2193}', "2,2 2,7|0.5,5.5 2,7 3.5,5.5"),         // ↓
    ('\u{25B6}', "1,2 4,4.5 1,7 1,2|1.8,3.6 2.6,4.5 1.8,5.4"), // ▶
    ('\u{25C0}', "3,2 0,4.5 3,7 3,2|2.2,3.6 1.4,4.5 2.2,5.4"), // ◀
    ('\u{25B2}', "0,6.5 2,2.5 4,6.5 0,6.5"),             // ▲
    ('\u{25BC}', "0,2.5 2,6.5 4,2.5 0,2.5"),             // ▼
    ('\u{2713}', "0,4.5 1.5,6.5 4,2"),                    // ✓
    ('\u{2717}', "0.5,2.5 3.5,6.5|3.5,2.5 0.5,6.5"),      // ✗
    ('\u{00DF}', "0,7 0,2 1,1 2.5,1 3.5,2 3.5,2.6 2.5,3.6 3.5,4.3 4,5 4,6 3,7 2,7"), // ß
    ('\u{00F8}', "1,3 3,3 4,4 4,6 3,7 1,7 0,6 0,4 1,3|0,7.4 4,2.6"), // ø
    ('\u{00D8}', "1,1 3,1 4,2 4,6 3,7 1,7 0,6 0,2 1,1|-0.2,7.6 4.2,0.4"), // Ø
    ('\u{0153}', "2,3.6 1.6,3 0.4,3 0,3.6 0,6.4 0.4,7 1.6,7 2,6.4 2,3.6|2,5 4,5 4,3.6 3.5,3 2.5,3 2,3.6|2,6.4 2.5,7 4,7"), // œ
    ('\u{0152}', "4,1 1,1 0,2 0,6 1,7 4,7|2,1 2,7|2,4 3.5,4"), // Œ
    ('\u{0142}', "1,1 2,1 2,6 3,7 4,7|0.9,4.6 3,3.2"),    // ł
    ('\u{0141}', "0,1 0,7 4,7|-0.6,4.8 1.6,3.3"),         // Ł
    ('\u{0131}', "1,3 2,3 2,7|1,7 3,7"),                  // ı
    ('\u{00B5}', "0,3 0,9|0,6 1,7 4,7|4,3 4,7"),          // µ
    ('\u{00FE}', "0,1 0,9|0,3 3,3 4,4 4,6 3,7 0,7"),      // þ
    ('\u{00DE}', "0,1 0,7|0,2.5 3,2.5 4,3.5 4,4.5 3,5.5 0,5.5"), // Þ
    ('\u{00F0}', "1,1.5 3,2.5|4,4.5 4,6 3,7 1,7 0,6 0,4.5 1,3.5 4,3.5|4,4.5 2,1"), // ð
    ('\u{00D0}', "0,1 0,7 3,7 4,6 4,2 3,1 0,1|-0.8,4 1.5,4"), // Ð
    ('\u{00A7}', "3.5,1.5 1,1.5 0.5,2.2 1,3 3,4 3.5,4.8 3,5.5|1,3.5 0.5,4.2 1,5 3,6 3.5,6.8 3,7.5 0.5,7.5"), // §
    ('\u{00B6}', "4,1 4,7|3,1 3,7|4,1 1.5,1 0.5,2 0.5,3 1.5,4 3,4"), // ¶
    ('\u{FFFD}', "0,1 4,1 4,7 0,7 0,1|1.4,3 2,2.4 2.6,3 2,3.8 2,4.6|2,5.6 2,5.7"), // replacement
];

/// The outlined box drawn for characters Facet can't show.
const TOFU: &str = "0.2,1 3.8,1 3.8,7 0.2,7 0.2,1";

/// Accents in the space above lowercase letters (y 1..2.4).
fn accent(a: char) -> &'static str {
    match a {
        '\u{301}' => "1.6,2.3 2.8,1.1",
        '\u{300}' => "1.2,1.1 2.4,2.3",
        '\u{302}' => "0.8,2.3 2,1.1 3.2,2.3",
        '\u{30C}' => "0.8,1.1 2,2.3 3.2,1.1",
        '\u{308}' => "1,1.65 1,1.75|3,1.65 3,1.75",
        '\u{303}' => "0.4,2.1 1.3,1.3 2.7,2.1 3.6,1.3",
        '\u{30A}' => "1.4,0.9 2.6,0.9 2.6,2.1 1.4,2.1 1.4,0.9",
        '\u{304}' => "0.8,1.7 3.2,1.7",
        '\u{306}' => "0.8,1.1 1.4,2 2.6,2 3.2,1.1",
        '\u{30B}' => "1,2.3 1.8,1.1|2.4,2.3 3.2,1.1",
        '\u{307}' => "2,1.65 2,1.75",
        '\u{327}' => "2,7 2,7.8 1.2,8.6",
        '\u{328}' => "3,7 2.2,7.8 2.6,8.6 3.6,8.6",
        _ => "",
    }
}

/// Base letter + combining accent for the precomposed letters we compose.
fn decompose(c: char) -> Option<(char, char)> {
    const ACUTE: char = '\u{301}';
    const GRAVE: char = '\u{300}';
    const CIRC: char = '\u{302}';
    const TILDE: char = '\u{303}';
    const DIAER: char = '\u{308}';
    const RING: char = '\u{30A}';
    const CARON: char = '\u{30C}';
    const CEDIL: char = '\u{327}';
    const MACRON: char = '\u{304}';
    const BREVE: char = '\u{306}';
    const DACUTE: char = '\u{30B}';
    const DOT: char = '\u{307}';
    const OGONEK: char = '\u{328}';
    let table: &[(char, char, char)] = &[
        ('À', 'A', GRAVE), ('Á', 'A', ACUTE), ('Â', 'A', CIRC), ('Ã', 'A', TILDE), ('Ä', 'A', DIAER), ('Å', 'A', RING),
        ('Ç', 'C', CEDIL), ('È', 'E', GRAVE), ('É', 'E', ACUTE), ('Ê', 'E', CIRC), ('Ë', 'E', DIAER),
        ('Ì', 'I', GRAVE), ('Í', 'I', ACUTE), ('Î', 'I', CIRC), ('Ï', 'I', DIAER), ('Ñ', 'N', TILDE),
        ('Ò', 'O', GRAVE), ('Ó', 'O', ACUTE), ('Ô', 'O', CIRC), ('Õ', 'O', TILDE), ('Ö', 'O', DIAER),
        ('Ù', 'U', GRAVE), ('Ú', 'U', ACUTE), ('Û', 'U', CIRC), ('Ü', 'U', DIAER), ('Ý', 'Y', ACUTE),
        ('à', 'a', GRAVE), ('á', 'a', ACUTE), ('â', 'a', CIRC), ('ã', 'a', TILDE), ('ä', 'a', DIAER), ('å', 'a', RING),
        ('ç', 'c', CEDIL), ('è', 'e', GRAVE), ('é', 'e', ACUTE), ('ê', 'e', CIRC), ('ë', 'e', DIAER),
        ('ì', 'ı', GRAVE), ('í', 'ı', ACUTE), ('î', 'ı', CIRC), ('ï', 'ı', DIAER), ('ñ', 'n', TILDE),
        ('ò', 'o', GRAVE), ('ó', 'o', ACUTE), ('ô', 'o', CIRC), ('õ', 'o', TILDE), ('ö', 'o', DIAER),
        ('ù', 'u', GRAVE), ('ú', 'u', ACUTE), ('û', 'u', CIRC), ('ü', 'u', DIAER), ('ý', 'y', ACUTE), ('ÿ', 'y', DIAER),
        ('Ā', 'A', MACRON), ('ā', 'a', MACRON), ('Ă', 'A', BREVE), ('ă', 'a', BREVE), ('Ą', 'A', OGONEK), ('ą', 'a', OGONEK),
        ('Ć', 'C', ACUTE), ('ć', 'c', ACUTE), ('Č', 'C', CARON), ('č', 'c', CARON), ('Ď', 'D', CARON), ('ď', 'd', CARON),
        ('Ē', 'E', MACRON), ('ē', 'e', MACRON), ('Ė', 'E', DOT), ('ė', 'e', DOT), ('Ę', 'E', OGONEK), ('ę', 'e', OGONEK),
        ('Ě', 'E', CARON), ('ě', 'e', CARON), ('Ğ', 'G', BREVE), ('ğ', 'g', BREVE), ('Ī', 'I', MACRON), ('ī', 'ı', MACRON),
        ('İ', 'I', DOT), ('Ń', 'N', ACUTE), ('ń', 'n', ACUTE), ('Ň', 'N', CARON), ('ň', 'n', CARON),
        ('Ō', 'O', MACRON), ('ō', 'o', MACRON), ('Ő', 'O', DACUTE), ('ő', 'o', DACUTE), ('Ř', 'R', CARON), ('ř', 'r', CARON),
        ('Ś', 'S', ACUTE), ('ś', 's', ACUTE), ('Ş', 'S', CEDIL), ('ş', 's', CEDIL), ('Š', 'S', CARON), ('š', 's', CARON),
        ('Ť', 'T', CARON), ('ť', 't', CARON), ('Ū', 'U', MACRON), ('ū', 'u', MACRON), ('Ů', 'U', RING), ('ů', 'u', RING),
        ('Ű', 'U', DACUTE), ('ű', 'u', DACUTE), ('Ÿ', 'Y', DIAER), ('Ź', 'Z', ACUTE), ('ź', 'z', ACUTE),
        ('Ż', 'Z', DOT), ('ż', 'z', DOT), ('Ž', 'Z', CARON), ('ž', 'z', CARON),
    ];
    table.iter().find(|(p, _, _)| *p == c).map(|&(_, b, a)| (b, a))
}

/// ASCII stand-ins for characters without a glyph of their own. Used by
/// `normalize` before layout, so widths and drawing agree.
pub fn stand_in(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{00A9}' => "(c)",
        '\u{00AE}' => "(R)",
        '\u{2122}' => "TM",
        '\u{00BD}' => "1/2",
        '\u{00BC}' => "1/4",
        '\u{00BE}' => "3/4",
        '\u{00B9}' => "1",
        '\u{00B2}' => "2",
        '\u{00B3}' => "3",
        '\u{00AA}' => "a",
        '\u{00BA}' => "o",
        '\u{201A}' => ",",
        '\u{201E}' => "\u{201D}",
        '\u{2039}' => "<",
        '\u{203A}' => ">",
        '\u{2032}' => "'",
        '\u{2033}' => "\"",
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2212}' | '\u{2043}' => "-",
        '\u{2015}' => "\u{2014}",
        '\u{2605}' | '\u{2606}' | '\u{2731}' => "*",
        '\u{2714}' => "\u{2713}",
        '\u{2718}' | '\u{2715}' | '\u{2716}' => "\u{2717}",
        '\u{25B8}' | '\u{25BA}' | '\u{276F}' => "\u{25B6}",
        '\u{25C2}' | '\u{25C4}' | '\u{276E}' => "\u{25C0}",
        '\u{25E6}' | '\u{25CF}' | '\u{25AA}' | '\u{25A0}' | '\u{2023}' | '\u{2219}' => "\u{2022}",
        '\u{00A6}' => "|",
        '\u{00AC}' => "-",
        '\u{00A4}' => "$",
        '\u{2264}' => "<=",
        '\u{2265}' => ">=",
        '\u{2260}' => "!=",
        '\u{2248}' => "~",
        '\u{221E}' => "oo",
        '\u{21D2}' => "=>",
        '\u{21D0}' => "<=",
        '\u{2194}' => "<->",
        _ => return None,
    })
}

/// Prepares text for Facet: spaces of every width become ' ', no-break
/// spaces become U+00A0 (kept, for line breaking), invisible characters
/// are removed, and characters without a glyph get their ASCII stand-in.
pub fn normalize(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{00A0}' | '\u{202F}' | '\u{2007}' => o.push('\u{00A0}'),
            '\u{2000}'..='\u{2006}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{205F}' | '\u{3000}' => o.push(' '),
            '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {}
            '\u{FE00}'..='\u{FE0F}' => {} // variation selectors
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => match stand_in(c) {
                Some(s) => o.push_str(s),
                None => o.push(c),
            },
        }
    }
    o
}

fn glyphs() -> &'static Glyphs {
    static G: OnceLock<Glyphs> = OnceLock::new();
    G.get_or_init(|| {
        let mut map = HashMap::new();
        for &(c, spec) in FACET.iter().chain(EXTRA.iter()) {
            map.insert(c, parse_spec(spec));
        }
        map.insert('\u{00A0}', Vec::new());
        let mut extent = HashMap::new();
        for (c, o) in &map {
            extent.insert(*c, ink_extent(o));
        }
        Glyphs { map, extent }
    })
}

fn ink_extent(o: &Outline) -> (f32, f32) {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for p in o.iter().flatten() {
        lo = lo.min(p.0);
        hi = hi.max(p.0);
    }
    if lo > hi {
        (0.0, 0.0)
    } else {
        (lo, hi)
    }
}

/// The outline Facet draws for `c` (composed, extra, or the box).
fn outline(c: char) -> std::borrow::Cow<'static, Outline> {
    let g = glyphs();
    if let Some(o) = g.map.get(&c) {
        return std::borrow::Cow::Borrowed(o);
    }
    if let Some((base, acc)) = decompose(c) {
        let mut o = g.map.get(&base).cloned().unwrap_or_default();
        let mut a = parse_spec(accent(acc));
        let below = acc == '\u{327}' || acc == '\u{328}';
        if base.is_ascii_uppercase() && !below {
            // Above a capital: lift into the space over the cap height.
            for p in a.iter_mut().flatten() {
                p.1 = p.1 * 0.75 - 1.55;
            }
        }
        o.append(&mut a);
        return std::borrow::Cow::Owned(o);
    }
    std::borrow::Cow::Owned(parse_spec(TOFU))
}

/// How a run of text is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Face {
    /// CSS font-size in px (clamped 4..=200).
    pub size: u16,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

impl Face {
    pub fn new(size: f32, bold: bool, italic: bool, mono: bool) -> Face {
        Face { size: size.round().clamp(4.0, 200.0) as u16, bold, italic, mono }
    }
    pub fn cell_h(self) -> f32 {
        self.size as f32 * CELL
    }
    fn sx(self) -> f32 {
        let w = self.cell_h() / 2.0;
        (w - 2.0 * PAD * w) / 4.0
    }
    fn stroke(self) -> f32 {
        let h = self.cell_h();
        if self.bold {
            (h / 8.5).max(1.6)
        } else {
            (h / 14.0).max(1.0)
        }
    }
    /// Distance from the top of the line's em box to the baseline.
    pub fn ascent(self) -> f32 {
        self.cell_h() * (OY + 7.0 * SY)
    }
    pub fn descent(self) -> f32 {
        self.cell_h() * (2.0 * SY + 0.02)
    }
    /// Where the top of the glyph cell sits relative to the baseline.
    fn cell_top(self) -> f32 {
        -self.ascent()
    }

    /// Advance width of one (normalized) character.
    pub fn advance(self, c: char) -> f32 {
        let sx = self.sx();
        if self.mono {
            return self.cell_h() / 2.0;
        }
        if c == ' ' || c == '\u{00A0}' {
            return SPACE * sx + self.stroke() * 0.5;
        }
        if c == '\t' {
            return 4.0 * (SPACE * sx);
        }
        let (lo, hi) = match glyphs().extent.get(&c) {
            Some(&e) => e,
            None => ink_extent(&outline(c)),
        };
        (hi - lo + GAP) * sx + self.stroke()
    }

    pub fn width(self, s: &str) -> f32 {
        s.chars().map(|c| self.advance(c)).sum()
    }
}

/// A rasterized glyph: an alpha mask placed relative to the pen position
/// (x) and the baseline (y).
pub struct Glyph {
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    pub alpha: Vec<u8>,
}

#[derive(Default)]
pub struct Cache {
    glyphs: HashMap<(char, Face), std::rc::Rc<Glyph>>,
}

impl Cache {
    pub fn glyph(&mut self, c: char, face: Face) -> std::rc::Rc<Glyph> {
        if let Some(g) = self.glyphs.get(&(c, face)) {
            return g.clone();
        }
        if self.glyphs.len() > 6000 {
            self.glyphs.clear();
        }
        let g = std::rc::Rc::new(rasterize(c, face, 4));
        self.glyphs.insert((c, face), g.clone());
        g
    }
}

/// Rasterizes `c` with `ss` x `ss` supersampling.
pub fn rasterize(c: char, face: Face, ss: u32) -> Glyph {
    let o = outline(c);
    let h = face.cell_h();
    let sx = face.sx();
    let sy = h * SY;
    let stroke = face.stroke();
    // Glyph-space x of grid 0: proportional faces start the ink at the pen
    // plus half the gap; mono faces centre like mkfont.py's cells.
    let (lo, _) = ink_extent(&o);
    let x0 = if face.mono { PAD * h / 2.0 } else { (GAP * 0.5 - lo) * sx + stroke * 0.5 };
    let shear = if face.italic { 0.21 } else { 0.0 };
    let base = -face.cell_top(); // baseline y within the cell
    let mut segs: Vec<(f32, f32, f32, f32)> = Vec::new();
    for line in o.iter() {
        let pts: Vec<(f32, f32)> = line
            .iter()
            .map(|&(gx, gy)| {
                let y = OY * h + gy * sy - base;
                let x = x0 + gx * sx - shear * y;
                (x, y)
            })
            .collect();
        if pts.len() == 1 {
            segs.push((pts[0].0, pts[0].1, pts[0].0, pts[0].1));
        }
        for w in pts.windows(2) {
            segs.push((w[0].0, w[0].1, w[1].0, w[1].1));
        }
    }
    if segs.is_empty() {
        return Glyph { left: 0, top: 0, w: 0, h: 0, alpha: Vec::new() };
    }
    let r = stroke / 2.0;
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for s in &segs {
        minx = minx.min(s.0.min(s.2));
        maxx = maxx.max(s.0.max(s.2));
        miny = miny.min(s.1.min(s.3));
        maxy = maxy.max(s.1.max(s.3));
    }
    let gx0 = (minx - r - 1.0).floor() as i32;
    let gy0 = (miny - r - 1.0).floor() as i32;
    let gx1 = (maxx + r + 1.0).ceil() as i32;
    let gy1 = (maxy + r + 1.0).ceil() as i32;
    let (w, hh) = ((gx1 - gx0).max(0) as u32, (gy1 - gy0).max(0) as u32);
    let mut alpha = vec![0u8; (w * hh) as usize];
    let r2 = r * r;
    let n = (ss * ss) as f32;
    for py in 0..hh {
        for px in 0..w {
            let cx = (gx0 + px as i32) as f32;
            let cy = (gy0 + py as i32) as f32;
            // Skip pixels far from every segment.
            let near = segs.iter().any(|s| {
                cx + 1.0 >= s.0.min(s.2) - r && cx <= s.0.max(s.2) + r && cy + 1.0 >= s.1.min(s.3) - r && cy <= s.1.max(s.3) + r
            });
            if !near {
                continue;
            }
            let mut hits = 0u32;
            for j in 0..ss {
                for i in 0..ss {
                    let x = cx + (i as f32 + 0.5) / ss as f32;
                    let y = cy + (j as f32 + 0.5) / ss as f32;
                    if segs.iter().any(|s| seg_dist2(x, y, *s) <= r2) {
                        hits += 1;
                    }
                }
            }
            alpha[(py * w + px) as usize] = ((hits as f32 * 255.0 * 1.15 / n).round()).min(255.0) as u8;
        }
    }
    Glyph { left: gx0, top: gy0, w, h: hh, alpha }
}

fn seg_dist2(px: f32, py: f32, s: (f32, f32, f32, f32)) -> f32 {
    let (ax, ay, bx, by) = s;
    let (dx, dy) = (bx - ax, by - ay);
    let l2 = dx * dx + dy * dy;
    let t = if l2 == 0.0 { 0.0 } else { (((px - ax) * dx + (py - ay) * dy) / l2).clamp(0.0, 1.0) };
    let (qx, qy) = (ax + t * dx - px, ay + t * dy - py);
    qx * qx + qy * qy
}

#[cfg(test)]
mod tests {
    use super::*;

    /// font_small from include/font_data.h, parsed at test time.
    fn header_small() -> Vec<Vec<u8>> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../include/font_data.h");
        let text = std::fs::read_to_string(path).expect("include/font_data.h");
        let start = text.find("font_small[").unwrap();
        let body = &text[start..];
        let body = &body[body.find('{').unwrap()..body.find("};").unwrap()];
        let mut glyphs = Vec::new();
        for line in body.lines().filter(|l| l.trim_start().starts_with("{{")) {
            let line = &line[..line.find("/*").unwrap_or(line.len())];
            let nums: Vec<u8> = line
                .split(|c: char| !c.is_ascii_digit())
                .filter(|s| !s.is_empty())
                .map(|s| s.parse().unwrap())
                .collect();
            assert_eq!(nums.len(), 128);
            glyphs.push(nums);
        }
        glyphs
    }

    #[test]
    fn matches_the_kernel_font() {
        // The kernel's 8x16 cells are our mono face at a cell height of 16.
        let glyphs = header_small();
        assert_eq!(glyphs.len(), 97);
        let mut total = 0f64;
        let mut count = 0f64;
        for (i, want) in glyphs.iter().enumerate().take(95) {
            let c = char::from(0x20 + i as u8);
            // Render exactly as mkfont.py does: 8x8 samples, an 8x16 cell.
            let g = rasterize_cell(c, 8, 16, 1.15);
            for (a, b) in g.iter().zip(want.iter()) {
                total += (*a as f64 - *b as f64).abs();
                count += 1.0;
            }
        }
        let mean = total / count;
        assert!(mean < 2.0, "mean difference {mean}");
    }

    /// mkfont.py's rasterize() on our outlines, for the comparison above.
    fn rasterize_cell(c: char, w: usize, h: usize, stroke: f32) -> Vec<u8> {
        let o = outline(c);
        let (wf, hf) = (w as f32, h as f32);
        let pad = wf * PAD;
        let sx = (wf - 2.0 * pad) / 4.0;
        let sy = hf / 10.0 * 0.93;
        let oy = hf * OY;
        let mut segs = Vec::new();
        for line in o.iter() {
            let mut pts: Vec<(f32, f32)> = line.iter().map(|&(x, y)| (pad + x * sx, oy + y * sy)).collect();
            if pts.len() == 1 {
                pts.push(pts[0]);
            }
            for p in pts.windows(2) {
                segs.push((p[0].0, p[0].1, p[1].0, p[1].1));
            }
        }
        let r2 = (stroke / 2.0) * (stroke / 2.0);
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let mut hits = 0;
                for j in 0..8 {
                    for i in 0..8 {
                        let px = x as f32 + (i as f32 + 0.5) / 8.0;
                        let py = y as f32 + (j as f32 + 0.5) / 8.0;
                        if segs.iter().any(|s| seg_dist2(px, py, *s) <= r2) {
                            hits += 1;
                        }
                    }
                }
                out.push(((hits as f32 * 255.0 * 1.15 / 64.0).round()).min(255.0) as u8);
            }
        }
        out
    }

    #[test]
    fn widths_are_sane() {
        let f = Face::new(16.0, false, false, false);
        assert!(f.advance('i') < f.advance('m'));
        assert!(f.width("hello") > 20.0 && f.width("hello") < 60.0);
        let m = Face::new(16.0, false, false, true);
        assert_eq!(m.advance('i'), m.advance('m'));
        assert!(Face::new(16.0, true, false, false).advance('a') > f.advance('a'));
    }

    #[test]
    fn every_char_rasterizes() {
        let mut cache = Cache::default();
        for c in [' ', 'a', 'é', 'Ž', '“', '—', '\u{4E2D}', '\u{FFFD}', 'ß'] {
            for face in [Face::new(16.0, false, false, false), Face::new(40.0, true, true, false), Face::new(4.0, false, false, true)] {
                let g = cache.glyph(c, face);
                assert_eq!(g.alpha.len(), (g.w * g.h) as usize);
            }
        }
        assert_eq!(normalize("a\u{00AD}b\u{200B}c\u{00A9}"), "abc(c)");
    }
}

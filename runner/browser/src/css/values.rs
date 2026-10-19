//! values.rs - CSS colours and lengths.

use super::tokenizer::{Cv, Tok};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
    pub const fn hex(v: u32) -> Color {
        Color { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a: 255 }
    }
    pub fn with_alpha(self, a: u8) -> Color {
        Color { a, ..self }
    }
    pub fn xrgb(self) -> u32 {
        (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }
    pub fn is_transparent(self) -> bool {
        self.a == 0
    }
    /// Relative luminance, 0..1 (for picking readable colours).
    pub fn luma(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
}

static NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xF0F8FF), ("antiquewhite", 0xFAEBD7), ("aqua", 0x00FFFF), ("aquamarine", 0x7FFFD4), ("azure", 0xF0FFFF),
    ("beige", 0xF5F5DC), ("bisque", 0xFFE4C4), ("black", 0x000000), ("blanchedalmond", 0xFFEBCD), ("blue", 0x0000FF),
    ("blueviolet", 0x8A2BE2), ("brown", 0xA52A2A), ("burlywood", 0xDEB887), ("cadetblue", 0x5F9EA0), ("chartreuse", 0x7FFF00),
    ("chocolate", 0xD2691E), ("coral", 0xFF7F50), ("cornflowerblue", 0x6495ED), ("cornsilk", 0xFFF8DC), ("crimson", 0xDC143C),
    ("cyan", 0x00FFFF), ("darkblue", 0x00008B), ("darkcyan", 0x008B8B), ("darkgoldenrod", 0xB8860B), ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400), ("darkgrey", 0xA9A9A9), ("darkkhaki", 0xBDB76B), ("darkmagenta", 0x8B008B), ("darkolivegreen", 0x556B2F),
    ("darkorange", 0xFF8C00), ("darkorchid", 0x9932CC), ("darkred", 0x8B0000), ("darksalmon", 0xE9967A), ("darkseagreen", 0x8FBC8F),
    ("darkslateblue", 0x483D8B), ("darkslategray", 0x2F4F4F), ("darkslategrey", 0x2F4F4F), ("darkturquoise", 0x00CED1),
    ("darkviolet", 0x9400D3), ("deeppink", 0xFF1493), ("deepskyblue", 0x00BFFF), ("dimgray", 0x696969), ("dimgrey", 0x696969),
    ("dodgerblue", 0x1E90FF), ("firebrick", 0xB22222), ("floralwhite", 0xFFFAF0), ("forestgreen", 0x228B22), ("fuchsia", 0xFF00FF),
    ("gainsboro", 0xDCDCDC), ("ghostwhite", 0xF8F8FF), ("gold", 0xFFD700), ("goldenrod", 0xDAA520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xADFF2F), ("grey", 0x808080), ("honeydew", 0xF0FFF0), ("hotpink", 0xFF69B4),
    ("indianred", 0xCD5C5C), ("indigo", 0x4B0082), ("ivory", 0xFFFFF0), ("khaki", 0xF0E68C), ("lavender", 0xE6E6FA),
    ("lavenderblush", 0xFFF0F5), ("lawngreen", 0x7CFC00), ("lemonchiffon", 0xFFFACD), ("lightblue", 0xADD8E6), ("lightcoral", 0xF08080),
    ("lightcyan", 0xE0FFFF), ("lightgoldenrodyellow", 0xFAFAD2), ("lightgray", 0xD3D3D3), ("lightgreen", 0x90EE90), ("lightgrey", 0xD3D3D3),
    ("lightpink", 0xFFB6C1), ("lightsalmon", 0xFFA07A), ("lightseagreen", 0x20B2AA), ("lightskyblue", 0x87CEFA),
    ("lightslategray", 0x778899), ("lightslategrey", 0x778899), ("lightsteelblue", 0xB0C4DE), ("lightyellow", 0xFFFFE0),
    ("lime", 0x00FF00), ("limegreen", 0x32CD32), ("linen", 0xFAF0E6), ("magenta", 0xFF00FF), ("maroon", 0x800000),
    ("mediumaquamarine", 0x66CDAA), ("mediumblue", 0x0000CD), ("mediumorchid", 0xBA55D3), ("mediumpurple", 0x9370DB),
    ("mediumseagreen", 0x3CB371), ("mediumslateblue", 0x7B68EE), ("mediumspringgreen", 0x00FA9A), ("mediumturquoise", 0x48D1CC),
    ("mediumvioletred", 0xC71585), ("midnightblue", 0x191970), ("mintcream", 0xF5FFFA), ("mistyrose", 0xFFE4E1), ("moccasin", 0xFFE4B5),
    ("navajowhite", 0xFFDEAD), ("navy", 0x000080), ("oldlace", 0xFDF5E6), ("olive", 0x808000), ("olivedrab", 0x6B8E23),
    ("orange", 0xFFA500), ("orangered", 0xFF4500), ("orchid", 0xDA70D6), ("palegoldenrod", 0xEEE8AA), ("palegreen", 0x98FB98),
    ("paleturquoise", 0xAFEEEE), ("palevioletred", 0xDB7093), ("papayawhip", 0xFFEFD5), ("peachpuff", 0xFFDAB9), ("peru", 0xCD853F),
    ("pink", 0xFFC0CB), ("plum", 0xDDA0DD), ("powderblue", 0xB0E0E6), ("purple", 0x800080), ("rebeccapurple", 0x663399),
    ("red", 0xFF0000), ("rosybrown", 0xBC8F8F), ("royalblue", 0x4169E1), ("saddlebrown", 0x8B4513), ("salmon", 0xFA8072),
    ("sandybrown", 0xF4A460), ("seagreen", 0x2E8B57), ("seashell", 0xFFF5EE), ("sienna", 0xA0522D), ("silver", 0xC0C0C0),
    ("skyblue", 0x87CEEB), ("slateblue", 0x6A5ACD), ("slategray", 0x708090), ("slategrey", 0x708090), ("snow", 0xFFFAFA),
    ("springgreen", 0x00FF7F), ("steelblue", 0x4682B4), ("tan", 0xD2B48C), ("teal", 0x008080), ("thistle", 0xD8BFD8),
    ("tomato", 0xFF6347), ("turquoise", 0x40E0D0), ("violet", 0xEE82EE), ("wheat", 0xF5DEB3), ("white", 0xFFFFFF),
    ("whitesmoke", 0xF5F5F5), ("yellow", 0xFFFF00), ("yellowgreen", 0x9ACD32),
    // CSS system colours, as a light theme draws them.
    ("canvas", 0xFFFFFF), ("canvastext", 0x000000), ("linktext", 0x1D4ED8), ("visitedtext", 0x1D4ED8), ("buttonface", 0xEFEFEF),
    ("buttontext", 0x000000), ("field", 0xFFFFFF), ("fieldtext", 0x000000), ("graytext", 0x6D6D6D), ("highlight", 0xB4D5FE),
    ("highlighttext", 0x000000), ("mark", 0xFFFF00), ("marktext", 0x000000), ("buttonborder", 0x767676), ("accentcolor", 0x6D3BE8),
];

pub fn named_color(name: &str) -> Option<Color> {
    let l = name.to_ascii_lowercase();
    if l == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    NAMED.iter().find(|(n, _)| *n == l).map(|&(_, v)| Color::hex(v))
}

fn hex_color(h: &str) -> Option<Color> {
    if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
    let dd = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(match h.len() {
        3 => Color { r: d(0)?, g: d(1)?, b: d(2)?, a: 255 },
        4 => Color { r: d(0)?, g: d(1)?, b: d(2)?, a: d(3)? },
        6 => Color { r: dd(0)?, g: dd(2)?, b: dd(4)?, a: 255 },
        8 => Color { r: dd(0)?, g: dd(2)?, b: dd(4)?, a: dd(6)? },
        _ => return None,
    })
}

/// Numbers in a colour function: numbers, percentages, angles, `none`.
fn fn_args(args: &[Cv]) -> Vec<(f32, bool)> {
    let mut out = Vec::new();
    for c in args {
        match c {
            Cv::Tok(Tok::Number(v)) => out.push((*v, false)),
            Cv::Tok(Tok::Percentage(v)) => out.push((*v, true)),
            Cv::Tok(Tok::Dimension(v, u)) => out.push((
                match u.as_str() {
                    "deg" => *v,
                    "rad" => v.to_degrees(),
                    "turn" => v * 360.0,
                    "grad" => v * 0.9,
                    _ => *v,
                },
                false,
            )),
            Cv::Tok(Tok::Ident(s)) if s.eq_ignore_ascii_case("none") => out.push((0.0, false)),
            _ => {}
        }
    }
    out
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn alpha(a: Option<&(f32, bool)>) -> u8 {
    match a {
        None => 255,
        Some(&(v, true)) => to_u8(v * 2.55),
        Some(&(v, false)) => to_u8(v * 255.0),
    }
}

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// Parses one colour value. `current` is what currentColor means.
pub fn parse_color(c: &Cv, current: Color) -> Option<Color> {
    match c {
        Cv::Tok(Tok::Ident(n)) if n.eq_ignore_ascii_case("currentcolor") => Some(current),
        Cv::Tok(Tok::Ident(n)) => named_color(n),
        Cv::Tok(Tok::Hash(h)) => hex_color(h),
        Cv::Func(f, args) => {
            let a = fn_args(args);
            match f.as_str() {
                "rgb" | "rgba" if a.len() >= 3 => {
                    let ch = |(v, pct): (f32, bool)| if pct { to_u8(v * 2.55) } else { to_u8(v) };
                    Some(Color { r: ch(a[0]), g: ch(a[1]), b: ch(a[2]), a: alpha(a.get(3)) })
                }
                "hsl" | "hsla" if a.len() >= 3 => {
                    let (r, g, b) = hsl(a[0].0, a[1].0 / 100.0, a[2].0 / 100.0);
                    Some(Color { r: to_u8(r * 255.0), g: to_u8(g * 255.0), b: to_u8(b * 255.0), a: alpha(a.get(3)) })
                }
                "hwb" if a.len() >= 3 => {
                    let (w, bl) = (a[1].0 / 100.0, a[2].0 / 100.0);
                    let (r, g, b) = hsl(a[0].0, 1.0, 0.5);
                    let k = |v: f32| to_u8((v * (1.0 - w - bl) + w) * 255.0);
                    Some(Color { r: k(r), g: k(g), b: k(b), a: alpha(a.get(3)) })
                }
                // oklch()/lab()/color-mix(): too far for now; a neutral stand-in.
                "oklch" | "oklab" | "lab" | "lch" | "color" => {
                    let l = a.first().map_or(0.5, |&(v, pct)| if pct { v / 100.0 } else if v > 1.0 { v / 100.0 } else { v });
                    let g = to_u8(l.clamp(0.0, 1.0) * 255.0);
                    Some(Color { r: g, g, b: g, a: alpha(a.get(3)) })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A length before layout: px plus a percentage of the containing block,
/// `auto`, or `none`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Auto,
    None,
    /// px + pct% of the reference size
    Calc(f32, f32),
}

impl Len {
    pub const ZERO: Len = Len::Calc(0.0, 0.0);
    pub fn px(v: f32) -> Len {
        Len::Calc(v, 0.0)
    }
    pub fn pct(v: f32) -> Len {
        Len::Calc(0.0, v)
    }
    /// The used value against `base`; None for auto/none, or a percentage
    /// of an unknown base.
    pub fn resolve(self, base: Option<f32>) -> Option<f32> {
        match self {
            Len::Calc(px, pct) if pct == 0.0 => Some(px),
            Len::Calc(px, pct) => base.map(|b| px + b * pct / 100.0),
            _ => None,
        }
    }
    pub fn or_zero(self, base: Option<f32>) -> f32 {
        self.resolve(base).unwrap_or(0.0)
    }
    pub fn is_auto(self) -> bool {
        self == Len::Auto
    }
    pub fn has_pct(self) -> bool {
        matches!(self, Len::Calc(_, p) if p != 0.0)
    }
}

/// What relative units are relative to.
#[derive(Clone, Copy, Debug)]
pub struct Units {
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
}

pub fn unit_px(v: f32, unit: &str, u: &Units) -> Option<f32> {
    Some(match unit {
        "px" => v,
        "em" => v * u.em,
        "rem" => v * u.rem,
        "ex" => v * u.em * 0.5,
        "ch" => v * u.em * 0.6,
        "cap" => v * u.em * 0.7,
        "lh" | "rlh" => v * u.em * 1.2,
        "pt" => v * 96.0 / 72.0,
        "pc" => v * 16.0,
        "in" => v * 96.0,
        "cm" => v * 96.0 / 2.54,
        "mm" => v * 96.0 / 25.4,
        "q" => v * 96.0 / 101.6,
        "vw" | "svw" | "lvw" | "dvw" => v * u.vw / 100.0,
        "vh" | "svh" | "lvh" | "dvh" => v * u.vh / 100.0,
        "vmin" => v * u.vw.min(u.vh) / 100.0,
        "vmax" => v * u.vw.max(u.vh) / 100.0,
        _ => return None,
    })
}

/// Parses a length or percentage (with calc/min/max/clamp).
pub fn parse_len(c: &Cv, u: &Units) -> Option<Len> {
    match c {
        Cv::Tok(Tok::Ident(s)) if s.eq_ignore_ascii_case("auto") => Some(Len::Auto),
        Cv::Tok(Tok::Ident(s)) if s.eq_ignore_ascii_case("none") => Some(Len::None),
        Cv::Tok(Tok::Ident(s))
            if ["fit-content", "max-content", "min-content", "-webkit-fill-available", "stretch", "-moz-available", "-webkit-fit-content"]
                .contains(&s.to_ascii_lowercase().as_str()) =>
        {
            Some(Len::Auto)
        }
        Cv::Tok(Tok::Number(v)) if *v == 0.0 => Some(Len::ZERO),
        Cv::Tok(Tok::Dimension(v, unit)) => unit_px(*v, unit, u).map(Len::px),
        Cv::Tok(Tok::Percentage(p)) => Some(Len::pct(*p)),
        Cv::Func(f, args) => match f.as_str() {
            "calc" | "-webkit-calc" | "-moz-calc" => calc(args, u, 0).map(|(px, pct)| Len::Calc(px, pct)),
            "min" | "max" | "clamp" => {
                let vals: Vec<(f32, f32)> = args.split(|c| matches!(c, Cv::Tok(Tok::Comma))).filter_map(|a| calc(a, u, 0)).collect();
                if vals.is_empty() {
                    return None;
                }
                // Mixed percentages can't be compared yet: take the first
                // argument (for clamp: the preferred value).
                if vals.iter().any(|v| v.1 != 0.0) {
                    let v = if f == "clamp" { vals.get(1).copied().unwrap_or(vals[0]) } else { vals[0] };
                    return Some(Len::Calc(v.0, v.1));
                }
                let px: Vec<f32> = vals.iter().map(|v| v.0).collect();
                let v = match f.as_str() {
                    "min" => px.iter().copied().fold(f32::MAX, f32::min),
                    "max" => px.iter().copied().fold(f32::MIN, f32::max),
                    _ if px.len() == 3 => px[1].clamp(px[0], px[2].max(px[0])),
                    _ => px[0],
                };
                Some(Len::px(v))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Evaluates a calc() expression to (px, pct): + - * / and parentheses.
fn calc(args: &[Cv], u: &Units, depth: usize) -> Option<(f32, f32)> {
    if depth > 8 {
        return None;
    }
    // Terms separated by + / - (whitespace-delimited, as CSS requires).
    let items: Vec<&Cv> = args.iter().filter(|c| !c.is_ws()).collect();
    let mut total = (0.0f32, 0.0f32);
    let mut sign = 1.0f32;
    let mut term: Option<(f32, f32, bool)> = None; // (px, pct, is_number)
    let mut op: Option<char> = None;
    let flush = |total: &mut (f32, f32), term: Option<(f32, f32, bool)>, sign: f32| {
        if let Some((px, pct, _)) = term {
            total.0 += sign * px;
            total.1 += sign * pct;
        }
    };
    for c in items {
        let val: Option<(f32, f32, bool)> = match c {
            Cv::Tok(Tok::Number(v)) => Some((*v, 0.0, true)),
            Cv::Tok(Tok::Dimension(v, unit)) => Some((unit_px(*v, unit, u)?, 0.0, false)),
            Cv::Tok(Tok::Percentage(p)) => Some((0.0, *p, false)),
            Cv::Block('(', inner) => calc(inner, u, depth + 1).map(|(a, b)| (a, b, false)),
            Cv::Func(f, inner) if f == "calc" => calc(inner, u, depth + 1).map(|(a, b)| (a, b, false)),
            Cv::Func(..) => match parse_len(c, u)? {
                Len::Calc(a, b) => Some((a, b, false)),
                _ => return None,
            },
            Cv::Tok(Tok::Delim(d @ ('+' | '-'))) if term.is_some() && op.is_none() => {
                flush(&mut total, term.take(), sign);
                sign = if *d == '-' { -1.0 } else { 1.0 };
                continue;
            }
            Cv::Tok(Tok::Delim(d @ ('*' | '/'))) => {
                op = Some(*d);
                continue;
            }
            _ => return None,
        };
        let v = val?;
        term = match (term, op.take()) {
            (None, _) => Some(v),
            (Some(t), Some('*')) => {
                if t.2 {
                    Some((v.0 * t.0, v.1 * t.0, v.2))
                } else if v.2 {
                    Some((t.0 * v.0, t.1 * v.0, false))
                } else {
                    return None;
                }
            }
            (Some(t), Some('/')) => {
                if !v.2 || v.0 == 0.0 {
                    return None;
                }
                Some((t.0 / v.0, t.1 / v.0, t.2))
            }
            _ => return None,
        };
    }
    flush(&mut total, term, sign);
    if total.0.is_finite() && total.1.is_finite() {
        Some((total.0.clamp(-1e6, 1e6), total.1.clamp(-1e5, 1e5)))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::tokenizer::{component_values, tokenize};
    use super::*;

    fn cv(s: &str) -> Cv {
        component_values(tokenize(s)).into_iter().next().unwrap()
    }
    const U: Units = Units { em: 10.0, rem: 16.0, vw: 1000.0, vh: 500.0 };

    #[test]
    fn colors() {
        assert_eq!(parse_color(&cv("#f80"), Color::BLACK), Some(Color::rgb(255, 136, 0)));
        assert_eq!(parse_color(&cv("#11223344"), Color::BLACK), Some(Color { r: 0x11, g: 0x22, b: 0x33, a: 0x44 }));
        assert_eq!(parse_color(&cv("rgb(10, 20, 30)"), Color::BLACK), Some(Color::rgb(10, 20, 30)));
        assert_eq!(parse_color(&cv("rgb(10 20 30 / 50%)"), Color::BLACK).unwrap().a, 128);
        assert_eq!(parse_color(&cv("rgba(0,0,0,.5)"), Color::BLACK).unwrap().a, 128);
        assert_eq!(parse_color(&cv("hsl(120, 100%, 25%)"), Color::BLACK), Some(Color::rgb(0, 128, 0)));
        assert_eq!(parse_color(&cv("RebeccaPurple"), Color::BLACK), Some(Color::hex(0x663399)));
        assert_eq!(parse_color(&cv("currentColor"), Color::WHITE), Some(Color::WHITE));
        assert_eq!(parse_color(&cv("#ggg"), Color::BLACK), None);
        assert_eq!(parse_color(&cv("#12345"), Color::BLACK), None);
    }

    #[test]
    fn lengths() {
        assert_eq!(parse_len(&cv("2em"), &U), Some(Len::px(20.0)));
        assert_eq!(parse_len(&cv("1rem"), &U), Some(Len::px(16.0)));
        assert_eq!(parse_len(&cv("12pt"), &U), Some(Len::px(16.0)));
        assert_eq!(parse_len(&cv("50%"), &U), Some(Len::pct(50.0)));
        assert_eq!(parse_len(&cv("10vw"), &U), Some(Len::px(100.0)));
        assert_eq!(parse_len(&cv("calc(100% - 2 * 10px)"), &U), Some(Len::Calc(-20.0, 100.0)));
        assert_eq!(parse_len(&cv("calc((1em + 2px) * 2)"), &U), Some(Len::px(24.0)));
        assert_eq!(parse_len(&cv("min(100px, 2em)"), &U), Some(Len::px(20.0)));
        assert_eq!(parse_len(&cv("clamp(10px, 50px, 30px)"), &U), Some(Len::px(30.0)));
        assert_eq!(parse_len(&cv("calc(1px / 0)"), &U), None);
        assert_eq!(parse_len(&cv("5"), &U), None);
        assert_eq!(Len::Calc(-20.0, 100.0).resolve(Some(200.0)), Some(180.0));
    }
}

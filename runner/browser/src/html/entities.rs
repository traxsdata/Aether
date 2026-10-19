//! entities.rs - named character references: every Latin-1 entity (which
//! HTML also accepts without the ';'), plus the Greek, punctuation, arrow,
//! math and symbol names pages commonly use.

/// (name, text, legacy: also recognised without a trailing ';')
pub static ENTITIES: &[(&str, &str, bool)] = &[
    ("AElig", "\u{C6}", true), ("AMP", "&", true), ("Aacute", "\u{C1}", true), ("Acirc", "\u{C2}", true),
    ("Agrave", "\u{C0}", true), ("Alpha", "\u{391}", false), ("Aring", "\u{C5}", true), ("Atilde", "\u{C3}", true),
    ("Auml", "\u{C4}", true), ("Beta", "\u{392}", false), ("COPY", "\u{A9}", true), ("Ccedil", "\u{C7}", true),
    ("Chi", "\u{3A7}", false), ("Dagger", "\u{2021}", false), ("Delta", "\u{394}", false), ("ETH", "\u{D0}", true),
    ("Eacute", "\u{C9}", true), ("Ecirc", "\u{CA}", true), ("Egrave", "\u{C8}", true), ("Epsilon", "\u{395}", false),
    ("Eta", "\u{397}", false), ("Euml", "\u{CB}", true), ("GT", ">", true), ("Gamma", "\u{393}", false),
    ("Iacute", "\u{CD}", true), ("Icirc", "\u{CE}", true), ("Igrave", "\u{CC}", true), ("Iota", "\u{399}", false),
    ("Iuml", "\u{CF}", true), ("Kappa", "\u{39A}", false), ("LT", "<", true), ("Lambda", "\u{39B}", false),
    ("Mu", "\u{39C}", false), ("Ntilde", "\u{D1}", true), ("Nu", "\u{39D}", false), ("OElig", "\u{152}", false),
    ("Oacute", "\u{D3}", true), ("Ocirc", "\u{D4}", true), ("Ograve", "\u{D2}", true), ("Omega", "\u{3A9}", false),
    ("Omicron", "\u{39F}", false), ("Oslash", "\u{D8}", true), ("Otilde", "\u{D5}", true), ("Ouml", "\u{D6}", true),
    ("Phi", "\u{3A6}", false), ("Pi", "\u{3A0}", false), ("Prime", "\u{2033}", false), ("Psi", "\u{3A8}", false),
    ("QUOT", "\"", true), ("REG", "\u{AE}", true), ("Rho", "\u{3A1}", false), ("Scaron", "\u{160}", false),
    ("Sigma", "\u{3A3}", false), ("THORN", "\u{DE}", true), ("Tau", "\u{3A4}", false), ("Theta", "\u{398}", false),
    ("Uacute", "\u{DA}", true), ("Ucirc", "\u{DB}", true), ("Ugrave", "\u{D9}", true), ("Upsilon", "\u{3A5}", false),
    ("Uuml", "\u{DC}", true), ("Xi", "\u{39E}", false), ("Yacute", "\u{DD}", true), ("Yuml", "\u{178}", false),
    ("Zeta", "\u{396}", false), ("aacute", "\u{E1}", true), ("acirc", "\u{E2}", true), ("acute", "\u{B4}", true),
    ("aelig", "\u{E6}", true), ("agrave", "\u{E0}", true), ("alefsym", "\u{2135}", false), ("alpha", "\u{3B1}", false),
    ("amp", "&", true), ("and", "\u{2227}", false), ("ang", "\u{2220}", false), ("apos", "'", false),
    ("aring", "\u{E5}", true), ("asymp", "\u{2248}", false), ("atilde", "\u{E3}", true), ("auml", "\u{E4}", true),
    ("bdquo", "\u{201E}", false), ("beta", "\u{3B2}", false), ("brvbar", "\u{A6}", true), ("bull", "\u{2022}", false),
    ("cap", "\u{2229}", false), ("ccedil", "\u{E7}", true), ("cedil", "\u{B8}", true), ("cent", "\u{A2}", true),
    ("check", "\u{2713}", false), ("chi", "\u{3C7}", false), ("circ", "\u{2C6}", false), ("clubs", "\u{2663}", false),
    ("colon", ":", false), ("comma", ",", false), ("cong", "\u{2245}", false), ("copy", "\u{A9}", true),
    ("crarr", "\u{21B5}", false), ("cup", "\u{222A}", false), ("curren", "\u{A4}", true), ("dArr", "\u{21D3}", false),
    ("dagger", "\u{2020}", false), ("darr", "\u{2193}", false), ("deg", "\u{B0}", true), ("delta", "\u{3B4}", false),
    ("diams", "\u{2666}", false), ("divide", "\u{F7}", true), ("dollar", "$", false), ("eacute", "\u{E9}", true),
    ("ecirc", "\u{EA}", true), ("egrave", "\u{E8}", true), ("empty", "\u{2205}", false), ("emsp", "\u{2003}", false),
    ("ensp", "\u{2002}", false), ("epsilon", "\u{3B5}", false), ("equals", "=", false), ("equiv", "\u{2261}", false),
    ("eta", "\u{3B7}", false), ("eth", "\u{F0}", true), ("euml", "\u{EB}", true), ("euro", "\u{20AC}", false),
    ("excl", "!", false), ("exist", "\u{2203}", false), ("fnof", "\u{192}", false), ("forall", "\u{2200}", false),
    ("frac12", "\u{BD}", true), ("frac14", "\u{BC}", true), ("frac34", "\u{BE}", true), ("frasl", "\u{2044}", false),
    ("gamma", "\u{3B3}", false), ("ge", "\u{2265}", false), ("gt", ">", true), ("hArr", "\u{21D4}", false),
    ("harr", "\u{2194}", false), ("hearts", "\u{2665}", false), ("hellip", "\u{2026}", false), ("hyphen", "\u{2010}", false),
    ("iacute", "\u{ED}", true), ("icirc", "\u{EE}", true), ("iexcl", "\u{A1}", true), ("igrave", "\u{EC}", true),
    ("image", "\u{2111}", false), ("infin", "\u{221E}", false), ("int", "\u{222B}", false), ("iota", "\u{3B9}", false),
    ("iquest", "\u{BF}", true), ("isin", "\u{2208}", false), ("iuml", "\u{EF}", true), ("kappa", "\u{3BA}", false),
    ("lArr", "\u{21D0}", false), ("lambda", "\u{3BB}", false), ("lang", "\u{27E8}", false), ("laquo", "\u{AB}", true),
    ("larr", "\u{2190}", false), ("lceil", "\u{2308}", false), ("lcub", "{", false), ("ldquo", "\u{201C}", false),
    ("le", "\u{2264}", false), ("lfloor", "\u{230A}", false), ("lowast", "\u{2217}", false), ("loz", "\u{25CA}", false),
    ("lpar", "(", false), ("lrm", "\u{200E}", false), ("lsaquo", "\u{2039}", false), ("lsqb", "[", false),
    ("lsquo", "\u{2018}", false), ("lt", "<", true), ("macr", "\u{AF}", true), ("mdash", "\u{2014}", false),
    ("micro", "\u{B5}", true), ("middot", "\u{B7}", true), ("minus", "\u{2212}", false), ("mu", "\u{3BC}", false),
    ("nabla", "\u{2207}", false), ("nbsp", "\u{A0}", true), ("ndash", "\u{2013}", false), ("ne", "\u{2260}", false),
    ("ni", "\u{220B}", false), ("not", "\u{AC}", true), ("notin", "\u{2209}", false), ("nsub", "\u{2284}", false),
    ("ntilde", "\u{F1}", true), ("nu", "\u{3BD}", false), ("num", "#", false), ("oacute", "\u{F3}", true),
    ("ocirc", "\u{F4}", true), ("oelig", "\u{153}", false), ("ograve", "\u{F2}", true), ("oline", "\u{203E}", false),
    ("omega", "\u{3C9}", false), ("omicron", "\u{3BF}", false), ("oplus", "\u{2295}", false), ("or", "\u{2228}", false),
    ("ordf", "\u{AA}", true), ("ordm", "\u{BA}", true), ("oslash", "\u{F8}", true), ("otilde", "\u{F5}", true),
    ("otimes", "\u{2297}", false), ("ouml", "\u{F6}", true), ("para", "\u{B6}", true), ("part", "\u{2202}", false),
    ("percnt", "%", false), ("period", ".", false), ("permil", "\u{2030}", false), ("perp", "\u{22A5}", false),
    ("phi", "\u{3C6}", false), ("pi", "\u{3C0}", false), ("piv", "\u{3D6}", false), ("plus", "+", false),
    ("plusmn", "\u{B1}", true), ("pound", "\u{A3}", true), ("prime", "\u{2032}", false), ("prod", "\u{220F}", false),
    ("prop", "\u{221D}", false), ("psi", "\u{3C8}", false), ("quest", "?", false), ("quot", "\"", true),
    ("rArr", "\u{21D2}", false), ("radic", "\u{221A}", false), ("rang", "\u{27E9}", false), ("raquo", "\u{BB}", true),
    ("rarr", "\u{2192}", false), ("rceil", "\u{2309}", false), ("rcub", "}", false), ("rdquo", "\u{201D}", false),
    ("real", "\u{211C}", false), ("reg", "\u{AE}", true), ("rfloor", "\u{230B}", false), ("rho", "\u{3C1}", false),
    ("rlm", "\u{200F}", false), ("rpar", ")", false), ("rsaquo", "\u{203A}", false), ("rsqb", "]", false),
    ("rsquo", "\u{2019}", false), ("sbquo", "\u{201A}", false), ("scaron", "\u{161}", false), ("sdot", "\u{22C5}", false),
    ("sect", "\u{A7}", true), ("semi", ";", false), ("shy", "\u{AD}", true), ("sigma", "\u{3C3}", false),
    ("sigmaf", "\u{3C2}", false), ("sim", "\u{223C}", false), ("sol", "/", false), ("spades", "\u{2660}", false),
    ("star", "\u{2606}", false), ("starf", "\u{2605}", false), ("sub", "\u{2282}", false), ("sube", "\u{2286}", false),
    ("sum", "\u{2211}", false), ("sup", "\u{2283}", false), ("sup1", "\u{B9}", true), ("sup2", "\u{B2}", true),
    ("sup3", "\u{B3}", true), ("supe", "\u{2287}", false), ("szlig", "\u{DF}", true), ("tau", "\u{3C4}", false),
    ("there4", "\u{2234}", false), ("theta", "\u{3B8}", false), ("thetasym", "\u{3D1}", false), ("thinsp", "\u{2009}", false),
    ("thorn", "\u{FE}", true), ("tilde", "\u{2DC}", false), ("times", "\u{D7}", true), ("trade", "\u{2122}", false),
    ("uArr", "\u{21D1}", false), ("uacute", "\u{FA}", true), ("uarr", "\u{2191}", false), ("ucirc", "\u{FB}", true),
    ("ugrave", "\u{F9}", true), ("uml", "\u{A8}", true), ("upsih", "\u{3D2}", false), ("upsilon", "\u{3C5}", false),
    ("uuml", "\u{FC}", true), ("verbar", "|", false), ("weierp", "\u{2118}", false), ("xi", "\u{3BE}", false),
    ("yacute", "\u{FD}", true), ("yen", "\u{A5}", true), ("yuml", "\u{FF}", true), ("zeta", "\u{3B6}", false),
    ("zwj", "\u{200D}", false), ("zwnj", "\u{200C}", false),
];

pub fn lookup(name: &str) -> Option<&'static str> {
    ENTITIES.binary_search_by(|e| e.0.cmp(name)).ok().map(|i| ENTITIES[i].1)
}

/// The longest legacy entity name (usable without ';') that `s` starts with.
pub fn longest_legacy_prefix(s: &str) -> Option<(&'static str, &'static str)> {
    let mut best: Option<(&'static str, &'static str)> = None;
    for &(name, text, legacy) in ENTITIES {
        if legacy && s.starts_with(name) && best.is_none_or(|b| name.len() > b.0.len()) {
            best = Some((name, text));
        }
    }
    best
}

/// Windows-1252 meanings of numeric references 0x80..0x9F (HTML's rule).
pub fn c1_replacement(n: u32) -> Option<char> {
    const T: [u32; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0,
        0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    if (0x80..=0x9F).contains(&n) && T[(n - 0x80) as usize] != 0 {
        char::from_u32(T[(n - 0x80) as usize])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_is_sorted() {
        for w in super::ENTITIES.windows(2) {
            assert!(w[0].0 < w[1].0, "{} >= {}", w[0].0, w[1].0);
        }
        assert_eq!(super::lookup("hellip"), Some("\u{2026}"));
        assert_eq!(super::longest_legacy_prefix("notit"), Some(("not", "\u{AC}")));
    }
}

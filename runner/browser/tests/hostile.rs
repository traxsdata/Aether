//! Hostile input, end to end: every document goes through parsing, the
//! cascade, layout and painting. Nothing may panic, hang or blow the stack,
//! whatever the input. Includes a small deterministic fuzzer.

use std::time::{Duration, Instant};

use browser::font::Cache;
use browser::page;
use browser::paint::{paint_page, Canvas};

/// Parses, styles, lays out and paints `html`; returns the page height.
fn render(html: &str) -> f32 {
    let l = page::load_builtin("t", html);
    let pg = page::lay_out(&l, 800.0, 600.0);
    assert!(pg.height.is_finite() && pg.height >= 0.0, "height {}", pg.height);
    let mut px = vec![0u32; 800 * 300];
    let mut cache = Cache::default();
    let form = page::initial_form(&l.doc);
    {
        let mut cv = Canvas::new(&mut px, 800, 300, 800);
        let images = std::collections::HashMap::new();
        paint_page(&mut cv, &mut cache, &pg, &images, 0, 0, 0.0, &form);
        paint_page(&mut cv, &mut cache, &pg, &images, 0, 0, (pg.height - 150.0).max(0.0), &form);
    }
    pg.height
}

fn big<F: FnOnce() + Send + 'static>(f: F) {
    // The browser runs pages on 32 MiB stacks; tests use the same.
    std::thread::Builder::new().stack_size(32 << 20).spawn(f).unwrap().join().unwrap();
}

fn timed(name: &str, limit_s: u64, html: String) {
    let t = Instant::now();
    render(&html);
    let e = t.elapsed();
    assert!(e < Duration::from_secs(limit_s), "{name} took {e:?}");
}

#[test]
fn deep_nesting() {
    big(|| {
        timed("divs", 20, "<div>".repeat(100_000) + "x");
        timed("spans", 20, "<span>".repeat(50_000) + "x");
        timed("formatting", 20, "<b><i><u><a href=x>".repeat(20_000) + "x");
        timed("lists", 20, "<ul><li>".repeat(20_000) + "x");
        timed("tables", 20, "<table><tr><td>".repeat(5_000) + "x");
        timed("svg", 20, "<svg><g>".repeat(20_000) + "x");
        timed("blockquote+float", 20, "<blockquote style='float:left'>".repeat(10_000) + "x");
        timed("flex", 20, "<div style='display:flex'>".repeat(5_000) + "x");
        timed("grid", 20, "<div style='display:grid;grid-template-columns:1fr 1fr'>".repeat(5_000) + "x");
        timed("inline-block", 20, "<span style='display:inline-block;padding:1px'>".repeat(5_000) + "x");
        timed("abs", 20, "<div style='position:absolute;bottom:0;right:0'>".repeat(5_000) + "x");
        timed("misnested", 20, "<p><b><table><td><i></p></b>".repeat(5_000));
    });
}

#[test]
fn huge_things() {
    big(|| {
        timed("long word", 30, "a".repeat(300_000));
        timed("many words", 30, "word ".repeat(200_000));
        timed("pre", 30, format!("<pre>{}</pre>", "line\n".repeat(50_000)));
        timed("attrs", 10, format!("<div {}>x</div>", (0..5000).map(|i| format!("a{i}=\"{}\"", "v".repeat(100))).collect::<String>()));
        timed("attr value", 10, format!("<a href=\"{}\">x</a>", "h".repeat(2_000_000)));
        timed("comment", 10, format!("<!--{}", "-".repeat(1_000_000)));
        timed("spans", 30, "<td colspan=99999 rowspan=99999>x".repeat(200));
        timed("big table", 30, format!("<table>{}</table>", "<tr><td>a<td>b<td>c<td>d".repeat(5000)));
        timed("sizes", 10, "<div style='width:1e30px;height:1e30px;margin:-1e30px;padding:1e30%;font-size:1e30px;line-height:1e30'>x</div><img width=99999999 height=99999999><hr size=99999999>".into());
        timed("negative", 10, "<div style='width:-5px;height:-5px;padding:-5px;font-size:-5px;border:-5px solid'>x</div>".into());
        timed("entities", 10, "&amp;&#0;&#xD800;&#99999999999;&#x110000;&;&&&&&#;&#x;&notit;&notin".repeat(10_000));
        timed("ol start", 10, "<ol start=-2147483648 reversed><li value=99999999999999>a<li>b</ol>".into());
    });
}

#[test]
fn hostile_css() {
    big(|| {
        timed("blocks", 20, format!("<style>{}</style>x", "{[(".repeat(50_000)));
        timed("selectors", 20, format!("<style>{} {{color:red}}</style><div>x</div>", "div ".repeat(20_000)));
        timed("not", 20, format!("<style>{}a{} {{color:red}}</style><a>x</a>", ":not(".repeat(5000), ")".repeat(5000)));
        timed("rules", 30, format!("<style>{}</style>{}", ".a{color:red}".repeat(100_000), "<p class=a>x".repeat(500)));
        timed("calc", 20, format!("<div style='width:{}1px{}'>x</div>", "calc(".repeat(5000), ")".repeat(5000)));
        timed("vars", 20, "<style>:root{--a:var(--b) var(--b) var(--b);--b:var(--c) var(--c) var(--c);--c:var(--d) var(--d) var(--d);--d:var(--e) var(--e);--e:1px 1px 1px 1px} p{margin:var(--a);--x:var(--x)}</style><p>x".into());
        timed("media", 20, format!("<style>@media {} {{p{{color:red}}}}</style><p>x", "(".repeat(10_000)));
        timed("strings", 20, format!("<style>p{{content:\"{}</style><p>x", "\\".repeat(100_000)));
        timed("descendant heavy", 30, format!("<style>{}</style>{}", "div div div div div div div div p{color:red}".repeat(200), "<div>".repeat(300) + &"<p>x".repeat(300)));
        timed("nth", 20, format!("<style>li:nth-child(2n+1):nth-last-child(3n-1):nth-of-type(odd){{color:red}}</style><ul>{}</ul>", "<li>x".repeat(5000)));
        timed("has", 20, format!("<style>div:has(div:has(div:has(p))){{color:red}}</style>{}", "<div>".repeat(2000)));
    });
}

/// A tiny deterministic random number generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[(self.next() % xs.len() as u64) as usize]
    }
}

const PIECES: &[&str] = &[
    "<div>", "</div>", "<p>", "</p>", "<span>", "</span>", "<b>", "</b>", "<i>", "<a href='#x'>", "</a>", "<table>", "</table>", "<tr>", "<td>", "<th colspan=3>",
    "</td>", "<ul>", "<ol start=5>", "<li>", "</li>", "<dl>", "<dt>", "<dd>", "<pre>", "</pre>", "<br>", "<hr>", "<img alt='pic' width=50>", "<input>", "<select><option>o",
    "<textarea>", "</textarea>", "<button>", "<form>", "<h1>", "</h1>", "<script>", "</script>", "<style>", "</style>", "<noscript>", "<svg>", "<math>", "</svg>",
    "<template>", "<frameset>", "<!--", "-->", "<!doctype html>", "<![CDATA[", "]]>", "&amp;", "&#x41;", "&", "<", ">", "\"", "'", "=", " ", "\n", "\t", "\0",
    "text", "more words here", "é", "\u{202E}", "\u{FFFD}", "日本", "<p style='", "color:red;", "display:flex;", "display:grid;", "display:table-cell;", "float:left;",
    "position:absolute;", "position:relative;top:-5px;", "width:50%;", "margin:auto;", "padding:1em;", "border:3px dashed;", "white-space:pre;", "font-size:3rem;",
    "content:'x';", "'>", "<div style='display:inline-block'>", "<div style='display:contents'>", "{", "}", "p{color:red}", "a:hover{", "@media(max-width:1px){",
    "*{margin:0}", ".x>.y+.z~.w{}", "@import 'x.css';", "::before{content:'b'}", "var(--x)", "--x:var(--x);", "calc(1px+", "<caption>", "<col span=9>", "<colgroup>",
    "<tbody>", "<thead>", "<tfoot>", "<marquee>", "<font size=7>", "<center>", "<details open><summary>s", "<base href='https://x/'>", "<title>", "</title>",
];

#[test]
fn fuzz_documents() {
    big(|| {
        let start = Instant::now();
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for i in 0..400 {
            let n = 20 + (rng.next() % 400) as usize;
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(rng.pick(PIECES));
            }
            let h = render(&s);
            assert!(h < 1e7, "case {i}: height {h}");
        }
        assert!(start.elapsed() < Duration::from_secs(120));
    });
}

#[test]
fn fuzz_bytes() {
    big(|| {
        let mut rng = Rng(12345);
        for _ in 0..300 {
            let n = (rng.next() % 3000) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| (rng.next() & 0xFF) as u8).collect();
            let (text, _) = browser::html::decode(&bytes, None);
            render(&text);
            // Bytes that look like markup.
            let marked: Vec<u8> = bytes.iter().map(|b| b"<>/=\"' &#;abcdpliv-!"[(*b % 20) as usize]).collect();
            render(&String::from_utf8_lossy(&marked));
        }
    });
}

#[test]
fn fuzz_stylesheets() {
    big(|| {
        let mut rng = Rng(777);
        let css = &PIECES[60..];
        for _ in 0..300 {
            let mut s = String::from("<style>");
            for _ in 0..(10 + rng.next() % 200) {
                s.push_str(rng.pick(css));
            }
            s.push_str("</style><div class=x><p class=y>a<span class=z>b</span></p><ul><li>c</ul></div>");
            render(&s);
        }
    });
}

//! pages.rs - the browser's built-in pages: about:home, about:security,
//! the plain-http warning and error pages. They are ordinary HTML and CSS,
//! drawn by the same engine as every other page.

/// Shared look of the built-in pages: æther's dark glass.
const STYLE: &str = r#"
html { background: #0b1022; color: #e6ecff; font-family: sans-serif; }
body { margin: 0; line-height: 1.55; }
a { color: #38e1f5; text-decoration: none; }
.wrap { max-width: 760px; margin: 0 auto; padding: 48px 32px 64px; }
h1 { font-size: 34px; margin: 0 0 6px; color: #ffffff; letter-spacing: 0.5px; }
h2 { font-size: 20px; margin: 34px 0 10px; color: #ffffff; }
p { margin: 0 0 12px; }
.lead { font-size: 17px; color: #b8c2e6; }
.dim { color: #8c96b8; }
.glass { background: #141b36; border: 1px solid #2a3560; border-radius: 14px; padding: 18px 22px; margin: 14px 0; }
.mark { display: inline-block; width: 38px; height: 38px; border-radius: 10px; background: linear-gradient(135deg, #38e1f5, #b06cff); }
code { font-family: monospace; color: #c8b5ff; }
"#;

pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// A built-in page by name (after "about:"), or None.
pub fn about(name: &str) -> Option<String> {
    match name {
        "home" | "blank" | "" => Some(home()),
        "security" => Some(security()),
        _ => None,
    }
}

pub fn home() -> String {
    format!(
        r#"<!doctype html><html><head><title>New Tab</title><style>{STYLE}
.hero {{ text-align: center; padding: 42px 24px 8px; }}
.hero h1 {{ font-size: 46px; margin-top: 14px; letter-spacing: 1px; }}
.hero .mark {{ width: 72px; height: 72px; border-radius: 20px; }}
.hero .lead {{ font-size: 18px; }}
.omnibox {{ max-width: 560px; margin: 24px auto 0; padding: 12px 20px; border-radius: 999px; border: 1px solid #2a3560; background: #0e1428; color: #8c96b8; text-align: left; }}
.omnibox b {{ color: #e6ecff; font-weight: normal; }}
.tiles {{ display: flex; gap: 16px; margin: 30px auto 0; max-width: 760px; }}
.tile {{ flex: 1; display: block; background: #141b36; border: 1px solid #2a3560; border-radius: 14px; padding: 16px 18px; color: #e6ecff; }}
.tile b {{ display: block; color: #38e1f5; margin-bottom: 4px; }}
.tile span {{ color: #8c96b8; font-size: 14px; }}
.keys {{ margin-top: 24px; font-size: 14px; color: #8c96b8; text-align: center; }}
.keys b {{ color: #e6ecff; background: #1a2344; border-radius: 5px; padding: 1px 6px; font-weight: normal; }}
.badges {{ text-align: center; margin-top: 12px; }}
.badge {{ display: inline-block; border: 1px solid #2a3560; border-radius: 999px; padding: 3px 12px; margin: 4px; font-size: 13px; color: #5cf2b0; background: #10243a; }}
</style></head><body>
<div class="wrap" style="padding-top: 12px; padding-bottom: 12px">
<div class="hero">
<span class="mark"></span>
<h1>æther</h1>
<p class="lead">A quieter window onto the web.</p>
<div class="badges"><span class="badge">Scripts on a leash</span><span class="badge">No plugins</span><span class="badge">Nothing kept</span><span class="badge">https first</span></div>
<div class="omnibox">Type an address in the bar above and press <b>Enter</b>.</div>
</div>
<div class="tiles">
<a class="tile" href="about:security"><b>How you're protected</b><span>What this browser does, and doesn't do, by design.</span></a>
<a class="tile" href="https://example.com/"><b>example.com</b><span>A small page to check that the network works.</span></a>
</div>
<div class="glass" style="margin-top: 26px">
<p><b>Tabs work the way you expect.</b> <b>Ctrl+T</b> opens one, <b>Ctrl+W</b> closes it, <b>Ctrl+Tab</b> moves between them, and <b>Ctrl+1</b> to <b>Ctrl+9</b> jumps to a tab. Each tab keeps its own history and its own script choice.</p>
<p class="dim" style="margin-bottom:0">Pages are HTML and CSS with pictures, decoded by the browser's own PNG, JPEG, GIF and BMP decoders. Scripts run only on https and only the site's own unless you choose otherwise, and nothing is stored after you close the window.</p>
</div>
<p class="keys"><b>Ctrl+L</b> address &nbsp; <b>Tab</b> next link &nbsp; <b>Space</b> / <b>PgDn</b> scroll &nbsp; <b>Alt+←</b> back &nbsp; <b>Esc</b> stop</p>
</div>
</body></html>"#
    )
}

pub fn security() -> String {
    format!(
        r#"<!doctype html><html><head><title>Security and privacy</title><style>{STYLE}
li {{ margin-bottom: 8px; }}
ul {{ padding-left: 22px; }}
.ok {{ color: #5cf2b0; font-weight: bold; }}
.no {{ color: #ff5c7a; font-weight: bold; }}
</style></head><body><div class="wrap">
<p><a href="about:home">&larr; Home</a></p>
<h1>Security and privacy</h1>
<p class="lead">This browser is small on purpose. Most attacks on browsers need a feature it simply doesn't have.</p>

<div class="glass">
<h2 style="margin-top:0">Scripts, on a short leash</h2>
<p>Pages run scripts in <b>æther script</b>, a JavaScript engine written from scratch for this browser. It runs the site's own scripts on https pages, never on plain http. Scripts can only talk to the site they came from, can't open windows, can't run code made from strings, and are stopped if they run too long. Anything they store is forgotten when you close the window.</p>
<p class="dim" style="margin-bottom:0">The <code>{{}}</code> button next to the address changes this for a site: the site's own scripts, other sites' scripts too, or none.</p>
</div>

<h2>What is never kept</h2>
<ul>
<li><span class="no">No persistent cookies or storage.</span> Nothing a site sends is written to disk: no cookies, no local storage, no cache. Closing the window forgets everything.</li>
<li><span class="no">No plugins or extensions,</span> and no downloads: files that aren't pages are not saved.</li>
<li><span class="no">No downloaded fonts.</span> Every page is drawn in Facet, æther's own font, so fonts can't be used to fingerprint you or to exploit a font parser.</li>
<li><span class="ok">Pictures are decoded in the browser.</span> PNG, baseline JPEG, GIF and BMP use small Rust decoders written for æther, with per-picture dimensions, total pixels and total bytes capped. SVG, WebP and progressive JPEG are not rendered yet: those pictures keep their description instead.</li>
</ul>

<h2>Connections</h2>
<ul>
<li><span class="ok">https by default.</span> An address without a scheme is opened over https.</li>
<li><span class="ok">Plain http is a deliberate choice.</span> Before an http page loads you see a warning, and the padlock stays open while you're there.</li>
<li><span class="ok">No silent downgrades.</span> A secure page can't redirect you to an insecure one, and a secure page never loads insecure stylesheets.</li>
<li><span class="ok">Network access is a capability.</span> æther gives this app the network only because its package asks for it (CAP_NET), and the system's firewall still decides where it may connect.</li>
</ul>

<h2>Built to resist hostile pages</h2>
<p>The HTML, CSS and URL parsers were written for this browser and are bounded everywhere: nesting depth, element counts, attribute sizes, stylesheet rules, selector work and layout depth all have limits, and malformed input is repaired rather than trusted. A page can be slow or ugly, but its scripts run inside budgets they can't escape, in an interpreter that never turns them into machine code.</p>
<p class="dim">Engine: æther script interpreter, HTML tokenizer and tree builder, CSS parser and cascade, block, inline, table and flex layout, PNG/JPEG/GIF/BMP decoders, painted in Facet. Written from scratch in Rust with no external libraries.</p>
</div></body></html>"#
    )
}

/// The warning shown before an http:// page loads.
pub fn http_warning(url: &str) -> String {
    let u = escape(url);
    format!(
        r#"<!doctype html><html><head><title>Not private</title><style>{STYLE}
.warn {{ border-color: #ff5c7a; background: #2a1422; }}
.btn {{ display: inline-block; padding: 9px 18px; border-radius: 9px; margin-right: 10px; font-weight: bold; }}
.primary {{ background: #38e1f5; color: #0b1022; }}
.secondary {{ border: 1px solid #ff5c7a; color: #ff9db0; }}
.url {{ font-family: monospace; color: #ffd0da; word-break: break-all; }}
</style></head><body><div class="wrap">
<h1 style="color:#ff5c7a">This connection isn't private</h1>
<p class="lead">The address uses plain <b>http</b>. Anyone on the network between you and the site can read what you see and send, and can change the page on its way to you.</p>
<div class="glass warn"><p class="url">{u}</p>
<p class="dim" style="margin:0">Many sites also answer over https. The safe choice is to go back.</p></div>
<p style="margin-top:22px"><a class="btn primary" href="about:back">Go back</a><a class="btn secondary" href="about:continue">Continue over http</a></p>
</div></body></html>"#
    )
}

/// A page that couldn't be loaded.
pub fn error(url: &str, message: &str) -> String {
    let u = escape(url);
    let m = escape(message);
    format!(
        r#"<!doctype html><html><head><title>Can't open page</title><style>{STYLE}
.err {{ border-color: #6d3be8; }}
.url {{ font-family: monospace; color: #b8c2e6; word-break: break-all; }}
</style></head><body><div class="wrap">
<h1>Can't open this page</h1>
<p class="url">{u}</p>
<div class="glass err"><p style="margin:0"><b>{m}</b></p></div>
<p class="dim">Check the address, or try again with Reload. <a href="about:home">Home</a> &middot; <a href="about:security">About this browser</a></p>
</div></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        let p = error("https://x/<script>", "bad & <b>");
        assert!(p.contains("&lt;script&gt;"));
        assert!(p.contains("bad &amp; &lt;b&gt;"));
        let d = crate::html::parse(&p);
        assert!(d.find("script").is_none());
    }
}

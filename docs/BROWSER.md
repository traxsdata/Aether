# The browser

æther comes with Browser, a small web browser built for safety first. It
renders HTML, CSS and pictures, with text drawn in Facet, æther's own font.
Pages can run scripts in **æther script**, a JavaScript engine written
from scratch for this browser with tighter limits than a usual browser:
scripts run only on https pages, only the site's own by default, can talk
only to their own origin, and keep nothing after the window closes. A
toolbar button changes that per site (see [Scripts](#scripts)).

It has no plugins, no extensions and no downloads. It keeps no cookies,
no local storage and no cache on disk: closing the window forgets everything.
Pages can't load fonts (everything is drawn in Facet). Pictures are decoded
by the browser's own PNG, JPEG, GIF and BMP decoders, with hard limits; a
picture that can't be decoded keeps its alt text. Addresses are https unless
you choose otherwise, and plain http needs your consent each time for a site.

Tabs work like a modern browser: **Ctrl+T** opens one, **Ctrl+W** closes it,
**Ctrl+Tab** / **Ctrl+Shift+Tab** move between them, and **Ctrl+1** to
**Ctrl+9** jump to a tab. Every tab keeps its own history, scroll position,
scripts and http choices.

The whole browser is Rust written for æther with no external crates: the
HTML parser, the CSS engine, layout, painting, the picture decoders and the
window. It uses only
`std`, `aether-host` (windows and input) and `webnet` (the network layer).

```sh
tools/build-browser.sh            # builds build/apps/browser.app
make && AETHER_DISK=none tools/qemu-drive.py tests/browser.txt
```

Then `run browser` in the terminal, or click its dock icon.

![Browser with two tabs](browser-tabs.png)

![Pictures decoded by the browser](browser-images.png)

## Engine and interface

The code is in `runner/browser/src`:

| Module | What it does |
|---|---|
| `html/` | Charset detection (BOM, HTTP header, `<meta>` prescan, then UTF-8 or Windows-1252/Latin-1, UTF-16), a tokenizer with character references (named, numeric, the legacy forms without `;`), and a tree builder following the HTML standard's insertion modes: implied end tags, `p`/`li`/`dd`/`td` auto-closing, void elements, raw text for `script`/`style`/`textarea`/`title`, tables with foster parenting, the adoption agency for misnested formatting, SVG and MathML as foreign content. `<noscript>` is parsed as ordinary markup, then hidden when the page's scripts run. |
| `css/` | A CSS Syntax 3 tokenizer, a stylesheet parser (`@media` evaluated against the window, `@import`, `@supports`; `@font-face` ignored), selectors up to Selectors 4 (`:is`, `:where`, `:not`, `:has`, `:nth-*`, attribute operators; `:hover` and friends never match, so pages look the same however you point), specificity and the cascade (user agent, presentational attributes, author, inline, `!important`), inheritance, custom properties with `var()`, `calc()`/`min()`/`max()`/`clamp()`, and units px, em, rem, %, pt, vw, vh and more. A user-agent stylesheet (`css/ua.css`) gives HTML its default look. |
| `layout/` | A box tree from the styled DOM, then block flow with margin collapsing, inline flow with line breaking, `white-space`, `text-align` (with justify), baseline alignment and inline boxes; lists with bullets and numbers (decimal, roman, alpha, greek); tables with automatic column widths, `colspan`/`rowspan` and captions; flex rows and columns with wrapping; simple grids; floats as inline blocks; relative and absolute positioning; pictures at their intrinsic size, the page's width/height or `max-width`, preserving the aspect ratio; form controls. The result is a display list plus hit regions (links, fields) and anchors for `#fragments`. |
| `paint.rs` | Software drawing into the window: anti-aliased rounded rectangles, gradients, borders (solid, dashed, dotted), scaled RGBA pictures, and text. |
| `image/` | The picture decoders: PNG (non-interlaced, 8- and 16-bit, palette and tRNS), baseline and progressive JPEG (Huffman, 8x8 IDCT, 4:4:4/4:2:2/4:2:0, grayscale), GIF (first frame, LZW, transparency) and uncompressed BMP. Pure safe Rust with size limits and no `unsafe`. |
| `font.rs` | Facet outlines (generated from `tools/mkfont.py` by `tools/mkfont-rs.py`) rasterised at any size with 4x4 supersampling and cached: bold, italic and monospace variants, extra glyphs for typography (curly quotes, dashes, bullets, arrows, currency), accents composed onto letters, and a box for characters Facet doesn't have. |
| `page.rs` | Loading: fetch through the network seam, decode, parse, collect `<style>` and `<link rel=stylesheet>` (fetched through the seam, with `@import`), `<base href>`, pictures (`<img src>`, `srcset` and data URLs), form submission (GET, and POST through webnet), fragments. |
| `script/` | æther script: the language (lexer, parser, interpreter, garbage collector, regular expressions, built-ins), the DOM and window bindings, and `prelude.js`, platform pieces written in æther script itself (URL, fetch, XMLHttpRequest, typed arrays, observers...). See [Scripts](#scripts). |
| `pages.rs` | The built-in pages: `about:home`, `about:security`, the http warning and error pages. |
| `ui.rs` | The window: the tab strip, toolbar, address bar, page view, input and the per-tab state. |
| `net.rs` | The seam to the network layer (see below). |

### Using it

- **Tabs.** The strip above the toolbar shows one tab per page. Click a
  tab to switch, its **×** to close it, or **+** to open a new one. The
  active tab is lighter and has a cyan top line. **Ctrl+T**, **Ctrl+W**,
  **Ctrl+Tab** and **Ctrl+1**…**Ctrl+9** do the same from the keyboard.
- **Address bar.** Click it or press **Ctrl+L**, type, press **Enter**.
  `example.com` means `https://example.com/`. The padlock is green and
  closed for https pages, red and open for http, and the æther mark for
  built-in pages.
- **Back, forward, reload, stop.** The toolbar buttons, **Alt+←**/**Alt+→**,
  **Backspace** for back, **Ctrl+R** to reload and **Esc** to stop a load in
  progress (the reload button turns into stop while loading).
- **Scrolling.** The mouse wheel, **PgUp**/**PgDn**, **Space** and
  **Shift+Space**, the arrow keys, **Home**/**End**, or drag the scrollbar.
- **Links.** Click them; the status line at the bottom shows where a link
  goes when you point at it. **Tab** and **Shift+Tab** move keyboard focus
  between links and form fields, **Enter** follows the focused link.
- **Forms.** Text fields, checkboxes, radio buttons and submit buttons
  work; forms are sent as GET (or POST, never over plain http without your
  consent). Select boxes show their selected option.
- **Plain http.** Opening an `http://` address first shows a warning page;
  "Continue over http" loads it and remembers the choice for that host until
  the window closes. Pages can't trigger "continue" themselves: the warning's
  internal links only work on the warning page, and web pages can only link
  to `about:home` and `about:security`.
- **Scripts.** The `{}` button left of the address bar shows what may run
  on this site: cyan for the site's own scripts (the default on https),
  violet for other sites' scripts too, red and crossed out for none.
  Clicking it moves through those three and reloads the page. The choice
  lasts until the window closes.
- **Pictures.** PNG, JPEG (baseline and progressive), GIF and BMP are
  decoded and drawn at their intrinsic size, or at the width and height the
  page gives. `alt` text is shown when a picture can't be loaded or decoded.
- **Errors.** When a page can't be loaded the error page shows the network
  layer's own message (`FetchError`'s text).

The window title is the active tab's title. Pages load on a worker thread,
so the window stays responsive and Stop works.

### Robustness

Every parser assumes hostile input and has limits instead of trust: tree
depth (200 open elements), node count (250 000), attribute count and size,
comment size, CSS rule count, selector length and nesting, a step budget for
selector matching, a budget for `var()` expansion, layout depth, box count,
and sizes clamped to sane ranges. Malformed input is repaired, never
rejected with a panic.

Pictures are untrusted too. A page may decode at most 96 pictures and
32 million pixels in total; each picture is at most 8192 pixels on a side,
16 million pixels in all, and 8 MiB compressed. PNG and GIF decompression is
capped before allocation, JPEG coefficients are checked for overflow, and a
damaged picture becomes its alt-text placeholder. An https page never loads
an http picture (mixed content), and no picture fetch sends cookies or a
referrer.

The test suites in `runner/browser/tests` include deep nesting, huge inputs,
three deterministic fuzzers (markup fragments, random bytes, stylesheets)
that run the whole pipeline down to painting, and picture tests for every
decoder, damaged input, decompression bombs and mixed content:

```sh
cd runner && cargo test -p browser
cargo run -p browser --example render -- page.html out.png   # one page to PNG
```

`runner/browser/tests/screens.txt` drives the browser headless and dumps
screenshots of the test pages in `runner/browser/testpages` (an article,
a library page with navigation, lists and tables, a styled pricing page,
a picture page and a hostile page); `tools/build-browser.sh --shots` runs
it, including a two-tab view.

## Scripts

æther script is the browser's own JavaScript engine (`runner/browser/src/script`),
written for æther in std-only Rust with no external crates. It runs the
JavaScript that ordinary sites use, ES2020 or so: classes, closures,
destructuring, spread, template literals, arrow functions, `async`/`await`
and promises, Map/Set/WeakMap, Symbol, JSON, regular expressions, Date and
Math. It is a tree-walking interpreter over an arena heap with a
mark-and-sweep collector, so a bug in it can't turn into machine code.

### What it leaves out, on purpose

- **No code from strings.** `eval`, `new Function`, and `setTimeout` or
  `setInterval` with a string are refused: a page can only run code it
  shipped as code.
- **No generators or modules** (`import`/`export`), no `with`, no
  WebAssembly, no workers, no shared memory, so no high-resolution timing
  tricks either.
- **No reaching out.** `fetch` and `XMLHttpRequest` reach only the page's
  own origin (scheme, host and port), with GET, HEAD or POST.
  `navigator.sendBeacon` does nothing, `window.open` is refused,
  `alert` shows in the status line, `confirm` answers no and `prompt` gives
  null. WebSockets, notifications, geolocation, camera, microphone,
  clipboard and service workers don't exist.
- **Nothing kept.** `localStorage`, `sessionStorage` and `document.cookie`
  work, but live in memory for this window only (5 MB a site), and script
  cookies are never sent to servers.

### Limits

A tab that loads in the background finishes its page, but its scripts wait
until you select the tab, so a background page can't spend the UI thread's
script budget while you read another one. Every task (a script, an event
handler, a timer) runs until it finishes or reaches a budget, then is
stopped and reported in the page's errors: 30
million steps per task (120 million for the page's first run, 40 million
for each round of timers), recursion depth 700, 2 million objects, strings
and arrays of 16 M, a million DOM nodes, 10,000 timers (at least 10 ms
apart), 2,000 fetches (6 at a time) and regular expressions with their own
step budget, so a page can't hang the browser with a runaway loop or a
catastrophic pattern. A page gets up to 200 scripts, 4 MB each and 24 MB in
all.

### Which scripts run

| Setting | Runs |
|---|---|
| This site's own (default on https) | inline scripts, and external ones from the same site (`cdn.example.com` counts as example.com) |
| Other sites' too | also external scripts from other sites, still https only |
| Off (always on plain http) | nothing; `<noscript>` content is shown |

External scripts are fetched like stylesheets, through the same seam,
firewall and TLS checks. `javascript:` links run as the page's own code
when scripts are on; typing `javascript:` in the address bar does nothing.

### How it fits the page

Scripts see the page's DOM (elements, attributes, `classList`, `style`,
`dataset`, `innerHTML`, forms, events with capture and bubbling) and the
window (`location`, `history.pushState`, timers, `requestAnimationFrame`,
`matchMedia`, `getBoundingClientRect` from the real layout). Clicks, keys,
form changes, submit, scrolling and resizing reach the page as events, and
whatever a script changes is laid out again. Navigation from a script goes
through the same checks as a click, and more than 8 script navigations in a
row are stopped.

### Not supported (by design or not yet)

Plugins, downloads, cookies, persistent storage, web fonts, WebP/AVIF/SVG
pictures, CSS background images, video, audio, iframes (shown as a
link-sized frame), canvas drawing, CSS animations and transforms, `:hover`
styling, text selection and copying, bookmarks, zoom and printing.
Characters outside Facet's set (most non-Latin scripts) are drawn as boxes.

## Network and TLS

The browser's network layer is `runner/webnet`, a library crate in pure,
std-only Rust with no external crates: the URL parser, the HTTP/1.1 client,
the TLS 1.3 client and all the cryptography under it are written for æther.
The browser runs as an ordinary static `x86_64-unknown-linux-musl` program
in æther's Linux layer and reaches the network only through æther's own
calls (below), so the kernel resolves every host name and the firewall
decides every connection.

### What the browser calls

```rust
use webnet::{fetch, FetchError, Method, Request, Response, Url};

let url = Url::from_user_input("example.com")?;   // -> https://example.com/
let resp: Response = fetch(&Request::get(url))?;  // follows redirects
// resp.url (final URL), resp.status, resp.headers, resp.body, resp.secure
```

- `Url::parse(s)`, `Url::from_user_input(s)` (no scheme means https),
  `url.join(rel)` (links and redirects), `as_str`, `scheme`, `host`,
  `port_or_default`, `path`, `query`, `fragment`. Errors are `UrlError`.
- `Request { url, method: Method::{Get, Head, Post}, body, allow_http,
  max_bytes }`; `Request::get(url)` and `Request::post_form(url, body)` set
  the defaults (8 MiB limit, http not allowed). `url::form_urlencoded`
  builds a form body.
- `fetch(&Request) -> Result<Response, FetchError>`. `FetchError`'s
  `Display` is a sentence for the error page ("The connection isn't secure:
  the certificate has expired."). `FetchError::InsecureHttp` and
  `FetchError::Downgrade` let the interface tell the http cases apart.
- `fetch_with(&Config, &Request)` takes timeouts and, for development on a
  host only, a proxy or a fixed address to connect to. On æther these
  development options are ignored.

### Policy

- **https by default.** Typing `example.com` means `https://example.com/`.
  A plain http URL fails with `InsecureHttp` unless the caller sets
  `allow_http`, which the interface does only after warning the user.
- **No downgrades.** Once a request has been https, a redirect to http is
  refused, whatever `allow_http` says. At most 10 redirects; 303 (and
  301/302 after a POST) become GET.
- **Bounded everything.** Header lines 16 KiB, headers 64 KiB and 128
  fields, the body `max_bytes` (after decompression, so a "zip bomb" can't
  grow), 15 s to connect, 30 s per read, 120 s per request.
- **Strict parsing.** Both Transfer-Encoding and Content-Length,
  conflicting Content-Lengths, folded header lines, bare CRs, unknown
  transfer codings: all refused rather than guessed at.
- **No cookies, no cache, nothing written to disk.** Each fetch is
  independent (`Connection: close`). A per-session in-memory cookie jar can
  come later; until then sites that need a login won't keep it.
- **URLs**: http and https only; ASCII host names (non-ASCII/IDNA hosts are
  refused rather than risk look-alike names), dotted-decimal IPv4 only (no
  `0x7f.1` tricks), no IPv6 literals yet, and no `user:password@` (mostly
  used to disguise where a link goes).
- Bodies are decoded from gzip or deflate (our own inflate, checksums
  verified). The response is the decoded body.

### TLS 1.3 and 1.2

`webnet::tls` is a TLS 1.3 client (RFC 8446) that also speaks TLS 1.2
(RFC 5246) to servers that haven't moved on yet: about a fifth of the
sites tested still answer only 1.2, often on the bare domain (samsung.com,
mit.edu, eff.org). No 0-RTT, no session resumption, no renegotiation, no
compression. TLS 1.1 and older are refused.

- Key exchange X25519, P-256 or P-384. The first ClientHello carries shares
  for X25519 and P-256 (Microsoft's servers, bing.com and office.com among
  them, have no X25519); a TLS 1.3 server that wants P-384 asks with
  HelloRetryRequest, which is handled (one retry, cookie echoed).
- TLS 1.3 cipher suites, in our order: TLS_CHACHA20_POLY1305_SHA256 (the
  fastest in portable constant-time software), TLS_AES_128_GCM_SHA256,
  TLS_AES_256_GCM_SHA384.
- TLS 1.2 is held to what is still considered safe: ECDHE key exchange
  only (forward secrecy; no static RSA, no finite-field DHE), AEAD suites
  only (ECDHE-ECDSA/RSA with ChaCha20-Poly1305, AES-128-GCM and
  AES-256-GCM; no CBC, so no padding oracles), uncompressed points, the
  extended master secret (RFC 7627) when the server has it, an empty
  renegotiation_info, and HelloRequest ignored. The ServerKeyExchange
  signature may be PKCS#1 v1.5, as 1.2 servers use it; SHA-1 is not
  offered.
- Downgrade protection (RFC 8446 section 4.1.3): a TLS 1.2 answer whose
  random ends in the `DOWNGRD` marker means someone removed 1.3 from our
  hello, and the connection is refused.
- SNI (not for IP addresses), ALPN `http/1.1`, a compatibility
  ChangeCipherSpec, an empty Certificate if a server asks for one, KeyUpdate
  handled, session tickets ignored.
- In TLS 1.3 the server proves its key with CertificateVerify: ECDSA
  P-256/SHA-256, ECDSA P-384/SHA-384 or RSA-PSS with SHA-256/384/512.
  PKCS#1 v1.5 is accepted only inside certificates, as RFC 8446 says.
- Every extension a server sends must be one we offered; duplicates,
  trailing bytes, oversized records, a changed session id or a second
  HelloRetryRequest are fatal, with the matching alert sent.
- A connection that ends without close_notify is an error, not the end of
  the page: a truncation attack can't cut a page short silently.

### Certificates

`webnet::x509` parses DER strictly (definite minimal lengths, v3 only, no
trailing data) and rejects any certificate with an unknown critical
extension. A chain is accepted only if:

- it leads from the server's certificate to a trusted root, each signature
  checked (RSA 2048-4096 bits PKCS#1 v1.5 or PSS, ECDSA P-256 or P-384,
  SHA-256/384/512), at most 8 intermediates, built from whatever the server
  sent, in any order;
- every certificate is within its validity dates, by æther's clock
  (SYS_TIME) or the system clock on a host;
- every issuer is a CA (basicConstraints), within its path length, allowed
  to sign certificates (keyUsage) and not restricted to other purposes
  (extKeyUsage), and its DNS and IP name constraints allow the server's
  names;
- the server's certificate is for web servers (extKeyUsage, if present) and
  may sign (keyUsage), and names the host: subjectAltName only (the common
  name is ignored), `*.` wildcards only as a whole leftmost label matching
  exactly one label, never `*.com`-style (RFC 6125). IP addresses must be
  listed as IP addresses.

Not done yet: revocation (OCSP, CRLs), Certificate Transparency, and
pinning.

### Cryptography

All in `webnet::crypto`, from the specifications: SHA-256/384/512, HMAC,
HKDF, the TLS 1.2 PRF, X25519, P-256/P-384 ECDH, ChaCha20-Poly1305,
AES-128/256-GCM, ECDSA P-256/P-384 verification and RSA verification.
Secret-dependent code has no secret-dependent branches or table lookups:
X25519 swaps with masks and inverts by a fixed chain, P-256/P-384 ECDH
(`ecdh.rs`) uses fixed-size Montgomery arithmetic with masked
corrections, the complete addition formulas of Renes, Costello and Batina
(no special cases to branch on) and 4-bit windows read by scanning the
whole table, Poly1305 reduces without branches, AES has no
S-box table (SubBytes is computed bitsliced, as the inverse in GF(2^8)
followed by the affine map, for four blocks at a time), GHASH multiplies
with masks, and tags are compared in constant time. Key material is wiped
when dropped. Signature verification works on public data and isn't
constant-time. Randomness comes from the `getrandom` call, which æther's
Linux layer answers from the kernel's generator; without it the program
stops rather than use weak randomness.

The software AES is about 6 MB/s on a current x86 core, ChaCha20 well over
100 MB/s; that's why ChaCha20 is offered first.

### Trust anchors

Trust has to come from somewhere: the root certificates are the one piece
of external data the browser needs. They are data, not code.
`runner/webnet/src/roots_data.rs` is generated by `tools/mkroots.py` and
keeps, for each root, only its subject name, public key and expiry date.

The committed file holds the 121 roots of **Mozilla's root store**, as
packaged by Ubuntu 24.04's `ca-certificates` 20260601~24.04.1
(`/usr/share/ca-certificates/mozilla/*.crt`). The input's SHA-256 is in the
file's header. To refresh it:

```sh
tools/mkroots.py --source "Mozilla roots, ca-certificates <version>" \
    -o runner/webnet/src/roots_data.rs /usr/share/ca-certificates/mozilla
# or Mozilla's own bundle: curl -O https://curl.se/ca/cacert.pem
tools/mkroots.py --source "curl.se cacert.pem <date>" -o runner/webnet/src/roots_data.rs cacert.pem
```

Use a bundle of public web roots only. A system's
`/etc/ssl/certs/ca-certificates.crt` may contain locally added roots (a
company's or a proxy's interception CA, for instance), and anything in the
input can vouch for any web site. Review the diff before committing.
Expired roots are dropped when generating, and checked again at run time.

### How it reaches the network

`aether-host` has a `net` module: `TcpStream::connect(host, port,
timeout_ms)`, `Read`, `Write`, `set_read_timeout`. On æther (`AETHER=1`) it
uses the native calls from `include/abi.h`, made from the Linux layer as
system call `0x1000 + N`:

| Call | Use |
|---|---|
| `SYS_NET_CONNECT` (34) | `(host, len, port, timeout_ms)`: the kernel resolves the name; the firewall decides by app and host |
| `SYS_NET_SEND` (35) | `(handle, buf, len)`: returns bytes accepted |
| `SYS_NET_RECV` (36) | `(handle, buf, len, timeout_ms or -1)`: 0 means the peer closed |
| `SYS_NET_CLOSE` (37) | when the stream is dropped |
| `SYS_NET_INFO` (38) | `aether_host::net::info()` |

Errors become `io::Error`s: `E_DENIED` is PermissionDenied "blocked by the
firewall" (also what an app that has read Confidential data gets),
`E_PERM` PermissionDenied "this app has no network permission",
`E_TIMEOUT` TimedOut, `E_NET` ConnectionRefused while connecting and
ConnectionReset afterwards. `fetch` turns a PermissionDenied into
`FetchError::Blocked`, shown as "The connection to example.com was refused:
blocked by the firewall."

Anywhere else the same type wraps `std::net::TcpStream`, so the whole stack
can be developed and tested on Linux.

### Testing

```sh
cd runner && cargo test -p webnet -p aether-host   # 57 unit tests
tools/test-webnet-tls.sh                            # 51 handshakes against openssl s_server
cargo run --release --bin webget -- https://example.com
```

- The unit tests check every primitive against published vectors: FIPS
  180-4 (SHA-2), RFC 4231 (HMAC), RFC 5869 (HKDF), RFC 7748 (X25519, incl.
  1000 iterations), RFC 8439 (ChaCha20, Poly1305, the AEAD), FIPS 197 (AES),
  the GCM specification's test cases (AES-128/256-GCM), RFC 6979 (ECDSA
  P-256 and P-384), RFC 8448 (the TLS 1.3 key schedule); RSA PKCS#1 v1.5
  and PSS against signatures made with OpenSSL; P-256/P-384 ECDH and the
  TLS 1.2 PRF against values from OpenSSL. They also cover the URL
  parser (including RFC 3986's resolution examples), HTTP framing,
  inflate, DER, dates, host matching and the fetch policy.
- `tools/test-webnet-tls.sh` makes a throwaway CA and checks, against
  `openssl s_server`, each TLS 1.3 and 1.2 cipher suite with RSA and ECDSA
  keys, RSA-4096, P-384, an intermediate, RSA-PSS SHA-384/512, PKCS#1 on a
  1.2 ServerKeyExchange, wildcards, a client certificate request and each
  key exchange group (P-384 through HelloRetryRequest in 1.3), then that
  each of these fails: a wrong host name, an IP not in the certificate, a
  wildcard over one label or two labels, expired and not-yet-valid
  certificates, an untrusted root, a non-CA issuer, a missing
  intermediate, a client-only certificate, a PKCS#1 CertificateVerify in
  1.3, P-521 only, and in 1.2 CBC-only, static-RSA-only, DHE-only and
  SHA-1-only servers, and TLS 1.1.
- `tools/test-real-sites.sh` runs the browser inside QEMU against real
  sites (Wikipedia over 1.3, bing.com over 1.3 with P-256, eff.org and
  mit.edu over 1.2, samsung.com over 1.2 then 1.3) with the built-in
  roots, from a sandbox whose only way out is a proxy:
  `tools/real-https-relay.py` answers DNS and tunnels each connection,
  still encrypted end to end, through the proxy.
- `webget` is a small command-line client. Its `--ca-file`,
  `--only-ca-file`, `--connect-to` and `--proxy` options exist only in that
  tool, for testing behind an intercepting proxy or against a local
  server; the library never uses them by default.

### The seam

The browser reaches the network only through `runner/browser/src/net.rs`,
which re-exports `Url`, `Request`, `Response`, `Method`, `fetch`,
`FetchError` and `UrlError` from `webnet`. The interface maps
`FetchError::InsecureHttp` to the http warning page and shows every other
error's text on the error page. Tests and headless screenshot runs on a
host use a local fetcher instead (`BROWSER_TEST_ROOT`, see
`runner/browser/src/testserve.rs`), which is never used on æther.

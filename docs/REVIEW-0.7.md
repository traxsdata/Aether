# Review of the 0.7 work

This is a working review of the 0.7 branch. It started from `2f8b042` (the
merge of 0.6) and merged current `main` (`5c38206`) with the shared
Aether-CraftApps release. It covers the Chrome-like browser shell, the
picture pipeline and app icons. It also records what is deliberately not in
this first 0.7 slice.

## What 0.7 changes

- **Browser tabs.** The window is now a tab strip plus the selected tab's
  toolbar and page. Each tab has its own page, history, scroll position,
  form state, scripts, script policy and plain-http consent. Ctrl+T opens a
  tab, Ctrl+W closes it (closing the last one closes the window), Ctrl+Tab
  and Ctrl+Shift+Tab move between tabs, and Ctrl+1..Ctrl+9 select one.
  The window title follows the active tab. Every tab is capped at 32.
- **A new-tab page and a Chrome-like layout.** The tab strip, omnibox,
  navigation buttons, security and script buttons, and the new-tab page use
  the same dark-glass design. Loading tabs show a spinner; the active tab
  has a close button.
- **Pictures.** `<img>` elements are fetched through the normal network
  seam (TLS, firewall, no cookies, no referrer), decoded in-process and
  drawn. PNG (8/16-bit, palette, tRNS), baseline and progressive JPEG
  (grayscale, 4:4:4, 4:2:2, 4:2:0), GIF (first frame, LZW, interlace,
  transparency) and uncompressed BMP (8/24/32-bit) are supported. Data URLs
  work without a network fetch. An https page refuses http pictures.
- **App icons.** App packages may carry an RGBA icon after the ELF. The
  header records its dimensions, offset and SHA-256, and the kernel checks
  all of them. The dock draws it; App Store reads an installed package's icon
  with `SYS_APP_ICON` and shows a coloured initial for apps that aren't
  installed. Browser, Settings, App Store, and the 13 Craft apps have icons;
  the Craft icons live only in their own untrusted packages.
- **Craft packages from Aether-CraftApps.** The branch keeps main's shared
  `craft-apps-latest` release: one API call lists every app, and each app's
  version comes from its asset name. Packages are published as `.aeapp`
  (GitHub rejects `.app`), with `.app.zip` copies for manual download. App
  Store prefers a direct package with a SHA-256 digest; if only a ZIP is
  available, it verifies the release digest, extracts the single `.app`
  entry with a small bounds-checked and CRC-checked reader, and then gives
  the package to the kernel. A local package file that is a ZIP is handled
  the same way. `make craft-apps` uses main's `tools/fetch-craft-apps.py`.
- **Docs and tests.** `docs/BROWSER.md`, `docs/APPS.md`, `docs/ROADMAP.md`
  and the README describe the new behavior. The browser test suite grew from
  9 UI tests to 13, plus 8 picture tests; the kernel package test now covers
  icon bounds and hashes, and the installer tests cover the `.app.zip`
  reader.

## Browser shell review

### State separation

`ui.rs` keeps the window in `State` and each tab in `Tab`. A tab owns
everything that must not leak between pages: the loaded document, layout,
form values, history, scripts, policy and http consent. The window owns the
tab list, the framebuffer scratch buffer, the chrome font cache and the
in-memory script storage.

Storage is shared across tabs because it is keyed by origin and should
behave like a browser session jar. It is still in memory only; nothing is
written to disk. Session storage is currently shared too, which is a known
simplification (see gaps).

### Tab lifecycle

- A new tab starts on `about:home` and focuses the address bar.
- Closing a tab calls `Tab::stop()` first, so its worker thread stops at
  the next cancellation point instead of finishing into a receiver that no
  longer exists.
- A tab that is not selected still finishes loading, but its initial
  scripts are deferred and its timers do not run until it is selected.
  This keeps a background page from spending the UI thread's script budget
  while the user is reading another tab.
- `MAX_TABS` is 32. Without a cap, Ctrl+T is an easy way for a user (or a
  script that can synthesize keys? no, scripts cannot) to spend memory on
  pages, layout and font caches.
- A page cannot create, close or select tabs. There is no `window.open`;
  the tab shortcuts are handled by the window before the page sees the key.
- Closing the last tab returns `false` from `handle`, which closes the
  window, as Chrome does. This is an explicit decision, not an accident.

### Per-tab policy

The script policy and plain-http consent remain per tab. A user who allows
third-party scripts on `example.com` in one tab does not silently allow them
in another tab; the stricter choice is deliberate. The address bar and
padlock always describe the active tab, and a background tab cannot change
the title or address bar of another tab.

## Picture pipeline review

### Loading

- `page::gather_images` runs after styles and before scripts. It reads
  `src` first and falls back to the largest `srcset` candidate.
- Network pictures go through `Request`/`fetch`, so TLS verification,
  redirect policy and the kernel firewall all apply exactly as they do for
  the page and stylesheets.
- `allow_http` is true only when the user already chose an insecure page.
  On an https page an http picture is refused before a connection is made.
- Data URLs are decoded in place, capped at 8 MiB. Invalid base64 or a
  non-image MIME type fails into the alt-text placeholder.
- Limits: 96 pictures per page, 8 MiB per picture, 32 MiB compressed in
  total, 32 million pixels in total. The decoders also cap each picture at
  8192 pixels per side and 16 million pixels in total.
- Pictures are fetched four at a time on short-lived threads. Cancellation
  is checked between batches; the page load itself is cancellable between
  fetches. A decode can take a moment for a large picture, but it is on the
  loader thread, not the UI thread.
- No picture request carries cookies or a Referer. The browser has no
  cookie jar to send, and `webnet` does not add one.

### Decoders

All decoders are safe Rust with no `unsafe`, no external crates and no
allocation based on an unchecked header field.

- **PNG:** checks the signature and every chunk length, accepts non-
  interlaced 8- and 16-bit gray/RGB/palette/gray+alpha/RGBA, applies all
  five filters, and handles tRNS. The zlib stream is bounded by the expected
  scanline size; a decompression bomb hits the cap before the pixel buffer
  is filled. 16-bit pictures are capped lower to bound the temporary
  scanline buffer.
- **JPEG:** parses DQT/DHT/SOF/SOS/DRI, baseline and progressive scans,
  restart markers (including non-interleaved MCU ordering), DC prediction
  and successive approximation. Coefficients are checked against `i16`
  before they are stored. The IDCT is a separable float transform in safe
  Rust. Grayscale and YCbCr are supported; CMYK and arithmetic coding are
  refused rather than guessed at.
- **GIF:** bounds the logical screen, the frame rectangle and the LZW
  output; handles sub-blocks, local/global palettes, transparency and
  interlacing; only the first frame is decoded. Animation is not supported.
- **BMP:** only uncompressed 8/24/32-bit, with palette bounds and row-stride
  checks. 32-bit files whose alpha channel is entirely zero are treated as
  opaque, as most BMP writers intend.

The decoders are exercised by unit and integration tests: exact pixel
checks for PNG/GIF/BMP, baseline and progressive JPEGs at 4:4:4 and 4:2:0,
grayscale, damaged input, an oversized PNG header, and a mixed-content page.

### Painting and layout

`Loaded.images` maps an `<img>` node to an `Arc<Image>` decoded once per
node. Layout gives a replaced box its intrinsic size when the page does not
set width/height, preserving the aspect ratio when only one dimension is
given. Painting scales with nearest-neighbour sampling and blends the
picture's alpha over the page. A picture that failed to load (or that the
page never provided) keeps the existing placeholder and alt text.

## Icon review

The package header grew five fields plus an icon hash in the space that was
reserved:

```
icon_format, icon_width, icon_height, icon_offset, icon_size, icon_sha256
```

`app_check_header()` requires:

- format 0 (no icon) with all icon fields zero, or format 1 (RGBA8888);
- dimensions 1..=256 on each side;
- `icon_size == width * height * 4`;
- the icon starts after the ELF and fits inside the package;
- the icon hash matches when the package is installed or started.

The icon hash is also checked at boot/registration (`app_check_icon`) before
the dock can draw an ISO package, without hashing the larger ELF yet.
`program_verify()` checks it again with the ELF hash on first start.

The icon is data. It is never mapped executable, never interpreted as code,
and is drawn through `gfx_image()` with clipping. A damaged or substituted
icon is refused with the rest of the package.

The browser, Settings, App Store and the 13 Craft apps have icons in
`assets/icons/`. The browser, Settings and App Store icons are original
æther artwork. The Craft icons are the apps' own icons, taken from their
`storytold/<app>` source trees under Apache-2.0; `assets/icons/craft/NOTICE`
records the exact paths. They are compiled only into the apps' own untrusted
packages, never into App Store. App Store asks the kernel for an installed
app's icon with `SYS_APP_ICON` (bounded to 256x256 RGBA) and shows a coloured
initial for apps that aren't installed.

## Review follow-up

The first review at `28b7405` requested two blockers and some cleanups. The
follow-up commit addresses them:

1. **No third-party artwork in a trusted app.** App Store no longer embeds
   the Craft icons. The 13 icons now come from the `storytold/<app>` source
   trees, are attributed in `assets/icons/craft/NOTICE`, and travel only in
   each app's own untrusted package. App Store reads an installed package's
   icon with the new `SYS_APP_ICON` call (CAP_APPS, 256x256 cap) and falls
   back to a coloured initial.
2. **`system_ui::Canvas::image` checks its C precondition.** It rejects a slice shorter than
   `width * height * 4` (and zero/oversized dimensions) before calling C,
   with a test that passes a four-byte slice for a 32x32 picture.
   `Canvas::new` now rejects a pixel slice smaller than `pitch * height`.
3. **Icon verification before display.** `app_check_icon` hashes the icon at
   boot and when `/system/apps` is scanned, so the dock never draws an icon
   whose hash has not been checked.
4. **Catalog update name check.** A downloaded package must have the header
   name of the app being installed or replaced; a release can no longer
   substitute another app while `replace=true`.
5. **App Store scrolling.** Scrolled rows are now positioned relative to
   the list top (`row_y(i - scroll)`) for both drawing and hit testing, so
   the scroll offset no longer pushes rows out of the window.
6. **TCG waits.** `tests/unit.txt` now waits 30 s before typing and 60 s
   after, matching the slower TCG runs.

## Test evidence

Run on this machine (QEMU TCG, no KVM):

```text
cargo test -p browser                         73 lib + 6 hostile + 8 images + 9 layout + 13 UI = 109 passed, 0 failed
cargo test -p system-ui -p aether-host -p eframe -p installer -p webnet -p browser   192 passed, 0 failed
make unittest                                 54 kernel / 66 terminal, 0 failed
make test                                     PASS
AETHER_DISK=none tools/qemu-drive.py tests/browser.txt   PASS
AETHER_DISK=none tools/qemu-drive.py tests/desktop.txt   PASS
tools/build-browser.sh --shots               15 screenshots, including the picture page and two tabs
App Store smoke                              GridCraft installed from a package; its icon drawn via SYS_APP_ICON
```

The QEMU browser run reached `example.com:443` through the browser's web
policy and the kernel network stack. The headless screenshot run uses the
local `test.aether` server, not the network.

## Deliberate gaps in this 0.7 slice

- **CSS background images** are parsed as "there is an image" but not
  fetched or painted yet. `<img>` is fully supported.
- **WebP, AVIF and SVG** are not decoded. Servers that only send those
  formats show the alt-text placeholder. We do not advertise them in an
  `Accept` header, so most servers fall back to JPEG/PNG.
- **Image loading is synchronous with page load.** A page with many large
  pictures takes longer to appear, though the window stays responsive and
  Stop works between batches. A progressive image pipeline is a follow-up.
- **No process per site.** All tabs share the browser process and its
  memory quota. Per-site processes are on the roadmap.
- **No tab restore, tab drag/reorder, or session restore.** The tab strip
  covers the common keyboard and pointer actions.
- **Session storage is shared between tabs.** Local storage is origin-keyed
  and shared, as it should be; session storage should be per tab and will
  be split in a follow-up.
- **No downloads, bookmarks, zoom or find-in-page.**
- **Icon support is in `.app` packages only.** Built-in C apps keep their
  hand-drawn dock icons; the package icon path is used by Browser, Settings,
  App Store and installed Craft apps.
- **Craft package size.** The PhotoCraft `.aeapp` is about 70 MiB (already
  stripped), so the package cap was raised from 64 to 128 MiB in the kernel,
  App Store and `make craft-apps`. Packages still live in physical pages,
  are hash-checked, and need one contiguous allocation to install.
- **Craft shells.** Only GridCraft and PhotoCraft have shells; the
  GPU-heavy apps still need software paths.

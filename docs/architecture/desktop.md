# Window manager and compositor

The desktop is an original, clean-room implementation of a modern desktop
workflow. Its design vocabulary is informed by publicly described macOS 27
"Golden Gate" principles — refined layered translucency, a global transparency
control, tighter uniform window corners, edge-to-edge sidebars, colourful icons,
stronger depth and separation, and search as a first-class action. No Apple
code, fonts, bitmaps, wallpapers, or icon resources are embedded or traced.
Every palette value, glyph, icon, animation curve, and layout rule in this tree
is defined by the project.

The defining reference is [Apple's public high-level description](https://www.apple.com/os/macos/) of Golden Gate:
enhanced Liquid Glass readability and a user-controlled opacity slider. Aether
implements those ideas at the level the current software renderer can honestly
support: layered alpha compositing, specular highlights, darkened edges, three
glass levels, and no claim of GPU blur or HDR.

## Layers

```text
aether-wm (kernel/wm)       host-tested policy: geometry, windows, focus,
                            workspaces, menus, dock, Spotlight, animation,
                            and bounded Files presentation state
aether-input (kernel/input) host-tested decoders, modifiers, bounded event FIFO
kernel/src/desktop.rs       renderer and input adapter (trusted kernel code)
kernel/src/icons.rs         original vector artwork
kernel/src/graphics.rs      double-buffered software compositor
kernel/src/input.rs         IRQ1 keyboard and IRQ12 PS/2 mouse
```

`aether-wm` is `no_std`, forbids `unsafe`, allocates nothing, and has no
hardware I/O, ambient service access, or capability handling. Browser requests
use an explicitly supplied service adapter. It is tested on the host
for focus, z-order, hit testing, workspace transitions, menu navigation,
Spotlight filtering, settings, responsive layout, and animation completion.

## Bounded, damage-tracked composition

The compositor uses a static 1920×1200 32-bit back buffer and an equally bounded
wallpaper cache: 18,432,000 bytes total (about 17.6 MiB). It recomputes the
procedural wallpaper only when its palette changes. Geometry/state changes
damage both old and new window extents, including shadows. All layers are
repainted back-to-front inside that region, then only damaged pixels are copied
to the display. Inner content clips cannot widen the damage clip; a rounded
silhouette clip prevents content from squaring off the lower window corners.

A 15×22 backing store restores the pixels under the cursor, so ordinary pointer
motion does not redraw the scene or copy the whole screen. Queued keys and mouse
events are batched before rendering, scene frames are paced start-to-start at
up to roughly 33 Hz (without adding a delay after painting), and animations
advance by actual elapsed 100 Hz timer ticks. Files reads
are event-driven, not repeated during every paint. `desktop` reports scene/cursor
paint counts and coarse last/max render times; `desktop redraw` explicitly
requests a full repaint for diagnostics.

This prevents intermediate drawing operations from going directly to scanout,
but **is not atomic or guaranteed tear-free**: GOP has no vblank/page-flip API,
and the final volatile copy can overlap scanout. Modes larger than the buffers
fall back to slower direct painting. Descriptor dimensions/stride are bounded
to 8192 pixels and apertures to 512 MiB. Both UEFI RGBX and BGRX formats and
padded strides are honored, with host tests of actual surface writes.

The renderer builds an original procedural "golden hour" wallpaper, then paints
windows back-to-front, the workspace overview, the dock, the menu bar, any open
menu, Spotlight, and finally the pointer. All primitives are clipped to the
destination rectangle and, for window content, to the content area.

## Windows

- Uniform 10-pixel corner radius across applications, with a one-pixel border.
- Three traffic-light controls: close, minimize, and zoom. The glyphs appear on
  hover; the controls remain usable without a pointer through keyboard
  shortcuts.
- A unified 31-pixel toolbar, centred title, specular top edge, darkened lower
  edge, and layered shadow. The focused window has a brighter shell, accent
  border, and stronger shadow.
- Drag a title bar to move; drag the bottom-right grip to resize. Double-click a
  title bar to zoom. Minimum size is enforced.
- Minimize animates the window toward its dock tile. Open and close animations
  are short, fixed-point transitions driven by the 100 Hz timer.
- Closing a window animates it out before its bounded slot is reused.

## Menu bar

The top bar contains the Aether mark (system menu), the active application name,
File, View, Window, and Help menus, and right-hand status items for search,
Spaces, security state, and an uptime clock. Every menu is a real static action
list; there is no decorative menu chrome. Menus support pointer selection and
keyboard navigation (`F10` opens the system menu; `Left`/`Right` change menus;
`Up`, `Down`, `Enter`, `Escape` navigate/activate/dismiss). Padding and separator
clicks cannot execute an action, and search-field clicks cannot activate results.

The system menu exposes application windows, Spaces, the overview, and the
appearance/accessibility toggles. The app menu exposes close, minimize, zoom,
focus cycling, and settings.

## Dock

A floating glass dock holds seven original application tiles and a Spaces tile.
Icons magnify under the pointer, running applications get a status dot, and a
tooltip appears above the emphasised tile. Clicking a running application
focuses or restores it; clicking a closed application opens it. The magnification
can be disabled in Settings.

## Spaces and the overview

There are four workspaces. `Ctrl+Left` and `Ctrl+Right` cycle them with a
horizontal slide; `Ctrl+1`…`Ctrl+4` jump directly. Each window belongs to one
workspace. Switching to an empty workspace leaves focus empty rather than
inventing a target.

`Ctrl+Up`, `F3`, the Spaces status item, and the Spaces dock tile open the
overview. It shows four desktop cards with live window previews and counts.
Arrow keys select a card, `Enter` switches, `Escape` closes, and a click on a
card switches directly.

## Spotlight-style search

`Ctrl+Space` (or the search status item) opens Aether Search. It filters
applications, workspaces, the overview, and accessibility actions as the user
types. Arrow keys select a result, `Enter` executes it, `Backspace` edits the
query, and `Escape` closes the panel. The index is a bounded static table; no
file contents or private data are searched, and the desktop gains no authority
from a search result.

## Files application

Files is a working **scoped RAM-filesystem browser and mutator**, not a
decorative listing or a complete Explorer implementation. Open it from the dock
or `Ctrl+N`:

- Click a sidebar location (Volume, Home, System, Portal).
- Select a row and double-click it, or use `Up`/`Down` and `Enter`, to open a
  folder or a bounded text preview. Selection scrolls into view.
- `Backspace` or the `<` button closes a preview or opens the parent folder.
- `F5` or the `R` button refreshes the folder after a diagnostic shell change.
- `F3` duplicates the selected file under a service-chosen `copyN` name, `F2`
  opens a bounded rename prompt, and `Delete` removes a file or empty folder.
- Previews show at most 512 bytes, do not execute programs, sanitize unsupported
  glyphs/control bytes, and mark truncation. Errors are shown, not disguised as
  empty folders. Failed navigation preserves the previous valid view.

The filesystem-service adapter permits scoped browsing and mutation of `/home`,
`/system`, and `/portal`; the root exposes directory names, but `/apps` vault
contents are denied. A UI path is not a grant and previewing does not authorize
another app. Deletion is refused while a file capability is open, and rename
refuses an existing destination. This adapter is still trusted ring-0 code, not
a separately isolated service. Tabs/split panes, safe portal import/export, and
durable storage are future work.

The Portal window shows one pending consent request at a time with `APPROVE`
and `DENY` controls. A request grants nothing; approval installs an exact-file
record in the portal table, and revocation removes future authority. The same
decisions are available from the console as `portal approve|deny|revoke <n>`.

## Liquid Glass and accessibility

Settings provides four live controls:

- **Reduce motion** — completes transitions without intermediate frames.
- **High contrast** — black surfaces, white text, signal-yellow focus, and
  stronger borders.
- **Liquid Glass** — Clear, Balanced, or Tinted. The level changes the alpha of
  menu-bar, dock, toolbar, sidebar, and window surfaces.
- **Magnify dock** — enables or disables pointer-driven icon magnification.

Keyboard operation has parity with the pointer: `Tab`/`Ctrl+Tab` cycle focus,
arrow keys move the focused window, `Ctrl+W` closes, `Ctrl+M` minimizes,
`Ctrl+F` zooms, `Ctrl+N` opens Files, `Ctrl+,` opens Settings, and `F3` opens
Spaces. `Shift+arrows` resize a floating window. In Files, unmodified Up/Down
select entries rather than moving the window. Settings actions are reachable
through keyboard menus or search. Focus is visible through the accent border
and title treatment; minimizing transfers focus immediately, and restoring a
zoomed window preserves its zoom state.

A screen reader is not implemented in this prototype. Text is an original 5×7
bitmap font; there is no font scaling or IME support. The high-contrast,
reduced-motion, keyboard-only, and dock-magnification paths are the honest
accessibility scope of this milestone.

## Input

`kernel/src/input.rs` owns IRQ1 and IRQ12. The 8259 PIC unmasks IRQ0, IRQ1, the
IRQ2 cascade, and IRQ12; the IDT installs vectors 32, 33, and 44. The keyboard
handler decodes set-1 make/break scancodes, including E0 dedicated arrows, and
tracks both sides of Ctrl, Alt, Shift, and Super independently. The mouse driver initialises the auxiliary PS/2
port, enables data reporting, and decodes three-byte packets with correct 9-bit
sign extension and inverted Y. Both paths use bounded FIFOs and never call the
desktop from interrupt context. Pointer presses/releases are retained between
paints; after overflow, an atomic final snapshot prevents a stuck drag. Shared
controller output is routed by its source bit and parity/timeout status, not
just the IRQ vector. Mouse probing attempts to re-enable the keyboard on every
exit, including timeout paths.

QEMU and VMware expose legacy PS/2 through the same i8042 interface. If the
controller probe fails, the desktop remains keyboard-only and the boot log says
so; keyboard input is not disabled by the mouse probe.

## Authority and limitations

The desktop still runs at ring 0 in this prototype. It is not a security domain
and must not be presented as one. Its logic is deliberately non-authoritative:
it uses filesystem/portal service adapters and displays live service state. Those
calls are a coding convention: **compromise of ring-0 desktop code is kernel
compromise**, not a contained application failure. Only the pure WM model has no
service access. The technical specification's user-space compositor/service
boundary remains unimplemented; this must not be called a completed microkernel
desktop. See [ADR-016](../decisions/ADR-016-desktop-review-and-uefi-correctness.md).

Not implemented: GPU acceleration, real background blur, HDR, screen capture,
per-application GPU surfaces, a user-space compositor protocol, multiple
displays, persistent preferences, screen readers, IME/text input, window
snapping, and application-provided window content. The current application
views are trusted kernel-drawn views over live service state. Moving the
compositor and application views to ring 3 is follow-up work.

## Validation

`make unittest` covers decoders, browser navigation and malformed data, WM state,
damage bounds, and the actual graphics module against padded/guarded RAM
surfaces in `tests/rendering`. `make iso-test` boots the same ISO with VGA and
QEMU's VMware SVGA adapter, then drives real PS/2 input to test dragging/resizing,
folder navigation/previews, denied vault access, search/menu interactions, and
the cursor-only path. It checks RGB colors and compares partial/full screenshots
while excluding only the changing uptime clock. Logs/screenshots and redraw
timings are in `build/iso-test/<adapter>/`. This is not a VMware Workstation test.

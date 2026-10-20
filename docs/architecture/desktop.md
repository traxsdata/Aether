# Window manager and compositor

The desktop is an original, clean-room implementation of a modern desktop
workflow. Its design vocabulary is informed by publicly described macOS 27
"Golden Gate" principles — refined layered translucency, a global transparency
control, tighter uniform window corners, edge-to-edge sidebars, colourful icons,
stronger depth and separation, and search as a first-class action. No Apple
code, fonts, bitmaps, wallpapers, or icon resources are embedded or traced.
Every palette value, glyph, icon, animation curve, and layout rule in this tree
is defined by the project.

The defining reference is the public high-level description of Golden Gate:
enhanced Liquid Glass readability and a user-controlled opacity slider. Aether
implements those ideas at the level the current software renderer can honestly
support: layered alpha compositing, specular highlights, darkened edges, three
glass levels, and no claim of GPU blur or HDR.

## Layers

```text
aether-wm (kernel/wm)       host-tested policy: geometry, windows, focus,
                            workspaces, menus, dock, Spotlight, animation
kernel/src/desktop.rs       renderer and input adapter (trusted kernel code)
kernel/src/icons.rs         original vector artwork
kernel/src/graphics.rs      double-buffered software compositor
kernel/src/input.rs         IRQ1 keyboard and IRQ12 PS/2 mouse
```

`aether-wm` is `no_std`, forbids `unsafe`, allocates nothing, and has no
framebuffer, device, filesystem, or capability access. It is tested on the host
for focus, z-order, hit testing, workspace transitions, menu navigation,
Spotlight filtering, settings, responsive layout, and animation completion.

## Double-buffered composition

The compositor paints into a static 1920×1200 32-bit back buffer and presents
the completed frame with one linear copy. This removes the tearing visible when
an immediate-mode renderer paints directly into a linear framebuffer: a
screenshot or display never observes a half-drawn window. Modes larger than the
back buffer fall back to direct painting rather than failing to boot.

The back buffer costs 9.2 MiB of kernel BSS. That is deliberate: the prototype
has no general kernel allocator available to the compositor and no GPU, and a
bounded static buffer has simple lifetime and security properties. Presentation
is the only path that writes the real framebuffer.

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
keyboard navigation (`Up`, `Down`, `Enter`, `Escape`).

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
Spaces. Focus is always visible through the accent border and title treatment.

A screen reader is not implemented in this prototype. Text is an original 5×7
bitmap font; there is no font scaling or IME support. The high-contrast,
reduced-motion, keyboard-only, and dock-magnification paths are the honest
accessibility scope of this milestone.

## Input

`kernel/src/input.rs` owns IRQ1 and IRQ12. The 8259 PIC unmasks IRQ0, IRQ1, the
IRQ2 cascade, and IRQ12; the IDT installs vectors 32, 33, and 44. The keyboard
handler decodes set-1 make/break scancodes and captures Ctrl, Alt, Shift, and
Super modifiers with each key. The mouse driver initialises the auxiliary PS/2
port, enables data reporting, and decodes three-byte packets with correct 9-bit
sign extension and inverted Y. Both paths use bounded queues/state and never call
the desktop from interrupt context.

QEMU and VMware expose legacy PS/2 through the same i8042 interface. If the
controller probe fails, the desktop remains keyboard-only and the boot log says
so; keyboard input is not disabled by the mouse probe.

## Authority and limitations

The desktop still runs at ring 0 in this prototype. It is not a security domain
and must not be presented as one. Its logic is deliberately non-authoritative:
it reads directory contents through the filesystem service, displays live
supervisor/application/portal state, and asks the portal for grants. It cannot
mint a file capability or bypass a filesystem root check. The window manager
itself has no service access at all.

Not implemented: GPU acceleration, real background blur, HDR, screen capture,
per-application GPU surfaces, a user-space compositor protocol, multiple
displays, persistent preferences, screen readers, IME/text input, window
snapping, and application-provided window content. The current application
views are trusted kernel-drawn views over live service state. Moving the
compositor and application views to ring 3 is follow-up work.

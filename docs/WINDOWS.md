# Windows, the dock and the menu bar

The window manager and compositor live in `kernel/wm.c`. Everything it
draws (title bars, the dock, the menu bar, menus, the window switcher and
system dialogs) is drawn by the kernel, so an app can't fake or cover it.
The design and artwork are original to æther.

## Using it

| To | Do this |
| --- | --- |
| Close a window | the red light on the left of its title bar, Super+W, or Window > Close |
| Quit an app | Super+Q, or the app menu > Quit |
| Minimize to the dock | the yellow light, Super+M (or Super+Down), or Window > Minimize |
| Restore | click the window in the dock (right of the divider), its app icon, the Window menu, or the switcher |
| Zoom (fill the space between the menu bar and the dock) | the green light, Super+Up, double-click the title bar, or Window > Zoom; do it again to go back |
| Resize | drag any edge or corner of a resizable window |
| Switch windows | hold Super or Alt and press Tab (Shift+Tab goes back, arrows move, Esc cancels); let go to switch |
| Start an app | click its dock icon (an app that is already running comes forward instead), or the æ menu |
| Another window of the same app | the app menu > New *App* Window |
| New terminal | Ctrl+Alt+T |
| Lock the screen | Super+L, or æ > Lock Screen |
| Escape a stuck dialog or app | Ctrl+Alt+Esc (closes the app behind the dialog, or the focused app) |

A light under a dock icon means the app is running. A minimized window
shows in the dock as a small picture with its app's initial in the corner.
The menu bar names the focused app; the æ menu starts system apps and can
minimize or show every window. The right side of the menu bar keeps the
security status and the clock.

Super+<key> combinations always belong to the window manager and never
reach an app.

## Resizable windows (for app authors)

A window keeps the size its app asked for unless the app opts in. Its zoom
light is hollow and its edges can't be dragged until then.

1. After `SYS_WIN_CREATE`, call `SYS_WIN_FLAGS(handle, WIN_FLAG_RESIZABLE,
   min_w, min_h)` with the smallest client size the app can lay out (0 means
   `WIN_MIN_W` x `WIN_MIN_H`).
2. When the user zooms or resizes, the app gets `EV_RESIZE` with the
   requested client size in `x`, `y`.
3. The app calls `SYS_WIN_RESIZE(handle, &win_info)`. The kernel allocates a
   buffer of the new size, copies the old picture into it, maps it at the
   **same address** in place of the old one and fills in `win_info`. The app
   must use the new width, height and pitch from then on (the old size's
   pages beyond the new buffer are unmapped), then redraw and present.

An app that ignores `EV_RESIZE` keeps its old size: nothing moves on screen
until the app takes the new size. Only the latest request is kept.

- C apps: `window_set_resizable()` and `window_resize()` in `user/lib/ui.c`
  (Notes and Files use them).
- Rust: the eframe stand-in opts every egui app in unless its
  `ViewportBuilder` says `resizable: false` (or fixes min = max size), and
  passes `min_inner_size` on. The aether-host backend takes the new size
  itself and hands the app `Event::Resize`. The browser opts in too.
- Settings and App Store draw fixed layouts and stay fixed-size.

## Security notes

- An app can't choose its own new size or position: the kernel only offers
  sizes the user asked for, clamped to the screen, `WIN_MAX_*` and the app's
  minimum, and `SYS_WIN_RESIZE` with nothing proposed just reports the
  current size.
- Resizing reuses the window's address-space slot (64 MiB, larger than any
  `WIN_MAX_W` x `WIN_MAX_H` buffer), so it doesn't use up the per-process
  limit of 64 window slots. The old buffer is unmapped before its pages are
  freed; if a page table can't be allocated, the old buffer is mapped back
  (its tables were never freed) and the call fails with `E_NOMEM`.
- Window limits are unchanged: 16 windows in all, 4 per process. Menus,
  the switcher and the dock's minimized area are bounded by those 16
  windows (the dock shows only as many minimized windows as fit; the rest
  stay reachable from the switcher and the Window menu).
- Input still goes only to the focused window. While a menu or the switcher
  is open, no keys reach any app; clicks outside a menu only close it.
- An app's **first** window may take focus when the user started it. A later
  window from a background app goes to the bottom of the stack and does not
  receive the keyboard until the user clicks it, so a package cannot steal
  keystrokes by opening a window.
- **Super+L** locks the desktop before any dialog sees the key. While locked,
  the compositor draws only the wallpaper and the lock panel; apps keep
  running but receive no key or mouse events. `Ctrl+Alt+Esc` is the emergency
  close path and also works while a system dialog is modal.
- `SYS_WIN_FLAGS` and `SYS_WIN_RESIZE` act only on the caller's own windows
  (looked up by owner and handle) and are on the untrusted apps' allowlist,
  since they can do nothing beyond the window the app already has.
- The kernel log records window actions by app name and window number, not
  by title, so file names in titles don't leak to apps that read the log.

## Tests

`tests/windows.txt` (run with `AETHER_DISK=none tools/qemu-drive.py
tests/windows.txt`) zooms, unzooms, resizes by the corner, minimizes,
restores from the Window menu, switches with Super+Tab and quits with
Super+Q, checking each step in the serial log and taking screenshots.
`tests/mouse.txt` closes a window with its red light.

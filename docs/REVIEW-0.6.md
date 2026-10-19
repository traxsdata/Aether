# Review of the 0.6 work

This review covers the core subsystems touched by 0.6: the program table and
app installation, capabilities, the window manager, the Linux layer, network
configuration and the Rust runner. It also records what was fixed and what is
still deliberately missing.

## What 0.6 changes

- Calculator, Notes, Files, Probe and Terminal stay native C programs in the
  kernel image.
- Settings (network, Wi-Fi status, firewall, privacy, about) is a new Rust
  system package.
- App Store is a new Rust system package that installs and updates packages
  from GitHub over the internet.
- Installed packages persist in `/system/apps`; updates never restart a
  running app, and a pending update is shown in its title bar.
- The kernel learns `CAP_FIREWALL`, `CAP_APPS` and `CAP_CONFIG`, plus the
  app/settings/OS-info system calls.
- Static IPv4 configuration, saved in `/system/settings`.

## Findings and fixes

### 1. Installed apps are a new trust boundary

An installed package is attacker-controlled input. The new path enforces:

- `app_check_header()` validates the package header, name, size, SHA-256 and
  the ELF image before anything is written.
- `elf_check()` repeats the loader's rules: no kernel addresses, no W+X, no
  interpreter, an entry point inside an executable segment.
- User packages are always `TRUST_UNTRUSTED`; `program_add()` masks their
  capabilities to window / picker / storage / network.
- System names are reserved. App Store cannot install, replace or remove
  Terminal, Calculator, Notes, Files, Probe, Settings or itself.
- Packages are stored in `/system/apps`, which `vfs.c` refuses to resolve for
  any app.
- A package is re-hashed on first start, so a damaged stored package is
  refused.

### 2. App images no longer fit in the kernel heap

Packages can be tens of megabytes; the kernel heap is 16 MiB. Installed
packages (and `SYS_APP_INSTALL`'s temporary copy) use contiguous physical
pages, not `kmalloc`. The program table owns those pages and frees them on
update/remove. This also keeps the package bytes out of the heap allocator's
fragile path.

### 3. Updates and running processes

Replacing a program frees the old package image, but a running process has
already copied its ELF into its own address space, so it keeps running safely.
The kernel marks matching live processes `update_pending`; the window manager
draws an `update pending` chip in the title bar. Removal of a running app is
refused with `E_BUSY`.

### 4. Window pages were freed while still mapped

`destroy_window()` freed the window's physical pages without removing the
owner's PTEs. The normal close path kills the process first, so the address
space is destroyed immediately afterwards, but the code was one API away from
a user-writable use-after-free. It now unmaps and prunes the window range
before freeing the pages.

### 5. Capabilities can no longer be raised by a caller

`proc_spawn()` now does `caps & prog->caps`. `SYS_SPAWN` already intersected
with the caller, but the direct kernel path (WM launch, tests) could pass a
capability the manifest didn't grant. The program manifest is the ceiling
everywhere.

### 6. Firewall management was tied to `TRUST_SYSTEM`

Settings needs to read and change rules without being a full system process.
`SYS_FIREWALL` now checks `CAP_FIREWALL`. Terminal keeps the capability;
ordinary apps do not.

### 7. Linux `ppoll` mishandled a bad timeout pointer

`NR_ppoll` turned a failed `read_timespec()` into a zero timeout instead of
`-EFAULT`. Fixed; an app can no longer turn an invalid pointer into a
busy-poll.

### 8. `SYS_WIN_TITLE` lacked a capability check

It was harmless because the window handle is checked against the owner, but it
now requires `CAP_WINDOW` like the other window calls.

### 9. Linux programs can receive an argument

`linux_setup()` now builds `argv[1]` from the spawn argument. A Linux-ABI app can use it to choose a mode; ordinary apps still see
`argc = 1`.

### 10. Ctrl+V reached apps with no text clipboard

The runner turned Ctrl+C/X/V into egui clipboard events and returned early.
With an empty text clipboard, Ctrl+V produced no event at all, so an app with
its own clipboard could not act on it. The runner now reports Ctrl+V as a key
when the text clipboard is empty.

### 11. Settings must not clobber an address being typed

The page refreshes every two seconds. It now leaves the static address fields
alone while the user has edited them, and only repopulates them after Apply or
when they are untouched.

### 12. System apps stay clean-room

Settings and App Store no longer link egui, epaint, `image`, `log` or bundled
fonts. `runner/system-ui` is a small in-tree Rust binding to `common/gfx.c`,
the same anti-aliased rectangles and Facet text the native apps use. Its
dependency tree is only `aether-host` (and, for App Store, the in-tree
`webnet`). Third-party code remains confined to the untrusted Craft apps.

### 13. App Store least privilege and install checks

- `CAP_FILES` is gone. OS images are saved through the system picker's save
  handle, so the network-facing app can't read or write all of `/home`.
- A missing or non-`sha256:` release digest is refused for catalog app and
  OS downloads; the old code failed open.
- Installing a package from a file first tries without replacement. If the
  name exists, the UI shows the old and new `source` strings and asks before
  replacing; a replacement inherits the old app's private folder.
- OS images are labelled unverified/signed-less in the UI. A digest proves
  integrity, not publisher authenticity, until releases are signed.
- `settings_set` refuses control characters, so a value can't inject extra
  `key=value` lines into `/system/settings`.

## Test evidence

Run on this machine (QEMU TCG, no KVM):

- `make unittest`: **54 kernel tests and 66 terminal tests, 0 failed.**
  The new kernel tests cover the app store install/replace/remove path and
  static network configuration.
- `cargo test -p system-ui -p aether-host -p eframe -p installer -p webnet -p browser`:
  **170 passed, 0 failed** (main's browser script tests included).
- `AETHER_DISK=none tools/qemu-drive.py tests/desktop.txt`: pass (sectest,
  terminal, Calculator 12×3.5 = 42).
- `AETHER_DISK=none tools/qemu-drive.py tests/mouse.txt`: pass (dock,
  Calculator 7×6 = 42, drag, close).
- `AETHER_DISK=none tools/qemu-drive.py tests/explorer.txt`: pass (browse,
  preview, new folder, rename, copy, cut, delete refusal, label raise/lower,
  Secret preview and viewer).
- `tests/storage.txt`, `tests/files.txt`, `tests/notes.txt` on a disk:
  pass with the original native Notes/explorer tests.
- `AETHER_DISK=none tools/qemu-drive.py tests/settings.txt`: pass (all five
  pages).
- Manual QEMU smoke runs for Settings and App Store; App Store reached
  `api.github.com` through the firewall and parsed the latest ArtCraft
  release (`artcraft-v0.41.0`), correctly reporting that it has no `.app`
  asset yet.

Tests not run in this environment: Craft app builds (the repositories' current
releases have no æther packages), Secure Boot signing, VMware, real hardware.

## Deliberate gaps in 0.6

- **Wi-Fi**: no driver. Settings says so; it does not fake a scan.
- **OS update installation**: App Store requires the release's SHA-256 and
  labels OS images as unsigned/unverified, but writing the boot medium and
  rebooting is manual. App updates are live.
- **Craft app availability**: the installer supports
  `*-aether-x86_64.app` release assets. The current upstream releases don't
  publish them, so the UI reports that instead of installing an incompatible
  desktop binary.
- **Craft app feature parity**: the native Files/Notes/Calculator apps keep
  their 0.5 behavior; the 0.6 dock adds Settings and App Store.
- **App package authenticity**: the SHA-256 is integrity, not a signature.
  HTTPS and the GitHub release are the authenticity boundary; signed packages
  remain future work (as does Secure Boot).
- **Atomic updates**: replacing a package writes the new file and then swaps
  the program table entry. A crash between the two leaves a valid package on
  disk that is picked up at next boot; a crash during the write leaves a
  truncated file, which `appstore_init()` refuses and logs.

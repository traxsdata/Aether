# æther 1.x plan

1.0 is the security-and-daily-driver baseline: session lock, per-thread system
dialogs, DLP metadata fixes, AFS/GPT hardening, network consent fixes and a
signed system image that pins its bootloader.

1.x is about making the desktop useful beyond the apps that ship with it,
without giving up the security model. The first workstream is a **contained
Linux application runtime**: a self-contained app bundle gets its own
read-only filesystem and writable overlay, not the host's files.

## Rules that do not change

Every milestone keeps these invariants:

- kernel memory is supervisor-only; apps cannot read or write it;
- W^X everywhere, and untrusted apps cannot create executable memory;
- capabilities are fixed at spawn and can only shrink;
- apps cannot name host files without a user grant;
- outgoing connections are per-app, per-host and checked by the firewall;
- DLP labels follow data across reads, writes, children and the clipboard;
- nothing runs from an unverified image;
- a child cannot leave its parent's namespace or gain a capability;
- the kernel stays small enough to audit. A Linux runtime is not a Linux
  kernel.

## 1.1 Contained Linux apps (first workstream)

Goal: run one signed, self-contained bundle that can spawn its own child
tools and reach exactly one approved network destination.

### 1.1a Namespace foundation — **done**

- `struct proc.root_ino` / `root_readonly`, inherited by children.
- VFS resolves `/` from the process root, refuses `..` escapes and all writes
  through a read-only root.
- Kernel tests: a contained root cannot see host names, reach outside, or
  write.
- Writable overlay: an encrypted upper layer (`overlay_ino`) mirrors the
  read-only tree. A write to an image file copies it up first; new files are
  created in the overlay. The image blocks are never modified.
- Tests check copy-up, overlay lookup preference and that the image content is
  unchanged. Directory listings/whiteouts and delete/rename semantics are the
  next part of 1.1a.

### 1.1b Bundle format

- A signed bundle with a manifest, a read-only rootfs image and an optional
  runtime.
- Manifest: name, entry point, ABI, capabilities, allowed network hosts,
  minimum Aether version, rootfs hash.
- Installer writes the image outside the host root; App Store shows the
  publisher and the capabilities before install.
- Update replaces the read-only image atomically and keeps the overlay.

Exit: a bundle installs, updates, and refuses a changed rootfs or manifest.

### 1.1c Loader and libraries

- `PT_INTERP` support: the loader, `ld.so` and shared libraries come from the
  bundle image, never from the host.
- `AT_PHDR`, `AT_BASE`, `AT_ENTRY`, `AT_EXECFN` and a rooted library search
  path.
- Static musl bundles remain the low-risk path; dynamic glibc is supported
  only from the image.

Exit: a dynamically linked hello-world bundle runs; a bundle that names a host
library is refused.

### 1.1d Sandboxed fork/exec

- `fork`/`exec` inside one bundle; no exec outside it.
- Child inherits caps, DLP taint, root and overlay; a child cannot widen them.
- Exec of an overlay-written file is refused unless the manifest lists that
  tool and its hash.
- Process tree limits, no ptrace, no setuid.

Exit: a bundle runs `git`/`rg`-style child tools and cannot exec `/system`,
`/home` or a modified overlay binary.

### 1.1e Runtime surface

- PTY/raw mode and the terminal signals Claude Code and shells expect.
- `eventfd`, `timerfd`, `/proc/self`, `getdents64`, `readlink`, `statx` and
  Unix sockets as needed.
- Per-app DNS/network policy, with a manifest host allowlist on top of the
  existing firewall.

Exit: one real CLI bundle passes a scripted end-to-end test.

## 1.2 Claude Code

Goal: Claude Code runs as a signed, untrusted, self-contained app.

- Bundle Node/Bun, Claude Code and the tools it shells out to.
- Grant one workspace through the picker; no ambient home directory.
- Network allowlist: the API host only (plus an explicit user override).
- Raise the package and memory limits for the bundle after measuring; keep
  the per-app quota and the global untrusted budget.
- Dedicated window/terminal with raw mode, resize and scrollback.
- Show the app's capabilities, workspace and network destinations in one
  place; let the user revoke them.

Exit: a scripted session starts the bundle, reads a granted file, runs a
child tool, makes one HTTPS request and cannot read anything else.

## 1.3 Daily-driver finish

Ordered by user pain, not novelty:

- **Shutdown and suspend** (ACPI; restart already exists), lid/backlight where
  the platform exposes it.
- **Change disk passphrase** from Settings > Security, with a kernel-verified
  old passphrase.
- **Trash and undo** for Files; never delete permanently without a confirm.
- **Label-aware clipboard** shared by Notes and Files, cleared on lock, with
  a capability so background apps cannot poll it.
- **Notifications** (`SYS_NOTIFY`) with app identity, rate limits and a
  history.
- **Responsive Settings/App Store** layouts for 1024x600 and larger; current
  fixed layouts clip.
- **First-run setup**: network check, clock, theme, a short tour, and the
  option to install the App Store apps.
- **App data management**: show size/label, delete data on uninstall when the
  user asks, keep it on update.

## 1.4 Trust and release

- **Signed app packages** and a publisher key model, so the App Store verifies
  authenticity, not only integrity.
- **Secure Boot** signing of loader and kernel, plus a documented key
  rotation path.
- **Rollback and anti-downgrade** tied to signed boot-control state, not only
  CRC32.
- Reproducible image builds and a second-person review checklist for release
  keys.

## 1.5 Hardware (coordinate with the driver work)

- Wi-Fi, audio, battery/lid, cameras, Bluetooth and USB storage.
- Suspend/resume and thermal policy.
- The existing USB/I2C/NVMe slice (PR #19) is the base.

## 2.0 Steam-class software

Not a syscall-table gap. Pick one:

- native game ports to æther;
- a Steam Link/remote-play client, which keeps Steam on another machine;
- a Linux VM with hardware virtualization, explicitly outside the small
  trusted kernel and with its own device model.

Do not promise "SteamOS in a window" until one of those has a working
prototype.

## Sequencing

1. 1.1a overlay + 1.1b bundle format (smallest end-to-end proof).
2. 1.1c loader, then 1.1d fork/exec, then 1.1e PTY.
3. 1.2 Claude Code bundle.
4. 1.3 items in parallel when they do not touch the bundle core.
5. 1.4 before calling the App Store trustworthy for third-party software.
6. Hardware follows the driver PRs; 2.0 is a separate decision.

## Evidence for every milestone

- A kernel unit test for each namespace/capability invariant.
- A QEMU scripted test for each user-visible flow (`tools/qemu-drive.py`).
- A negative test: try to escape the root, exec outside the bundle, write the
  image, read the host, or connect to an unapproved host.
- A short review note in `docs/REVIEW-*.md` with what was not tested.

# The Rust runner

æther runs ordinary Rust programs built for Linux, unmodified, as apps.
That's how the Craft apps (PhotoCraft, GridCraft and the rest) run without
being rewritten: they're built from their own GitHub sources for the
`x86_64-unknown-linux-musl` target and packaged for æther.

0.6 also has two of the OS's own apps in Rust: Settings and App Store in
`runner/apps/`. They do not use eframe or egui; they draw through the in-tree
`system-ui` binding to the kernel apps' own C graphics and Facet font. They
are built into system packages by `tools/build-rust-apps.sh`; the kernel's
manifest, not the package header, decides their capabilities. Calculator,
Notes, Files, Probe and Terminal stay native C programs in the kernel image. The same runner still executes
downloaded Craft apps as untrusted packages.

There are three parts:

1. **A Linux system call layer** in the kernel (`kernel/linux.c`). Static
   Linux programs make Linux system calls; æther answers them with its own
   mechanisms and its own rules.
2. **App packages** (`tools/mkapp.py`): the program plus a header with its
   name, version, source and memory limit, checked by the kernel at boot.
3. **An eframe stand-in** (`runner/eframe`): the Craft apps draw with egui
   through eframe, which normally needs a GPU. æther's version of the eframe
   crate draws egui in software into an æther window, so no app code changes.

## How a call is handled

A program built for Linux keeps calling `read`, `mmap`, `clone`, `futex` and
so on. Each one is mapped onto what æther already does, never around it:

| Linux | æther |
|---|---|
| `open("/data/notes.txt")`, or a relative path | the app's private folder, `/apps/<name>` (needs CAP_STORAGE) |
| any other absolute path (`/etc`, `/home`, `/proc`) | "no such file": untrusted apps can't name your files |
| a file you chose in the picker | arrives as an already-open file descriptor |
| `read` / `write` on files | the same DLP rules: reading taints the app, writing raises the file's label |
| `write` to stdout / stderr | the system log, rate-limited, and dropped once the app has read Confidential data |
| `mmap`, `brk`, thread stacks | reserved address space; pages are made when first touched and count against the app's memory limit |
| `mprotect` / `mmap` with execute | refused: no memory is ever writable and executable, and untrusted apps can't make new code |
| `clone` (threads) | threads in the same process; `futex` for locks |
| `fork`, `execve`, `vfork` | not supported: an app can't start other programs |
| `socket` | not supported; apps with `CAP_NET` use æther's native net calls (`0x1000 + 34..`), which the firewall checks |
| `kill` on another process | refused |
| signals | handlers are accepted but never called; a crash ends the app |

Anything not implemented returns `ENOSYS` and is logged once. Native æther
calls (windows, events, the file picker) are reachable at `0x1000 + N`.

## Packages

```sh
tools/mkapp.py --name gridcraft --title GridCraft --version 1.0.0 \
    --source github.com/storytold/gridcraft --quota 1024 \
    target/x86_64-unknown-linux-musl/release/gridcraft build/apps/gridcraft.app
make
```

`make` copies everything in `build/apps/` to `\apps` on the ISO, and the boot
loader reads them into memory. Before an app is listed the kernel checks the
header, the program's SHA-256, the ELF image and its name (it can't replace a
system program). **Whatever the header asks for, an installed app is
untrusted** and gets at most a window, the file picker, a private folder and
the network. Apps with a window appear in the dock; any app can be started
from the terminal with `run <name>`.

0.6 adds runtime installation: App Store hands a TLS-delivered package to the
kernel, which repeats the checks and stores it in `/system/apps`. See
[APPS.md](APPS.md).

Integrity on the boot partition isn't authenticity: until Secure Boot
signing (and signed updates in 0.5), someone who can write to the boot
partition can replace an app. The sandbox still holds for whatever runs.

## Testing

`tools/build-linuxtest.sh` builds `runner/linuxtest`, a plain Rust program
that checks threads, locks, channels, condition variables, thread-local
storage, sleeping, the clock, files, large allocations, growing vectors and
panics, then tries ten things the sandbox must refuse. Then:

```sh
make && AETHER_DISK=none tools/qemu-drive.py tests/linux.txt
```

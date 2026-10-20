# Linux application compatibility

**Document ID:** AETHER-LINUX-001
**Status:** Import and static Linux ELF execution stages only; no dynamic
runtime and no compatibility claim
**Related:** [ADR-017](../decisions/ADR-017-linux-app-runtime.md), technical
specification §7 (compatibility)

## Purpose

This document defines how Aether will run unmodified Linux x86-64 applications
distributed as AppImages. It follows the same evidence discipline as the
[Windows compatibility plan](windows-compatibility.md): an application is not
called compatible until a documented runtime runs it on a documented platform
and the required host interfaces are mapped to tested Aether interfaces.

The first target is GridCraft 0.8.0. It is a useful stress case because it is a
real GUI application with a large Rust dependency graph, dynamic glibc linkage,
threads, Wayland/X11, Vulkan, D-Bus, and file dialogs.

## Non-claims

- Aether does not contain Linux, glibc, Wayland, Mesa, Wine, or a hypervisor.
- The AppImage launcher is not executed. Aether imports the SquashFS payload
  into a private root; FUSE is not required.
- A private AppImage root is not a security boundary by itself. The Aether
  security domain, filesystem service, and portal enforce isolation.
- No GridCraft record may be classified above `unsupported` until the gates
  below pass.

## Tested artifact

| Field | Value |
|---|---|
| Name | GridCraft |
| Version | 0.8.0 |
| Asset | `gridcraft-0.8.0-linux-x86_64.AppImage` |
| Size | 19,024,376 bytes |
| SHA-256 | `942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77` |
| Container | AppImage type 2; SquashFS 4.0, zstd, 128 KiB blocks, 46 inodes |
| SquashFS offset | 944,632 |
| SquashFS bytes | 18,076,825 |
| Payload | `usr/bin/gridcraft` (GUI), `usr/bin/gridcraft-cli` (headless) |
| Runtime ELF | static-pie AppImage launcher |
| Payload linkage | dynamic: `ld-linux-x86-64.so.2`, `libc.so.6` (up to GLIBC_2.35), `libm.so.6`, `libgcc_s.so.1` |
| GUI stack | `winit` 0.30 (Wayland/X11), `wgpu`/Vulkan, D-Bus, XDG desktop portal, `arboard` clipboard |
| Test date | 2026-10-20 |
| Classification | `unsupported` (no runtime) |

`make appimage-test` exercises the container parser against a synthetic image.
`appimage-inspect` reports the fields above from a real AppImage without
executing it. It accepts an optional pinned SHA-256 digest and refuses a
mismatch; publisher signatures and the trust store are still the runtime
manager's job.

## Required host facilities

The status column follows the Windows plan's rule: an interface counts as
implemented only when a tested Aether interface exists and the runtime uses it.
The Gate 1 inventory expands this summary into per-facility owners and test
cases in [the Linux host-interface inventory](linux-host-interface-inventory.md).

| Facility | What the runtime needs | Aether interface to define | Current status |
|---|---|---|---|
| AppImage import | Validate ELF/SquashFS bounds, digest, publisher; unpack read-only root | AppImage importer and package trust policy | **Container inspection, pinned digest, signed AppImage and runtime-rootfs records, all-compression SquashFS extraction, host packaging, and embedded signed-rootfs in-OS import implemented**; the populated glibc/Mesa rootfs and user-supplied AppImage import are missing |
| ELF and libraries | `PT_INTERP`, `PT_LOAD`, `PT_DYNAMIC`, relocations, TLS, `ld.so` | Linux address-space loader and shared-library service | Partial: static `PT_LOAD` execution works; interpreter and shared libraries missing |
| Process creation | `clone`/`fork`/`execve`, wait, exit, process groups | User-space process API over the kernel task model | Partial: one fixed boot process, no creation |
| Threads and TLS | `clone` with thread flags, `set_tid_address`, `arch_prctl`, `rseq` | Thread API and per-thread kernel state | Partial: `arch_prctl` FS base and context-switched TLS pointer; no threads |
| Memory | `mmap`/`munmap`/`mprotect`/`brk`, file mappings, shared memory | Memory-object capabilities and user mapping syscalls | Partial: runtime frame allocation, per-task VMA table, file-backed `mmap`, `munmap` reclaim, `mprotect`, and growing `brk`; no `mremap`, page-fault handling, or shared memory |
| Synchronization | `futex`, robust lists, condition variables, `eventfd` | Futex wait/wake primitive and timeout service | Partial: single-task `FUTEX_WAIT`/`WAKE` no-ops; no wait queue |
| Signals | Handler delivery, `sigaltstack`, `rt_sigreturn`, fault-to-signal | Fault delivery and signal runtime | Not implemented |
| Time | `clock_gettime`, `nanosleep`, timers, `poll`/`epoll` | High-resolution time and wait services | Partial: `clock_gettime` from the 100 Hz tick; no sleeps/timers |
| Files | `openat`, `statx`, `readlink`, `rename`, `fsync`, inotify | Filesystem service API and portal grants | Partial: read-only embedded root plus a bounded per-task `/tmp` overlay; `openat`/`read`/`pread64`/`write`/`close`/`lseek`/`access`; no stat, directories, persistence, or portal |
| Dynamic files | `/proc/self/exe`, `/proc/self/maps`, `/dev/urandom`, `/dev/null` | Bounded Linux runtime pseudo-filesystem | Not implemented |
| Wayland/X11 | `wl_display`, shm buffers, xdg-shell, seat, clipboard | Wayland compositor service over the Aether compositor | Not implemented |
| Vulkan/GL | Vulkan loader/ICD or EGL/GLES; swapchain to a surface | GPU service or bundled software renderer | Not implemented; no GPU service |
| Input | Pointer, keyboard, focus, cursor, keymap | Compositor input protocol | Partial: PS/2 keyboard and mouse in the desktop |
| D-Bus / XDG portal | Session bus, file chooser portal | IPC bridge and portal integration | Not implemented |
| Audio | ALSA/PipeWire | Audio service | Not implemented; not required for first milestone |
| Network | Sockets, DNS | Network service and capability policy | Not implemented; denied by default |

## Runtime architecture

```text
AppImage file
    |
    v
[ import: validate container, digest, publisher ]
    |
    v
private read-only root + private writable overlay
    |
    v
[ linuxd: Linux ABI runtime ] -- IPC --> [ fsd ] [ portal ] [ waylandd ] [ Vulkan/GL ]
    |                                          |
    +----------- narrow kernel primitives -----+--> [ compositor ] [ input ]
```

- `linuxd` is a user-space service. It owns Linux ABI emulation, the ELF loader,
  library search paths, environment, `/proc` synthesis, and per-application
  configuration.
- The kernel provides only primitives that cannot be emulated safely in user
  space: address-space mapping, thread creation and scheduling, futex
  wait/wake, fault delivery, and the syscall trap. It does not gain a Linux
  filesystem, network stack, device drivers, or graphics stack.
- `waylandd` exposes the Wayland subset used by `winit`/`egui`. It receives
  shared-memory buffers, validates their geometry, and asks the compositor to
  present them in the application's window.
- Graphics is the highest-risk component. The first GUI attempt will use a
  bundled Mesa software Vulkan (lavapipe) or software GL path, selected
  explicitly, with no host GPU passthrough. A GPU service is a later
  optimization.
- File access uses `fsd`/portal handles. The Linux runtime never receives an
  ambient `/home`, `/`, block device, or network socket.

## Gates

### Gate 0 — AppImage import inspection

Parse the container without executing it, validate ELF/SquashFS structure and
bounds, report compression and offsets, and fail closed on malformed input.

Signed import records (`apps/linux/appimage/src/manifest.rs`) bind identity,
version, runtime, digest, and publisher to an Ed25519 signature checked against
a trust store; `appimage-manifest` signs and verifies both `linux-appimage` and
`linux-rootfs` artifacts. `tools/build-runtime-rootfs.sh` packages a root
directory as zstd SquashFS and signs it.

**Evidence:** `apps/linux/appimage` validates the container and verifies pinned
digests and signed import records; `apps/linux/squashfs` reads
zstd/gzip/lz4/lzo/xz/lzma SquashFS metadata, file data, fragments, directories,
and symlinks, with extraction limits, symlink/hardlink policy, and a host
`squashfs-extract` tool. It extracts all 16 files (48,350,419 bytes) and two
symlinks from the tested AppImage. Runtime-rootfs packaging/signing and in-OS
import of an embedded signed rootfs are implemented; the populated glibc/Mesa
rootfs and user-supplied AppImage import remain Gate 2 work.

### Gate 1 — Host-interface inventory

Map every facility the pinned application and runtime need to an Aether
interface, with owners and test cases.

**Evidence:** [the Linux host-interface inventory](linux-host-interface-inventory.md)
maps 32 facilities from the pinned GUI and CLI payloads to an interface, owner,
and test ID, with decisions for X11, D-Bus, process spawning, clipboard, paths,
locale, GPU, and network. **Met:** 2 facilities are existing, 15 partial, 10
missing, 2 deliberately denied, and 3 deferred. The inventory is complete; the
missing interfaces are Gate 2 work and their absence does not invalidate the
mapping. Do not claim compatibility.

The remaining gap is implementation, not planning. Gate 2 proceeds from the
inventory's owner and test IDs in dependency order.

### Gate 2 — Interface mapping and prototype

Implement the narrow kernel primitives and the `linuxd` ABI service. Run static
Linux test binaries first, then dynamically linked glibc binaries. Every
syscall and pointer is validated; unsupported calls fail with `ENOSYS`, never
with ambient authority.

**Evidence:** boot transcript, syscall coverage tests, denial tests. **Partial:**
a dynamically linked test image is mapped through `PT_INTERP` into an `ET_DYN`
address space, receives argc/argv/envp/auxv, and hands control from a test
interpreter to the main entry (`AETHER_LINUX_DYN_INTERP_OK`,
`AETHER_LINUX_DYN_MAIN_OK`, `AETHER_LINUX_DYN_OK`). The opt-in `make
glibc-test` target builds a runtime rootfs from the host glibc, resolves the
real `PT_INTERP` path through that rootfs, and runs a dynamically linked C
hello through `ld.so`, relocations, and shared `libc` (`AETHER_GLIBC_OK`). This
is a host-ABI smoke test, not the pinned production runtime. File metadata,
`readlink`, `fcntl`, descriptor duplication, directory enumeration, and a
bounded pipe with non-blocking `poll` are also exercised
(`AETHER_LINUX_FILEMETA_OK`, `AETHER_LINUX_DIR_OK`,
`AETHER_LINUX_PIPE_OK`, `AETHER_LINUX_EVENT_OK`). A thread is created with
`clone(CLONE_VM|CLONE_THREAD)`, synchronizes through `FUTEX_WAIT`/`FUTEX_WAKE`,
and joins through `CLONE_CHILD_CLEARTID` (`AETHER_LINUX_THREAD_OK`). `mremap`
grows and moves a mapping while preserving contents (`AETHER_LINUX_MREMAP_OK`).
A SIGUSR1 handler is installed, raised, run, and returned from through
`rt_sigreturn` (`AETHER_LINUX_SIGNAL_OK`). A child process is forked and its
exit status is reaped through `wait4` (`AETHER_LINUX_FORK_OK`), then the
process replaces itself with `/usr/bin/exec` through `execve`
(`AETHER_LINUX_EXECVE_OK`). A static freestanding Linux ELF runs at CPL3,
exercises anonymous and
file-backed `mmap`, `munmap` and fixed-address reuse, `mprotect`, growing
`brk`, FS-based TLS, `openat`/`read`/`close` against a validated read-only
root, create/write/read through the `/tmp` overlay, `write`, and `exit`. It
produces `AETHER_LINUX_ROOT_OK`, `AETHER_LINUX_OVERLAY_OK`,
`AETHER_LINUX_VMA_OK`, plus `AETHER_LINUX_OK`; a writable+executable mapping
and a W+X `mprotect` are denied. Frames are allocated at `mmap`/`brk` time and
returned on `munmap` and task exit. The dynamic loader, threads, real futex
waiting, signals, `mremap`, page-fault handling, and the user-space `linuxd`
protocol remain missing.

### Gate 3 — Headless GridCraft

Import the AppImage and run the bundled `gridcraft-cli` unmodified
(`gridcraft-cli eval '=1+1'` and a CSV round trip) in an isolated domain with a
read-only app root and a private overlay.

**Evidence:** exact AppImage digest, command transcript, file-portal denial
tests, and a compatibility record with named tests.

### Gate 4 — Windowed GridCraft

Run `usr/bin/gridcraft` with the Wayland bridge and software renderer, present
its window in the Aether compositor, accept keyboard and pointer input, and
open/save a workbook through the portal.

**Evidence:** screenshot or pixel checksum, input trace, portal grant and
revocation tests, and a compatibility record no better than the suite supports.

## Security acceptance criteria

- The importer validates container bounds before reading a superblock or
  handing any offset to a decompressor.
- The application root is read-only; writes go to a per-application overlay.
- User files are reachable only through portal grants or scoped handles.
- Network, audio, and devices are denied unless explicitly granted.
- A crash in the Linux runtime terminates only that application domain.
- A malformed ELF, library, or Wayland message cannot escape the runtime
  address space or corrupt another application.
- Runtime updates and AppImages are digest- and publisher-verified before use.
- No compatibility record is upgraded without a completed run and named tests.

## Current blockers

1. No pinned production glibc/Mesa/Wayland runtime rootfs; the current dynamic
   proof packages the host glibc and runs a hello, not GridCraft.
2. No user-space process creation, threads, real futex queues, or signals.
3. No `mremap`, page-fault handling, or shared mappings; every mapping is
   eager and private.
4. No populated and pinned glibc/Mesa/Wayland runtime rootfs.
5. No `linuxd` service, Wayland service, or software renderer.
6. No D-Bus broker or XDG portal bridge for the Linux domain.
7. No persistent storage; the current RAM disk is volatile.
8. No user-supplied AppImage import or Linux runtime package channel.

These are ordered dependencies, not independent bugs. Gate 3 does not require
graphics; Gate 4 does. Owners and test IDs are in the
[host-interface inventory](linux-host-interface-inventory.md).

## References

- AppImage specification: <https://docs.appimage.org/reference/specification.html>
- SquashFS 4.0 on-disk format: <https://dr-emann.github.io/squashfs/>
- Wayland protocol: <https://wayland.freedesktop.org/docs/html/>
- Vulkan and lavapipe: <https://docs.mesa3d.org/drivers/llvmpipe.html>
- GridCraft: <https://github.com/storytold/gridcraft>

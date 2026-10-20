# Linux application compatibility

**Document ID:** AETHER-LINUX-001
**Status:** Gate 2 prototype is partial; no application compatibility claim
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
| AppImage import | Validate ELF/SquashFS bounds, digest, publisher; unpack read-only root | AppImage importer and package trust policy | **Container inspection, pinned digest, signed AppImage and runtime-rootfs records, all-compression SquashFS extraction, host packaging, pinned host-glibc root profile, embedded signed-rootfs in-OS import, and loader-staged signed AppImage import implemented**; the populated Mesa/Wayland rootfs, a cross-host reproducible pin, and user-selected AppImage import are missing |
| ELF and libraries | `PT_INTERP`, `PT_LOAD`, `PT_DYNAMIC`, relocations, TLS, `ld.so` | Linux address-space loader and shared-library service | Partial: static and dynamic exec fixtures propagate bounded argv/envp; a `PT_INTERP` fixture, opt-in host-glibc hello, and a host-derived pinned glibc/loader profile run; Mesa/Wayland libraries and a cross-host reproducible pin remain missing |
| Process creation | `clone`/`fork`/`execve`, wait, exit, process groups | User-space process API over the kernel task model | Partial: eager-copy fork, blocking `wait4` with nonchild/selector checks, bounded same-session process groups with `setpgid`/`getpgid`/`getsid`/`setsid` validation, static/dynamic exec, and bounded `clone3` thread creation; stopped/continued waits and group selectors return `ENOSYS`/`EINVAL` |
| Threads and TLS | `clone` with thread flags, `set_tid_address`, `arch_prctl`, `rseq` | Thread API and per-thread kernel state | Partial: `clone` and `clone3` threads, per-thread stacks/TLS, clear-child-tid join, and bounded `rseq` registration/validation (CPU id always 0, no abort notifications); detach and affinity remain |
| Memory | `mmap`/`munmap`/`mprotect`/`brk`, file mappings, shared memory | Memory-object capabilities and user mapping syscalls | Partial: bounded demand paging for private anonymous mappings; `MAP_SHARED|MAP_ANONYMOUS` uses a reference-counted physical-frame registry, so contents survive `fork` and are visible to parent and child while private mappings stay private. A bounded in-memory page cache backs read-shared file mappings keyed by rootfs/overlay inode and page offset, including aliasing across unrelated processes; writes are never persisted to the read-only root and `msync` returns `EOPNOTSUPP`. Shared mappings are limited to 16 VMA entries and 256 physical pages per application domain, with `ENOMEM` on exhaustion. `mremap` of a shared mapping returns `EOPNOTSUPP`. |
| Synchronization | `futex`, robust lists, condition variables, `eventfd` | Futex wait/wake primitive and timeout service | Partial: futex wait/wake queues, timeout (`ETIMEDOUT`), clear-child-tid join, robust-futex registration and exit-time `FUTEX_OWNER_DIED` wakeup for the exact 24-byte x86-64 head layout, and eventfd counters shared by threads in one tgid; condition variables, semaphores, and other robust-list layouts remain |
| Signals | Handler delivery, `sigaltstack`, `rt_sigreturn`, fault-to-signal | Fault delivery and signal runtime | Partial: x86-64 `rt_sigframe` with `siginfo_t`/`ucontext_t`, `SA_SIGINFO`, `SA_ONSTACK` with fallback, GPR/mask restore, `SA_RESTART` for `wait4`, self/group and cross-process SIGUSR1 handlers, and default `SIGKILL`/`SIGTERM`/`SIGCHLD`; stopping, queued real-time signals, and most `siginfo` variants remain |
| Time | `clock_gettime`, `nanosleep`, timers, `poll`/`epoll` | High-resolution time and wait services | Partial: `clock_gettime` from the 100 Hz tick; relative/absolute `nanosleep`/`clock_nanosleep` with `EINTR`; bounded `timerfd_create`/`settime`/`gettime` for `CLOCK_MONOTONIC`, one-shot and periodic, with poll/epoll integration. High-resolution clocks and `CLOCK_REALTIME` remain absent |
| Files | `openat`, `statx`, `readlink`, `rename`, `fsync`, inotify | Filesystem service API and portal grants | Partial: read-only embedded root, per-application `/tmp`, `/settings`, and `/logs` overlays, and inotify with blocking or nonblocking instances, four watches, file and directory events (`IN_MODIFY`, `IN_ATTRIB`, `IN_OPEN`, `IN_CLOSE_WRITE`, `IN_CLOSE_NOWRITE`, `IN_CREATE`, `IN_IGNORED`), bounded-queue `IN_Q_OVERFLOW`, and thread wakeups. Recursive watches, move/delete/unmount events, and cross-process watch sharing are unsupported |
| Multiplexing and notification | `poll`, `epoll`, `eventfd`, `inotify` | Blocking/finite `poll`/`epoll_wait`, same-tgid thread-shared eventfd, blocking pipes, and blocking/nonblocking inotify | Partial: `poll` and `epoll_wait` support finite and infinite timeouts with scheduler parking and deadlines; `epoll` is level-triggered by default with a bounded `EPOLLET` transition slice and honest `EPOLLONESHOT` re-arm via `EPOLL_CTL_MOD`; pipes block on empty read/full write across `fork`-inherited descriptors; inotify is bounded to one instance, four watches, and eight queued events per process. Full inotify, nested epoll watches, and cross-process eventfd sharing remain unsupported |
| Dynamic files | `/proc/self/exe`, `/proc/self/maps`, `/proc/self/status`, `/proc/self/fd`, `/dev/urandom`, `/dev/zero`, `/dev/null` | Bounded Linux runtime pseudo-filesystem | **Implemented** in the kernel prototype through open/read/stat/readdir, plus a read-only `/proc/aether/syscalls` audit view. `/sys`, tty, input, and GPU nodes are absent. `/dev/random` and `/dev/urandom` return the same deterministic boot PRNG stream as `getrandom`, not host entropy |
| Wayland/X11 | `wl_display`, shm buffers, xdg-shell, seat, clipboard | Wayland compositor service over the Aether compositor | Not implemented |
| Vulkan/GL | Vulkan loader/ICD or EGL/GLES; swapchain to a surface | GPU service or bundled software renderer | Not implemented; no GPU service |
| Input | Pointer, keyboard, focus, cursor, keymap | Compositor input protocol | Partial: PS/2 keyboard and mouse in the desktop |
| D-Bus / XDG portal | Session bus, file chooser portal | IPC bridge and portal integration | Not implemented |
| Audio | ALSA/PipeWire | Audio service | Not implemented; not required for first milestone |
| Network | Sockets, DNS | Network service and capability policy | Not implemented; denied by default |

`poll` and `epoll_wait` accept zero, finite millisecond, and infinite
(negative) timeouts. A waiting task is parked through the scheduler and is
woken by readiness or by the tick deadline; the syscall is re-executed after
the wakeup, so the handler re-evaluates readiness with the original absolute
deadline. `epoll` reports level-triggered readiness on every call,
edge-triggered readiness only on a new transition for the tested same-task
slice, and `EPOLLONESHOT` watches are disarmed after one report and re-armed by
`EPOLL_CTL_MOD`. Nested epoll watches return `EINVAL`. Pipe storage is global:
`fork`-inherited descriptors share one bounded 512-byte buffer, blocking reads
return `0` at end-of-file once all writers close, and blocking writes wait for
space. A deliverable pending signal interrupts these waits with `EINTR`.
Eventfd counters are shared between threads with the same tgid; fork creates a
process-private counter snapshot. `timerfd` is limited to two descriptors per
task, `CLOCK_MONOTONIC`/`CLOCK_REALTIME` aliased to the tick clock, one-shot and
periodic modes, and integration with `poll`/`epoll`. Futex timed waits remain
covered by `AETHER_LINUX_FUTEX_TIMEOUT_OK`.

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

- `linuxd` is a user-space service. In the current Gate 2 slice it is the
  ring-3 launch policy manager: it validates the signed identity, version, and
  selected launch path over a capability-scoped IPC channel and approves the
  application task. The longer-term design also moves Linux ABI emulation, the
  ELF loader, library search paths, `/proc` synthesis, and per-application
  configuration into it; those are not implemented yet.
- The kernel still provides the Linux syscall trap and dispatcher, ELF and
  `PT_INTERP` mapping, page tables/VMAs, thread scheduling, futex waits, fault
  delivery, and runtime library loading. These are not system-call remoting:
  `linuxd` does not handle Linux registers or emulate descriptors in this
  slice. The kernel does not gain a Linux filesystem, network stack, device
  drivers, or graphics stack.
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
and symlinks, with extraction limits, symlink/hardlink policy, an explicit
xattr allowlist/denylist, and a host `squashfs-extract` tool. It extracts all 16 files (48,350,419 bytes) and two
symlinks from the tested AppImage. Runtime-rootfs packaging/signing and in-OS
import of both the packed runtime rootfs and a separately identified signed
AppImage are implemented. The runtime pin is host-derived (glibc only); Mesa
and Wayland content are still absent.

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
bounded demand paging for private anonymous mappings is exercised for zero-fill
and write/read (`AETHER_LINUX_DEMAND_PAGING_OK`). Static and dynamic `execve`
fixtures verify bounded argv/envp propagation (`AETHER_LINUX_EXECVE_OK`,
`AETHER_LINUX_EXECVE_DYN_OK`). A dynamic test image is mapped through
`PT_INTERP` into an `ET_DYN` address space, receives argc/argv/envp/auxv, and
hands control from the test interpreter to the main entry
(`AETHER_LINUX_DYN_INTERP_OK`, `AETHER_LINUX_DYN_MAIN_OK`,
`AETHER_LINUX_DYN_OK`). The opt-in `make glibc-test` target builds a runtime
rootfs from host glibc, resolves the real `PT_INTERP` path through that rootfs,
and runs a dynamically linked C hello through `ld.so`, relocations, and shared
`libc` (`AETHER_GLIBC_OK`). This is a host-ABI smoke test, not the pinned
production runtime. `make runtime-rootfs-pinned` packages the host-derived
pinned profile, and the normal boot test consumes it: the kernel verifies the
manifest digest against the build pin and reports the exact glibc version
(`AETHER_RUNTIME_ROOT_OK`). A separately identified signed AppImage is built by
`tools/build-appimage-fixture.sh`, copied to the boot volume as
`\linux\app.AppImage` with its signed record at `\linux\app.import`, and read
by the UEFI loader rather than embedded in the kernel. The loader publishes
the physical extents in `BootInfo` v4 and retains the buffers; the kernel
verifies the record and digest against the trust store before parsing,
reports `AETHER_LINUX_APPIMAGE_STAGED_OK source=loader`, imports the payload,
and launches `/usr/bin/app` (`AETHER_LINUX_APPIMAGE_OK`,
`AETHER_LINUX_APPIMAGE_EXEC_OK`). The AppImage runtime is never executed.
A missing file fails in the loader and a tampered file fails verification
before parsing or launch (`make appimage-stage-missing-test`,
`make appimage-stage-tamper-test`). The user AppImage is still placed on the
boot volume by the image build, not selected from a persistent device or file
picker. File metadata, `readlink`, `fcntl`, descriptor duplication,
directory enumeration, and a bounded pipe with non-blocking `poll` are also
exercised (`AETHER_LINUX_FILEMETA_OK`, `AETHER_LINUX_DIR_OK`,
`AETHER_LINUX_PIPE_OK`, `AETHER_LINUX_EVENT_OK`). Inotify exercises
nonblocking and default-blocking instances, multiple file/directory watches,
`IN_MODIFY`/`IN_ATTRIB`/`IN_OPEN`/`IN_CLOSE_WRITE`/`IN_IGNORED`, queue overflow,
and a blocking read woken by a `CLONE_VM` thread
(`AETHER_LINUX_INOTIFY_OK`, `AETHER_LINUX_INOTIFY_BLOCK_OK`). Pseudo-files and
synthetic devices are read through normal descriptors
(`AETHER_LINUX_PROC_OK`, `AETHER_LINUX_DEV_OK`). A default-deny syscall
classifier returns `ENOSYS` for unknown calls and `EACCES` for network/device
groups, with counters in `/proc/aether/syscalls`
(`AETHER_LINUX_SYSCALL_POLICY_OK`); path/read-only/device denials and
descriptor/VMA/overlay quotas have their own markers
(`AETHER_LINUX_DENIAL_OK`, `AETHER_LINUX_QUOTA_OK`).
A thread is created with
`clone(CLONE_VM|CLONE_THREAD)`, synchronizes through `FUTEX_WAIT`/`FUTEX_WAKE`,
and joins through `CLONE_CHILD_CLEARTID` (`AETHER_LINUX_THREAD_OK`). `mremap`
grows and moves a mapping while preserving contents (`AETHER_LINUX_MREMAP_OK`).
A SIGUSR1 handler is installed, raised, run, and returned from through
`rt_sigreturn` (`AETHER_LINUX_SIGNAL_OK`); a separate child receives
cross-process SIGUSR1 (`AETHER_LINUX_CROSS_PROCESS_SIGNAL_OK`). A child process
is forked and reaped through blocking `wait4`, with nonchild/selector checks
(`AETHER_LINUX_FORK_OK`), and bounded same-session process groups are exercised
(`AETHER_LINUX_PROCESS_GROUP_OK`), including group-scoped `kill(-pgid)` and
`setpgid` `EPERM`/`ESRCH`/`EINVAL` validation
(`AETHER_LINUX_PROCESS_GROUP_KILL_OK`). The signal fixture verifies an x86-64
`rt_sigframe` with `siginfo_t`/`ucontext_t`, `si_code` for `kill` and
`tgkill`, `ucontext` register/mask capture, and GPR restore
(`AETHER_LINUX_SIGINFO_OK`); alternate-stack delivery and safe fallback
(`AETHER_LINUX_ALTSTACK_OK`); `SA_RESTART` versus `EINTR` for `wait4`
(`AETHER_LINUX_RESTART_OK`); and default `SIGTERM` handler plus `SIGKILL`
termination (`AETHER_LINUX_SIGNAL_DEFAULT_OK`). A bounded `clone3` thread
create/join and its `EINVAL`/`ENOSYS` rejections are tested
(`AETHER_LINUX_CLONE3_OK`), as are `set_robust_list`/`get_robust_list` and
owner-died wakeup on thread exit (`AETHER_LINUX_ROBUST_FUTEX_OK`) and bounded
`rseq` registration/validation (`AETHER_LINUX_RSEQ_OK`). Futex timeout behavior
is tested (`AETHER_LINUX_FUTEX_TIMEOUT_OK`). Eventfd counters are shared across
threads in one tgid (and remain process-private across fork). Blocking
multiplexing is covered by `poll` finite/infinite timeouts over a pipe and an
eventfd (`AETHER_LINUX_POLL_BLOCK_OK`), `epoll_wait` finite/infinite waits with
level-triggered re-reporting, an edge-triggered transition slice, and one-shot
re-arm (`AETHER_LINUX_EPOLL_BLOCK_OK`), and cross-process blocking pipe
read/write plus end-of-file (`AETHER_LINUX_PIPE_BLOCK_OK`). Relative and
absolute sleeps with signal `EINTR` (`AETHER_LINUX_NANOSLEEP_OK`) and
one-shot/periodic timerfds seen through `poll` and `epoll`
(`AETHER_LINUX_TIMERFD_OK`) are also exercised. A ring-3 `linuxd`
launch manager runs in its own PML4 with a two-capability IPC channel. The
kernel verifies the signed runtime profile and AppImage, sends the signed
identity, version, and launch path, and waits for approval. `linuxd` first
rejects a deliberately invalid path (`AETHER_LINUXD_DENY_OK`), then approves
the genuine request; the kernel creates and schedules the application task only
after approval (`AETHER_LINUXD_OK`, `AETHER_LINUXD_USER_OK`). The service's
native syscall surface is allowlisted and excludes files, portal access,
endpoint creation, and window requests. Linux syscall dispatch, ELF mapping,
and library loading remain kernel-side; the syscall-remoting ABI is
unimplemented and is not claimed. The overlay maps
`/tmp`, `/settings`, and `/logs`, enforces a 1 KiB per-application quota and
512-byte per-file limit, survives task exit within a boot, and supports erasure
(`AETHER_LINUX_OVERLAY_PERSIST_OK`, `AETHER_LINUX_OVERLAY_ERASE_OK`). It is
RAM-backed and does not survive power loss. A static freestanding Linux ELF runs at CPL3,
exercises anonymous and
file-backed `mmap`, `munmap` and fixed-address reuse, `mprotect`, growing
`brk`, FS-based TLS, `openat`/`read`/`close` against a validated read-only
root, create/write/read through the `/tmp` overlay, `write`, and `exit`. It
produces `AETHER_LINUX_ROOT_OK`, `AETHER_LINUX_OVERLAY_OK`,
`AETHER_LINUX_VMA_OK`, plus `AETHER_LINUX_OK`; a writable+executable mapping
and a W+X `mprotect` are denied. Private anonymous pages are allocated on demand
and reclaimed on `munmap` and task exit. A parent and child share one
`MAP_SHARED|MAP_ANONYMOUS` page and observe writes in both directions while a
neighbouring private page keeps its pre-fork contents
(`AETHER_LINUX_SHARED_FORK_OK`); the shared frame is reference-counted and is
reclaimed without a double free when both mappings disappear. A read-shared
mapping of the rootfs `/etc/motd` returns the expected bytes and reports
`EOPNOTSUPP` from `msync` (`AETHER_LINUX_MAP_SHARED_FILE_OK`). Per-domain VMA,
descriptor, overlay-byte, shared-VMA, and shared-page quotas are enforced with
denial evidence (`AETHER_LINUX_QUOTA_OK`), as are default-deny syscall
classification and path/device denials (`AETHER_LINUX_SYSCALL_POLICY_OK`,
`AETHER_LINUX_DENIAL_OK`). Nested epoll watches, complete inotify semantics,
high-resolution timers, stopped/continued waits, signal stopping, remaining
`siginfo` variants, and the user-space `linuxd` protocol remain missing.
True writeback for shared file mappings and cross-process `mremap` of shared
regions remain unsupported. Gate 2 remains partial.

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

1. The runtime pin is derived from the host glibc; Mesa and Wayland content are
   absent, and the pin is not reproducible across hosts until a pinned artifact
   channel exists.
2. `linuxd` is a ring-3 launch-policy manager, but the Linux syscall
   dispatcher, ELF/library mapping, and VMA management remain in the kernel.
   The syscall-remoting ABI and removal of Linux dispatch from the kernel are
   unimplemented; no remote-syscall claim is made.
3. Shared mappings are bounded and in-memory. `MAP_SHARED|MAP_ANONYMOUS`
   survives `fork` through reference-counted frames, and read-shared file
   mappings alias a bounded page cache across processes. There is no writeback
   to the read-only root (`msync` returns `EOPNOTSUPP`), shared `mremap` is
   rejected, and shared VMA/page quotas (16 entries and 256 pages per
   application domain) are released on unmap and task exit but not persisted
   across a crash. Per-domain VMA, descriptor, and overlay-byte quotas also
   exist.
4. Inotify is bounded: no recursive watches, move/delete/unmount events, or
   cross-process watch sharing; `/sys` and non-synthetic device nodes are
   absent. Cross-process eventfd counters, nested epoll watches,
   high-resolution timers, stopped/continued waits, signal stopping, queued
   real-time signals, and the remaining `siginfo`/`clone3`/`rseq` variants
   remain absent. Blocking/finite poll, epoll, pipe, sleep, and timerfd slices
   are present for the tested same-task and pipe `fork` cases, and the
   implemented `siginfo`/`ucontext`, alt-stack, restart, robust-futex, and
   `clone3` thread subset is bounded and fixture-tested.
5. The pinned runtime rootfs carries a host-derived glibc/loader profile; Mesa
   and Wayland content, the Wayland service, and a software renderer are absent.
6. No D-Bus broker or XDG portal bridge for the Linux domain.
7. No persistent storage; the RAM disk and the application overlay are
   volatile, so overlay persistence is limited to one boot.
8. The AppImage import path is exercised with a separately built signed
   artifact that the UEFI loader stages from the boot volume. There is no user
   file picker, writable install location, or persistent block device, so the
   artifact is still test-built rather than user-supplied.

These are ordered dependencies, not independent bugs. Gate 3 does not require
graphics; Gate 4 does. Owners and test IDs are in the
[host-interface inventory](linux-host-interface-inventory.md).

## References

- AppImage specification: <https://docs.appimage.org/reference/specification.html>
- SquashFS 4.0 on-disk format: <https://dr-emann.github.io/squashfs/>
- Wayland protocol: <https://wayland.freedesktop.org/docs/html/>
- Vulkan and lavapipe: <https://docs.mesa3d.org/drivers/llvmpipe.html>
- GridCraft: <https://github.com/storytold/gridcraft>

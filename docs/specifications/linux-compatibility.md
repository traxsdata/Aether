# Linux application compatibility

**Document ID:** AETHER-LINUX-001
**Status:** Gate 4 windowed GridCraft is met at QEMU prototype scope; the
Gate 3 headless CLI run, the Gate 4 runtime rootfs packaging, the kernel
display endpoint, the GUI portal contract, and the Wayland bridge are
implemented and exercised by that completed run. General Linux application
compatibility is not claimed.
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
- No GridCraft record may be classified above `limited` until Gate 4 passes
  and the Gate 2 production follow-up is complete. Gate 4 has passed at QEMU
  prototype scope; the record remains `limited` because the follow-up is
  open.

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
| Test date | 2026-10-21 for Gate 4; 2026-10-20 for Gate 3 |
| Runtime profile | Gate 3: `gate3-host-glibc`, glibc 2.43; Gate 4: `gate4-host-lavapipe`, Mesa `lavapipe-26.0.8-1ubuntu0.3` and Wayland `1.24.0-2`; both host-derived pins |
| Classification | `limited` (headless and windowed GridCraft pass at QEMU prototype scope; general Linux compatibility is not claimed) |

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
| AppImage import | Validate ELF/SquashFS bounds, digest, publisher; unpack read-only root | AppImage importer and package trust policy | **Container inspection, pinned digest, signed AppImage and runtime-rootfs records, all-compression SquashFS extraction, host packaging, pinned host-glibc and host-lavapipe root profiles, embedded and loader-staged signed-rootfs import, loader-staged signed AppImage import, and the real GridCraft 0.8.0 import and launch implemented**; a cross-host reproducible pin, a merged per-app root, and user-selected AppImage import are missing |
| ELF and libraries | `PT_INTERP`, `PT_LOAD`, `PT_DYNAMIC`, relocations, TLS, `ld.so` | Linux address-space loader and shared-library service | Partial: static and dynamic exec fixtures propagate bounded argv/envp; a `PT_INTERP` fixture, opt-in host-glibc hello, host-derived glibc/loader profiles, and the dynamically linked GridCraft CLI run; the Gate 4 profile packages Mesa lavapipe, LLVM, Wayland client libraries, and the Vulkan loader with a complete `ldd` closure. A cross-host reproducible pin remains missing |
| Process creation | `clone`/`fork`/`execve`, wait, exit, process groups | User-space process API over the kernel task model | Partial: eager-copy fork, blocking `wait4` with nonchild/selector checks, bounded same-session process groups with `setpgid`/`getpgid`/`getsid`/`setsid` validation, static/dynamic exec, and bounded `clone3` thread creation; stopped/continued waits and group selectors return `ENOSYS`/`EINVAL` |
| Threads and TLS | `clone` with thread flags, `set_tid_address`, `arch_prctl`, `rseq` | Thread API and per-thread kernel state | Partial: `clone` and `clone3` threads, per-thread stacks/TLS, clear-child-tid join, and bounded `rseq` registration/validation (CPU id always 0, no abort notifications); detach and affinity remain |
| Memory | `mmap`/`munmap`/`mprotect`/`brk`, file mappings, shared memory | Memory-object capabilities and user mapping syscalls | Partial: bounded demand paging for private anonymous mappings; `MAP_SHARED|MAP_ANONYMOUS` uses a reference-counted physical-frame registry, so contents survive `fork` and are visible to parent and child while private mappings stay private. A bounded in-memory page cache backs read-shared file mappings keyed by rootfs/overlay inode and page offset, including aliasing across unrelated processes; writes are never persisted to the read-only root and `msync` returns `EOPNOTSUPP`. Shared mappings are limited to 16 VMA entries and 256 physical pages per application domain, with `ENOMEM` on exhaustion. `mremap` of a shared mapping returns `EOPNOTSUPP`. |
| Synchronization | `futex`, robust lists, condition variables, `eventfd` | Futex wait/wake primitive and timeout service | Partial: futex wait/wake queues, timeout (`ETIMEDOUT`), clear-child-tid join, robust-futex registration and exit-time `FUTEX_OWNER_DIED` wakeup for the exact 24-byte x86-64 head layout, and eventfd counters shared by threads in one tgid; condition variables, semaphores, and other robust-list layouts remain |
| Signals | Handler delivery, `sigaltstack`, `rt_sigreturn`, fault-to-signal | Fault delivery and signal runtime | Partial: x86-64 `rt_sigframe` with `siginfo_t`/`ucontext_t`, `SA_SIGINFO`, `SA_ONSTACK` with fallback, GPR/mask restore, `SA_RESTART` for `wait4`, self/group and cross-process SIGUSR1 handlers, and default `SIGKILL`/`SIGTERM`/`SIGCHLD`; stopping, queued real-time signals, and most `siginfo` variants remain |
| Time | `clock_gettime`, `nanosleep`, timers, `poll`/`epoll` | High-resolution time and wait services | Partial: `clock_gettime` from the 100 Hz tick, with `CLOCK_REALTIME` anchored to the CMOS RTC read once at first use and unknown clock ids rejected; relative/absolute `nanosleep`/`clock_nanosleep` with `EINTR`; bounded `timerfd_create`/`settime`/`gettime` for `CLOCK_MONOTONIC`, one-shot and periodic, with poll/epoll integration. High-resolution clocks and per-task CPU-time clocks remain absent (the CPU-time clocks report uptime) |
| Files | `openat`, `statx`, `readlink`, `rename`, `unlink`, `fsync`, inotify | Filesystem service API and portal grants | Partial: read-only embedded root, per-application `/tmp`, `/settings`, and `/logs` overlays with bounded `rename`/`unlink` confined to the current application, exact-file portal grants exposed as `/run/aether/portal/<grant-id>/<file>` for read and read+write opens with revocation, and inotify with blocking or nonblocking instances, four watches, file and directory events (`IN_MODIFY`, `IN_ATTRIB`, `IN_OPEN`, `IN_CLOSE_WRITE`, `IN_CLOSE_NOWRITE`, `IN_CREATE`, `IN_IGNORED`), bounded-queue `IN_Q_OVERFLOW`, and thread wakeups. Recursive watches, move/delete/unmount events, `renameat`/`unlinkat`, cross-process watch sharing, and deferred deletion for open descriptors are unsupported |
| Multiplexing and notification | `poll`, `epoll`, `eventfd`, `inotify` | Blocking/finite `poll`/`epoll_wait`, same-tgid thread-shared eventfd, blocking pipes, and blocking/nonblocking inotify | Partial: `poll` and `epoll_wait` support finite and infinite timeouts with scheduler parking and deadlines; `epoll` is level-triggered by default with a bounded `EPOLLET` transition slice and honest `EPOLLONESHOT` re-arm via `EPOLL_CTL_MOD`; pipes block on empty read/full write across `fork`-inherited descriptors; inotify is bounded to one instance, four watches, and eight queued events per process. Full inotify, nested epoll watches, and cross-process eventfd sharing remain unsupported |
| Dynamic files | `/proc/self/exe`, `/proc/self/maps`, `/proc/self/status`, `/proc/self/fd`, `/dev/urandom`, `/dev/zero`, `/dev/null` | Bounded Linux runtime pseudo-filesystem | **Implemented** in the kernel prototype through open/read/stat/readdir, plus a read-only `/proc/aether/syscalls` audit view. `/sys`, tty, input, and GPU nodes are absent. `/dev/random` and `/dev/urandom` return the same deterministic boot PRNG stream as `getrandom`, not host entropy |
| Wayland/X11 | `wl_display`, shm buffers, xdg-shell, seat, clipboard | Wayland compositor service over the Aether compositor | Partial: an in-app bridge (`runtime/wayland/wayland_shim.c`) implements the `libwayland-client.so.0` client ABI — registry, `wl_compositor`, `wl_shm` pools over `memfd`, `wl_seat` keyboard/pointer, xdg-shell toplevel configure, an embedded US keymap, and frame callbacks — plus a `libwayland-cursor.so.0` stub, and `make gate4-runtime` installs it at the host client libraries' SONAME path inside the signed rootfs. `libxkbcommon.so.0` is packaged with a verified closure, and the synthetic `/dev/aether-display` endpoint, memfd shared buffers, and the Linux compositor window are implemented. Clipboard, a Wayland wire server, and interactive resize are unimplemented; the completed Gate 4 windowed run exercises the present/input subset |
| Vulkan/GL | Vulkan loader/ICD or EGL/GLES; swapchain to a surface | GPU service or bundled software renderer | Partial: the Gate 4 rootfs packages the Vulkan loader, Mesa lavapipe, `libvulkan_lvp.so`, the ICD, LLVM, and dependencies, and the Wayland WSI resolves through the in-app bridge to the display endpoint; there is no GPU service, and the completed windowed run renders through lavapipe at QEMU prototype scope |
| Input | Pointer, keyboard, focus, cursor, keymap | Compositor input protocol | Partial: PS/2 keyboard and mouse in the desktop; focused Linux windows receive kernel-encoded key/pointer records with surface-pixel coordinates, modifier bits, and press/release pairs, which the bridge converts into Wayland keyboard/pointer events with an embedded US keymap. Broader layouts, cursor-surface handling, pointer capture, and drag-and-drop remain absent |
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
task, `CLOCK_MONOTONIC` and `CLOCK_REALTIME` (absolute deadlines on the latter
are wall-clock time), one-shot and periodic modes, and integration with `poll`/`epoll`. Futex timed waits remain
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
  present them in the application's window. In the implemented Gate 4 slice
  this subset lives in the in-app client bridge
  (`runtime/wayland/wayland_shim.c`) over the `/dev/aether-display` endpoint
  rather than in a socket service; the user-space `waylandd` split remains the
  design target.
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
`linux-rootfs` artifacts. An AppImage record may also carry the signed
`memory=jit` field, which is the only way a Linux task is allowed
writable+executable memory. `tools/build-runtime-rootfs.sh` packages a root
directory as zstd SquashFS and signs it.

**Evidence:** `apps/linux/appimage` validates the container and verifies pinned
digests and signed import records; `apps/linux/squashfs` reads
zstd/gzip/lz4/lzo/xz/lzma SquashFS metadata, file data, fragments, directories,
and symlinks, with extraction limits, symlink/hardlink policy, an explicit
xattr allowlist/denylist, and a host `squashfs-extract` tool. It extracts all 16 files (48,350,419 bytes) and two
symlinks from the tested AppImage. Runtime-rootfs packaging/signing and in-OS
import of both the packed runtime rootfs and a separately identified signed
AppImage are implemented. The Gate 4 runtime profile packages Mesa lavapipe,
LLVM, Wayland client libraries, and `libxkbcommon`; the Gate 3 profile remains
glibc-only.

### Gate 1 — Host-interface inventory

Map every facility the pinned application and runtime need to an Aether
interface, with owners and test cases.

**Evidence:** [the Linux host-interface inventory](linux-host-interface-inventory.md)
maps 32 facilities from the pinned GUI and CLI payloads to an interface, owner,
and test ID, with decisions for X11, D-Bus, process spawning, clipboard, paths,
locale, GPU, and network. **Met at Gate 1 time; refreshed as Gate 4 code
lands:** 2 facilities are existing, 16 partial, 9
missing, 2 deliberately denied, and 3 deferred. The inventory is complete; the
remaining gaps are implementation work and their absence does not invalidate the
mapping. Do not claim compatibility.

The remaining gap is implementation, not planning. Gate 2 proceeds from the
inventory's owner and test IDs in dependency order.

### Gate 2 — Interface mapping and prototype

Implement the narrow kernel primitives and the `linuxd` ABI service. Run static
Linux test binaries first, then dynamically linked glibc binaries. Every
syscall and pointer is validated; unsupported calls fail with `ENOSYS`, never
with ambient authority.

**Evidence:** boot transcript, syscall coverage tests, denial tests. **Met (prototype scope):**
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
the physical extents in `BootInfo` v5 and retains the buffers; the kernel
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
`/tmp`, `/settings`, and `/logs`, enforces a 16-entry overlay with a 4 KiB
per-file limit and an 8 KiB per-application quota, survives task exit within a
boot, and supports erasure
(`AETHER_LINUX_OVERLAY_PERSIST_OK`, `AETHER_LINUX_OVERLAY_ERASE_OK`). It is
RAM-backed and does not survive power loss. A static freestanding Linux ELF runs at CPL3,
exercises anonymous and
file-backed `mmap`, `munmap` and fixed-address reuse, `mprotect`, growing
`brk`, FS-based TLS, `openat`/`read`/`close` against a validated read-only
root, create/write/read through the `/tmp` overlay, `write`, and `exit`. It
produces `AETHER_LINUX_ROOT_OK`, `AETHER_LINUX_OVERLAY_OK`,
`AETHER_LINUX_VMA_OK`, plus `AETHER_LINUX_OK`; a writable+executable
mapping is refused with `EACCES` because this program has no import record
declaring `memory=jit` (the AppImage fixture declares it and its W+X mapping
succeeds, as the LLVM section allocator used by lavapipe requires), and a W+X
`mprotect` is always denied. `PROT_NONE` pages are guard pages: a forked
child that touches one faults, and the kernel refuses them as syscall
buffers (`AETHER_LINUX_GUARD_OK`). Private anonymous pages are allocated on demand
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
`AETHER_LINUX_DENIAL_OK`). The ring-3 `linuxd` service approves the launch
after validating identity, version, and path, and rejects a malformed path
(`AETHER_LINUXD_USER_OK`, `AETHER_LINUXD_DENY_OK`).
Nested epoll watches, complete inotify semantics, high-resolution timers,
stopped/continued waits, signal stopping, remaining `siginfo` variants, and
the `linuxd` syscall-remoting protocol remain missing. Linux syscall dispatch,
ELF/library mapping, and VMA management are still kernel-side. True writeback
for shared file mappings and cross-process `mremap` of shared regions remain
unsupported. Gate 2's prototype exit is met at QEMU scope; these items are
tracked as the Gate 2 production follow-up and do not justify a compatibility
claim.

### Gate 3 — Headless GridCraft

Import the AppImage and run the bundled `gridcraft-cli` unmodified
(`gridcraft-cli eval '=1+1'` and a CSV round trip) in an isolated domain with a
read-only app root and a private overlay.

**Status: met at QEMU prototype scope.** `make gate3-test` stages the pinned
19,024,376-byte AppImage, verifies its record and digest, imports the SquashFS
payload, and launches `/usr/bin/gridcraft-cli` through its `PT_INTERP` against
the signed Gate 3 runtime rootfs. One process runs:

```text
gridcraft-cli run --in /usr/share/gridcraft/gate3-in.csv \
  --cmd 'formulas.evaluate={"formula":"=1+1"}' \
  --cmd 'cell.set={"cell":"A4","input":"total"}' \
  --cmd 'cell.set={"cell":"B4","input":"=SUM(B2:B3)"}' \
  --out /tmp/gate3-out.csv --print A1:B4 --csv
```

The boot transcript contains the formula result `2.0`, cell `B4` value `3`,
`wrote /tmp/gate3-out.csv (38 bytes)`, and the CSV rows `item,value`,
`alpha,1`, `beta,2`, `total,3`, followed by a zero exit,
`AETHER_GATE3_APP_OK`, and
`AETHER_GATE3_DENIAL_OK portal=none native=denied root=readonly
overlay=private`. The denial marker is derived from the application task's
empty capability table, not from guest output. The compatibility database
record is `limited` with the named tests `linux.appimage.import`,
`linux.gridcraft.eval`, `linux.gridcraft.csv`, and `linux.denial.portal`.

**Prototype limitations:** the selected program is read from the verified
AppImage, while libraries and data are resolved from the signed runtime rootfs;
the imported tree is not yet merged into a per-application visible root. The
runtime pin is host-derived, the overlay is RAM-backed, and the AppImage is
staged at build time rather than selected by a user. These do not block the
headless gate but do block a production compatibility claim.

### Gate 4 — Windowed GridCraft

Run `usr/bin/gridcraft` with the Wayland bridge and software renderer, present
its window in the Aether compositor, accept keyboard and pointer input, and
open/save a workbook through the portal.

**Status: met at QEMU prototype scope** on 2026-10-21 at tested code commit
`716dcb4`, revalidated at `ba6544c` (a docs-only descendant), and extended by
the interaction-performance slice at `3092b50`, on `qemu-q35-ovmf` with
software VGA, no GPU, and no X11/D-Bus.
`make gate4-portal-test` builds a signed AppImage fixture, seeds an exact-file
read+write grant for the demo root, and runs the fixture as a Linux task.
The fixture opens
`/run/aether/portal/dd8f879ba78d207c/gate4.xlsx`, reads the workbook fixture,
writes `<file>.sctmp`, commits it with `rename`, reads the committed bytes
back, stages a second save, asks a test-only kernel path to revoke the grant,
and verifies that the post-revocation rename, read, and write all fail closed.
The kernel then reads the backing file and checks it still contains exactly the
first committed bytes, so the revoked commit did not land.

The namespace is resolved by the host-tested policy in
`aether-services::portal_store`: the path must be exactly
`/run/aether/portal/<16-hex-grant-id>/<file>`, the caller root must match the
grant root exactly, the namespace file must be the grant's final path
component, and the requested rights must be a subset of the grant. `..`,
duplicate slashes, trailing slashes, extra components, overlong names, caller
root mismatch, unknown grant ids, and revoked records all fail closed. There is
no directory grant and no ambient `/home` mapping: the namespace descriptor
carries the resolved backing path, which is `/home/gate4.xlsx` in this test.

The save transaction is also exact-file scoped. `<file>.sctmp` is a bounded
staging slot, not a second user file: it may only be created and written when
`<file>` is an active write grant for the caller root. `rename(<file>.sctmp,
<file>)` commits to the same backing path after rechecking root, file, rights,
and revocation; a non-temp source, a temp destination, a different final file,
a cross-grant rename, traversal, or a revoked grant fails before any bytes are
written. `unlink(<file>.sctmp)` discards the staging slot. Staging is RAM
backed, bounded to 2 MiB per grant with one active slot per file and two active
slots overall, and is independent of the application overlay quota.

Kernel-owned evidence, emitted only after validating the operation:

- `AETHER_GATE4_PORTAL_OK path=/run/aether/portal/dd8f879ba78d207c/gate4.xlsx
  read_bytes=106 source=kernel`
- `AETHER_GATE4_SAVE_OK file=/home/gate4.xlsx bytes=58
  staging=sctmp+rename temp_bytes=58 staged_bytes=87 renames=1 source=kernel`
  (one commit, 87 bytes staged across the valid and revoked-attempt saves, and
  the kernel reads back the exact committed fixture bytes)
- `AETHER_GATE4_REVOKE_OK path=/run/aether/portal/dd8f879ba78d207c/gate4.xlsx
  read_denials=1 write_denials=1 source=kernel`

The GUI feature `gate4-gui-test` keeps that static transcript intact and adds
a separate exact-file grant for the real 4,089-byte XLSX fixture at
`/home/gate4-gui.xlsx`. Its namespace is
`/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx` (grant id
`5b428ae53860eb72`), and `AETHER_PORTAL_WORKBOOK` names it for argv. The kernel
validates the committed bytes as a ZIP/XLSX container (`PK\x03\x04` at offset
0, size > 1024) and emits the save marker immediately on the successful
`.sctmp` rename:

```text
AETHER_GATE4_SAVE_OK file=/home/gate4-gui.xlsx bytes=4089 staging=sctmp+rename source=kernel
```

After the GUI app exits, the kernel revokes this grant itself, retries a direct
read, temp open, and rename, and emits `AETHER_GATE4_REVOKE_OK` only when all
three are denied with `Revoked`. If the save marker never fired, the post-exit
path reports `AETHER_BOOT_FAILED reason=gate4-gui save marker missing` instead
of claiming success. `make gate4-gui-portal-test` exercises this feature with a
static XLSX read/stage/commit fixture; the windowed GridCraft target consumes
the same feature and namespace argument. This is not GUI evidence by itself.

The launch environment carries `AETHER_PORTAL_WORKBOOK` with the namespace
path, and a dynamically linked GUI app can receive the same path through the
existing `LINUX_APPIMAGE_ARGS` argv. The test fixture itself is static and uses
the compiled namespace path.

**Exercised by the completed run:** the Wayland bridge
(`runtime/wayland/wayland_shim.c` as a drop-in `libwayland-client.so.0`), the
presentation path through the `/dev/aether-display` endpoint into the ring-0
compositor, the Mesa lavapipe software Vulkan path over the bridge's surface
protocol, keyboard and pointer routing, the windowed `/usr/bin/gridcraft`
launch, and the portal workbook open/save path. `make gate4-test` stages the
signed GridCraft AppImage (digest
`942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`,
19,024,376 bytes), boots the Gate 4 runtime rootfs (`gate4-host-lavapipe`,
Mesa `lavapipe-26.0.8-1ubuntu0.3`, Wayland `1.24.0-2`), and
`tools/gate4_smoke.py` waits for document-title readiness, clicks the window,
sends `4`, `2`, and `ctrl-s` through the emulated PS/2 path, closes the
window, and requires the post-exit revocation. The kernel-owned markers were:

```text
AETHER_GATE4_DISPLAY_CREATE_OK pid=7 size=1440x900
AETHER_GATE4_PRESENT_OK frames=1 checksum=0x7b2cafc5 source=linux
AETHER_GATE4_INPUT_OK keys=1 pointer=24 focus=3
AETHER_GATE4_SAVE_OK file=/home/gate4-gui.xlsx bytes=4093 staging=sctmp+rename source=kernel
AETHER_GATE4_PORTAL_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_bytes=4089 source=kernel
AETHER_GATE4_REVOKE_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_denials=1 write_denials=1 source=kernel
AETHER_BOOT_OK stage=kernel version=0.1.0
```

The latest `3092b50` screenshots are `build/gate4/gridcraft.ppm` (1920x1200,
2,304,000 non-black pixels, sha256
`ccd46fc0993ab92cf51f272260b0f2c6c733d341e80a669888a5d60e2dae3bdb`) and
`build/gate4/gridcraft-after-input.ppm` (1920x1200, 2,303,983 non-black
pixels, sha256
`a3c8b2da8ea17c064ba0447e0642b7fa8eb673632789e780668967719a0736c3`); the
post-input screenshot shows the real workbook with `GATE4-GUI.XLSX` in the
title. The original Gate 4 run used the 1280x800 artifacts listed in the
review record. The run also passes `AETHER_LINUX_RESTART_OK` after the fixture
synchronization fix.

This proves only the named windowed tests at QEMU prototype scope. It is not a
general GUI or Linux compatibility claim.

**Interaction performance (prototype):** the compositor now clips the Linux
blit to the damage rectangle, copies opaque rows as packed words, hashes only
the first presented frame, and repaints only the content rectangle for a
surface present. Clock damage is limited to the menu bar, windows outside the
damage rectangle are not walked, and the scheduler gives the compositor a
slice after a present. Long surface copies service the PS/2 controller between
chunks so keystrokes are not lost while interrupts are masked. During a
drag/resize gesture the window uses an opaque base and omits its shadow; the
gesture end repaints the full frame. The kernel reports
`AETHER_GATE4_PERF frames=... scene_paints=... cursor_paints=...
last_paint_ms=... max_paint_ms=... fast_paints=... slow_paints=...` at
endpoint close. This is a latency
improvement, not a throughput claim: lavapipe still rasterizes on the single
emulated CPU, every present still copies the full 1440x900 surface because the
bridge ABI has no damage list, and no GPU service exists.

**Still open after Gate 4:** a real desktop file chooser/D-Bus portal,
clipboard, interactive resize and window chrome, broader keymaps and cursor
surfaces, and any compatibility record stronger than `limited`. The Gate 2
production follow-up is not complete, so the GridCraft record stays `limited`.

**Runtime rootfs packaging: met at QEMU prototype scope.** `make gate4-runtime`
assembles `tools/build-gate4-runtime.sh` from pinned host libraries:

| Component | Packaged value |
|---|---|
| Profile | `gate4-host-lavapipe` |
| glibc / loader | host glibc 2.43, `ld-linux-x86-64.so.2` |
| Mesa | lavapipe 26.0.8-1ubuntu0.3 |
| Wayland client | 1.24.0-2 baseline; the build installs the in-tree Wayland bridge (`runtime/wayland/wayland_shim.c`) and cursor stub at the same SONAME paths in place of the host client libraries |
| Vulkan | loader 1.4.341.0-1, `libvulkan_lvp.so`, `lvp_icd.json` |
| LLVM | `libLLVM.so.21.1`, 138,567,144 bytes |
| Files / bytes | 40 files, 164,543,720 uncompressed bytes |
| SquashFS image | 45,268,992 bytes, zstd, signed `linux-rootfs` record |

The rootfs carries the Vulkan ICD at `/usr/share/vulkan/icd.d/lvp_icd.json`,
`/usr/share/doc/aether-runtime/provenance`, a test-only `/etc/motd`, and small
Linux ABI fixtures used by the boot gate. It does not carry the GridCraft GUI
binary or data; those come from the separately signed AppImage.

The image is too large to embed in the kernel, so the loader stages it from
`\linux\runtime.sqfs` with its record at `\linux\runtime.import`. `BootInfo` v5
publishes both extents. The kernel verifies the signed record and the manifest
pin, then verifies every `file_sha256` entry before importing the tree. The
manifest digest is the build pin; a tampered image fails signature
verification, and a missing image fails in the loader. `make gate4-runtime-test`
boots the staged rootfs, requires
`AETHER_GATE4_RUNTIME_OK profile=gate4-host-lavapipe
mesa=lavapipe-26.0.8-1ubuntu0.3 wayland=1.24.0-2`, and runs the real AppImage's
`/usr/bin/gridcraft-cli --version` through it (`gridcraft-cli 0.8.0`).

**Measured limitation:** the allocation-only bootstrap importer needs about
1.04 GiB for this 164 MiB tree because decompression scratch is never
reclaimed. The Gate 4 profile therefore builds with a 1.5 GiB heap; the
statistics below measure the current `make gate4-test` harness, which uses a
4 GiB guest. This is a packaging/boot prototype, not a production runtime
size.

**Production follow-up not yet met:** the GUI launch, Wayland bridge,
compositor presentation, input, portal workbook open/save, and pixel checksum
now pass at QEMU prototype scope, but the run is not a production claim. The
runtime pin is derived from the build host and is not reproducible across
hosts until a signed package channel exists. The rootfs still requires the
kernel-side importer and has no user-selected AppImage installation path, and
Linux ABI/ELF/VMA work remains in the kernel (Gate 2 production follow-up).
The GridCraft record therefore remains `limited`.

**Implemented kernel slice (Gate 4.1 prototype).** A granted Linux task can
`open("/dev/aether-display")`, issue fixed `CREATE`/`PRESENT`/`SET_TITLE`/
`DESTROY` records, and read fixed 32-byte input records. `PRESENT` validates the
guest range and stride, copies pixels into a kernel-owned surface, and the
ring-0 compositor paints that surface clipped and scaled into a Linux window.
Keyboard, pointer, enter/leave, configure, and frame events are queued while the
window is focused; pointer coordinates are converted to surface pixel space
(scaled and clamped), and key records carry a modifiers bitmask plus a
press/release pair. The queue is
bounded and drops oldest records on overflow. `memfd_create` (319), `ftruncate`
(77), `fstat`, and `mmap(MAP_SHARED)` provide bounded shared pools: 16 MiB per
object, four objects per task, reference-counted across `dup`/`fork` and
released on exit or fault. Access is default-deny: only the kernel boot display
fixture and the signed `ai.storyteller.gridcraft` AppImage identity are granted;
native tasks, services, and other Linux applications receive `EACCES`.

Boot evidence is produced by `make gate4-display-test`:
`AETHER_GATE4_DISPLAY_CREATE_OK`, `AETHER_GATE4_PRESENT_OK frames=1
checksum=... source=linux`, and `AETHER_GATE4_INPUT_OK keys=... pointer=...
focus=...`, followed by the guest's `AETHER_GATE4_GUEST_DISPLAY_OK`. The test
injects a real PS/2 key and mouse packet through the input queue after the first
present; the kernel only emits `INPUT_OK` after the guest has read key, pointer,
and focus records.

**Completed windowed run:** the `make gate4-test` revalidation at `ba6544c`
(a docs-only descendant of `716dcb4`) on 2026-10-21 recorded the marker
transcript, screenshot evidence, and
tested platform in the [Gate 4 status](#gate-4--windowed-gridcraft) above. The
bridge, keymap delivery, frame-callback timing, Mesa/lavapipe content, and the
workbook open/save portal path are exercised by that run. The endpoint remains
deliberately narrower than a Wayland wire server; fd passing, clipboard, a
D-Bus portal chooser, and a wire server remain absent, and no general Wayland
or GUI compatibility claim is made.

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

1. The runtime pin is derived from the host glibc and, for the Gate 4 profile,
   host Mesa/LLVM/Wayland packages. It is not reproducible across hosts until a
   pinned artifact channel exists; the Gate 4 image also needs a 1.5 GiB import
   heap because the bootstrap allocator never reclaims.
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
5. The Gate 3 pinned runtime rootfs carries a host-derived glibc/loader
   profile. The Gate 4 profile adds host-derived Mesa lavapipe/LLVM, the
   Vulkan loader, `libxkbcommon`, and the in-tree Wayland client bridge
   installed at the host `libwayland-client.so.0` SONAME path; presentation
   goes through the `/dev/aether-display` endpoint into the ring-0
   compositor. The completed windowed run exercises this path but does not
   make the pin reproducible.
6. No D-Bus broker or XDG portal bridge for the Linux domain.
7. No persistent storage; the RAM disk and the application overlay are
   volatile, so overlay persistence is limited to one boot.
8. The AppImage import path is exercised with the pinned real GridCraft
   artifact, but the UEFI loader stages it from the boot volume at build time.
   There is no user file picker, writable install location, or persistent block
   device, so the artifact is not yet user-supplied. The imported AppImage tree
   is also not merged into the application's visible root: the selected program
   is read from the verified AppImage, while libraries and data resolve from the
   runtime rootfs.
9. The kernel bootstrap heap is a 512 MiB fixed arena because the SquashFS
   importer accumulates every AppImage file and decompression buffer and never
   reclaims. A selective import of the selected program or a reclaiming scratch
   allocator is required before this is a general runtime.

These are ordered dependencies, not independent bugs. Gate 3 does not require
graphics; Gate 4 does. Owners and test IDs are in the
[host-interface inventory](linux-host-interface-inventory.md).

## References

- AppImage specification: <https://docs.appimage.org/reference/specification.html>
- SquashFS 4.0 on-disk format: <https://dr-emann.github.io/squashfs/>
- Wayland protocol: <https://wayland.freedesktop.org/docs/html/>
- Vulkan and lavapipe: <https://docs.mesa3d.org/drivers/llvmpipe.html>
- GridCraft: <https://github.com/storytold/gridcraft>

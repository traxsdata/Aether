# Linux Host-Interface Inventory

**Document ID:** AETHER-LINUX-002
**Status:** Gate 1 complete; Gate 2 prototype partial; no compatibility claim
**Related:** [Linux compatibility](linux-compatibility.md),
[Linux layer](../architecture/linux-layer.md),
[ADR-017](../decisions/ADR-017-linux-app-runtime.md),
[system ABI](system-abi.md), [compatibility database](compatibility.md)

## Purpose and method

Gate 1 asks for every facility the pinned application and its runtime need to be
mapped to an Aether interface, with an owner and a test case. This document is
that mapping. It covers the GUI payload and the headless CLI because Gate 3 uses
the CLI before Gate 4 attempts the windowed application.

The inventory separates two questions that are easy to confuse:

- **Mapped:** the facility has an interface, an owner, and a test case. This is
  the Gate 1 deliverable.
- **Implemented:** the interface exists and is tested. This is Gate 2 work and is
  recorded in the status column so a mapping is never mistaken for a capability.

The evidence comes from the pinned artifact, not from a later GridCraft release:

```sh
sha256sum gridcraft-0.8.0-linux-x86_64.AppImage
appimage-inspect gridcraft-0.8.0-linux-x86_64.AppImage
squashfs-extract gridcraft-0.8.0-linux-x86_64.AppImage root
readelf -d root/usr/bin/gridcraft
readelf -Ws root/usr/bin/gridcraft
strings -a root/usr/bin/gridcraft
```

The inspected image is the one in [Linux compatibility](linux-compatibility.md):
19,024,376 bytes, SHA-256
`942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`.
It extracts to 16 files, 27 directories, and 2 symlinks (48,350,419 bytes). The
two payloads are dynamically linked PIE executables. Their direct dependencies
are `libc.so.6`, `libm.so.6`, and `libgcc_s.so.1`; the GUI payload also names
`ld-linux-x86-64.so.2`. The GUI has 193 undefined dynamic symbols and the CLI
has 115.

The direct dependencies are small; the runtime surface is not. The GUI payload
loads Wayland, X11/XCB, EGL, Vulkan, D-Bus, and xkbcommon libraries at runtime
with `dlopen`, and imports libc calls for processes, threads, signals, sockets,
`mmap`, `statx`, `poll`, and file descriptors. The inventory below follows those
observed symbols and protocol names.

## Result

**Gate 1 is complete for the pinned artifact.** Every observed facility has a
proposed Aether interface, an owner, and a test case; the policy decisions and
acceptance checklist are recorded below. Implementation status is uneven:

| Status | Meaning | Facilities |
|---|---|---:|
| Existing | A tested interface exists and can be used by the runtime | 2 |
| Partial | Some interface or host tests exist; the runtime cannot fully use it yet | 16 |
| Missing | No interface exists | 9 |
| Denied | Deliberate absence; the runtime must fail closed | 2 |
| Deferred | Not required for Gate 3 or Gate 4 | 3 |

These counts are the Gate 1 planning baseline, refreshed where Gate 4 code
changes the answer (HI-19, HI-20, HI-24). They do not claim that the existing
interfaces are sufficient for GridCraft: the gates prove that by running
binaries.

## Facility inventory

Status values are `existing`, `partial`, `missing`, `denied`, or `deferred`.
Owners name the component that must provide the interface, including planned
user-space services. Test IDs are defined in the [test plan](#test-plan).

### Import, launch, and runtime environment

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-01 | AppImage container, digest, signed record, SquashFS import; `appimage-inspect`, `squashfs-extract` | `aether-appimage` + `aether-squashfs` + trust store | `apps/linux/*`, `aether-packaging` | HI-T01 | existing (fixture only) |
| HI-02 | Runtime rootfs: glibc, `ld.so`, `libm`, `libgcc_s`, Mesa, Wayland, D-Bus, X11 libraries | Signed runtime rootfs built and imported through the test trust store; loader resolves `PT_INTERP` from it. The Gate 4 profile is staged by the loader via `BootInfo` v5 and packages host-derived glibc, `libwayland-client`/`libwayland-cursor`, `libxkbcommon`, the Vulkan loader, Mesa lavapipe, LLVM, and the complete `ldd` closure: 40 files, 164,543,720 bytes uncompressed, 45,268,992 bytes compressed, signed `linux-rootfs` record. The kernel verifies the manifest pin and every file digest and emits `AETHER_GATE4_RUNTIME_OK`. A cross-host reproducible package channel and D-Bus/X11 runtime content are missing | `aether-packaging`, `tools/build-gate4-runtime.sh`, `tools/build-runtime-rootfs.sh`, `tools/build-glibc-rootfs.sh` | HI-T02 | partial |
| HI-03 | Dynamic ELF loading: `PT_INTERP`, `ET_DYN`, auxv, `ld-linux`, `dlopen`, `dlsym`, `dlclose`, `dl_iterate_phdr`, `__tls_get_addr` | Kernel loader resolves the interpreter through the verified rootfs, maps main/interpreter, builds auxv, and enters `ld.so`; a host-glibc hello links and runs. Static/dynamic exec fixtures carry bounded argv/envp. A user-space `linuxd` library service and broader `dlopen` coverage are missing | `kernel/src/linux_loader.rs`, `apps/linux/elf`, planned `user/linuxd` | HI-T03 | partial |
| HI-04 | Process creation: `fork`, `execvp`, `posix_spawnp`, `posix_spawn_file_actions_*`, `waitpid`, `waitid`, `pidfd_spawnp`, `setsid`, `setpgid` | Eager-copy `fork`, descriptor inheritance, blocking `wait4` with nonchild/selector checks, static/dynamic `execve` with bounded argv/envp, and bounded same-session process groups; broader process APIs/session cases remain | `kernel/src/scheduler.rs`, `kernel/src/syscall.rs` | HI-T04 | partial |
| HI-05 | Threads: `pthread_create`, `pthread_join`, `pthread_detach`, `pthread_setname_np`, `sched_getaffinity`, `set_tid_address`, `rseq` | `clone` with CLONE_VM/THREAD, per-thread kernel stack, TLS, tgid/tid, and clear-child-tid join; detach/affinity/rseq details remain | `kernel/src/scheduler.rs` | HI-T05 | partial |
| HI-06 | Environment and identity: `environ`, `getenv`, `getauxval`, `uname`, `sysconf`, `gnu_get_libc_version`, `getpwuid_r`, `getgrouplist`, `setlocale`, `realpath`, `getcwd` | Fixed `LD_LIBRARY_PATH` and auxv are supplied; broader environment, passwd, locale, and `getcwd` policy are missing | `kernel/src/linux_loader.rs`, planned `user/linuxd` | HI-T06 | partial |
| HI-07 | Descriptor I/O: `open`/`open64`/`openat`, `read`, `write`, `writev`, `close`, `lseek64`, `dup`, `dup2`, `fcntl`, `pipe2` | Per-domain descriptor table over native file handles and a bounded pipe ring | `kernel/src/linux.rs`, `kernel/src/scheduler.rs` | HI-T07 | partial |

### Files, memory, and kernel primitives

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-08 | Metadata and directories: `stat64`, `statx`, `fstat64`, `newfstatat`, `readlink`, `getdents64`, `mkdir`, `unlink`, `rename`, `dirfd` | Synthetic stat/statx and directory enumeration over the imported root; filesystem service mutations remain missing | `kernel/src/linux.rs`, `kernel/src/scheduler.rs`, `kernel/fs` | HI-T08 | partial |
| HI-09 | User files: `HOME`, `open` of workbook paths, `org.freedesktop.portal.FileChooser`, save dialogs | Exact-file portal grants exposed as `/run/aether/portal/<grant-id>/<file>` for read and read+write opens, with a bounded `.sctmp` staging file and commit-by-rename scoped to the caller root and revocable; `/home/user` remains the private overlay. A desktop chooser/D-Bus portal is still missing | `kernel/src/portal.rs`, `kernel/services/src/portal_store.rs`, `kernel/src/scheduler.rs`, planned `user/portald` | HI-T09 | partial |
| HI-10 | Memory: `mmap64`, `mprotect`, `munmap`, `mremap`, `brk`, file mappings, W^X | Per-task VMA table; bounded demand paging for anonymous mappings; file-backed `mmap`, `munmap` reclaim, `mprotect`, `mremap` grow/move, growing `brk`; `MAP_SHARED|MAP_ANONYMOUS` within one `CLONE_VM` thread group. `fork()` is rejected while such a mapping exists; cross-process shared ownership and quotas remain missing | `kernel/src/memory`, `kernel/src/scheduler.rs`, `kernel/src/syscall.rs` | HI-T10 | partial |
| HI-11 | Synchronization: `futex`, pthread mutex/condition variables, robust lists, `eventfd` | Futex wait/wake queues with timeouts | `kernel/src/scheduler.rs`, `kernel/src/ipc.rs` | HI-T11 | partial |
| HI-12 | Signals: `sigaction`, `signal`, `sigemptyset`, `sigaddset`, `sigaltstack`, `kill`, `pause` | `rt_sigaction`/`rt_sigprocmask`/`sigaltstack`, self/group `kill`/`tgkill`, handler frames, restorer return, fault-to-signal, and cross-process SIGUSR1 fixture; siginfo/ucontext completeness and signal-safe restart remain | `kernel/src/scheduler.rs`, `kernel/src/syscall.rs` | HI-T12 | partial |
| HI-13 | Time and waits: `clock_gettime`, `clock_nanosleep`, `poll` timeouts, `timerfd` | Time service plus a blocking wait queue | `kernel/src/scheduler.rs`, `kernel/src/syscall.rs` | HI-T13 | partial |
| HI-14 | Random: `getrandom` | Kernel random source | `kernel/src/syscall.rs` | HI-T14 | partial |
| HI-15 | Pseudo-files: `/proc/self/{exe,maps,status,fd}`, `/dev/{null,zero,random}`, bounded `/sys` | Bounded kernel synthesis for `/proc/self/{exe,maps,status,fd}`, `/proc/aether/syscalls`, and `/dev/{null,zero,random,urandom}`; `/sys` absent | `kernel/src/scheduler.rs`, planned `user/linuxd` | HI-T15 | partial |
| HI-16 | Multiplexing and notification: `poll`, `epoll`, `eventfd`, `inotify` | Timeout-zero `poll`, per-task `eventfd2`, basic epoll, and bounded inotify (blocking/nonblocking, four watches, directory and file events, overflow); blocking poll/epoll waits and cross-process watch sharing are missing | `kernel/src/scheduler.rs`, planned `user/linuxd` | HI-T16 | partial |
| HI-17 | Unix sockets: `socketpair`, `connect`, `bind`, `listen`, `accept4`, `sendmsg`, `recvmsg`, `getsockopt`, `setsockopt` | Brokered local sockets with validated `SCM_RIGHTS` | planned `user/linuxd`, `kernel/src/ipc.rs` | HI-T17 | missing |
| HI-18 | Network: `socket`, `getaddrinfo`, `freeaddrinfo`, `gai_strerror`, `shutdown`; AF_INET/AF_INET6 | Network service with explicit capability | planned network service | HI-T18 | denied |

### Display, input, and graphics

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-19 | Wayland client transport: `libwayland-client.so.0`, `wl_display_connect`, `wl_display_*`, event queues, fd passing | Prototype synthetic `/dev/aether-display` endpoint: one descriptor per granted Linux task, fixed request/event records, `poll` readiness, no socket or fd passing. The in-app client bridge (`runtime/wayland/wayland_shim.c`) implements the `libwayland-client.so.0` ABI, and `make gate4-runtime` installs it at the host library's SONAME path in the Gate 4 rootfs. The completed Gate 4 run exercises the tested subset end to end; fd passing remains absent | `runtime/wayland/wayland_shim.c`, `kernel/src/display.rs`, `tools/build-gate4-runtime.sh` | HI-T19, `make gate4-display-test`, `make gate4-test` | partial |
| HI-20 | Wayland protocols: `wl_compositor`, `wl_surface`, `wl_shm`, `wl_region`, `wl_callback`, `wl_seat`, `wl_keyboard`, `wl_pointer`, `wl_output`, `xdg_shell`, `xdg_popup`, `xdg_activation`, `wp_viewporter`, `wp_fractional_scale`, `wp_cursor_shape` | The client-side bridge implements the tested subset at QEMU prototype scope: compositor, surface, shm pools over `memfd`, seat keyboard/pointer with an embedded keymap, xdg-shell toplevel configure, frame callbacks, and a cursor stub. The completed Gate 4 run exercises this subset with the real GUI; unsupported Wayland requests fail closed and the endpoint does not claim protocol compatibility. Clipboard protocols remain excluded by D-04 | `runtime/wayland/wayland_shim.c`, `kernel/src/display.rs` | HI-T20, `make gate4-test` | partial |
| HI-21 | Shared-memory buffers: `wl_shm`, `wl_shm_pool`, `wl_buffer`, `wl_egl_window_*` | The bridge implements `wl_shm` pool objects over `memfd` (`wl_shm_pool`/`wl_buffer`) and the kernel provides `memfd_create`/`ftruncate`/`fstat`/`mmap(MAP_SHARED)` with 16 MiB per object, four objects per task, bounded stride/range validation on present, copy into a kernel-owned surface, and clipped/scaled painting in the Linux window; the completed Gate 4 run exercises this path | `runtime/wayland/wayland_shim.c`, `kernel/src/memfd.rs`, `kernel/src/display.rs`, `kernel/src/desktop.rs` | HI-T21, `make gate4-display-test`, `make gate4-test` | partial |
| HI-22 | Input: keyboard, pointer, touch, keymap from `libxkbcommon`, cursor themes (`libXcursor`, `wp_cursor_shape`) | PS/2 keyboard and pointer events are routed to the focused Linux window as bounded endpoint records with surface-pixel coordinates, modifier bits, and key press/release pairs; enter/leave and configure are sent on focus change. Touch, keymap data, and cursor-theme protocols are absent | `kernel/src/input.rs`, `kernel/src/display.rs`, planned `user/waylandd` | HI-T22, `make gate4-display-test` | partial |
| HI-23 | EGL/OpenGL ES: `libEGL.so.1`, `EGL_*` platform extensions, `wl_egl_window` | Software GL service (llvmpipe) over the Wayland surface | `kernel/src/graphics.rs`, planned graphics service | HI-T23 | missing |
| HI-24 | Vulkan: `libvulkan.so.1`, `VK_KHR_surface`, `VK_KHR_wayland_surface`, `VK_KHR_swapchain`, lavapipe | Gate 4 rootfs packages `libvulkan.so.1`, `libvulkan_lvp.so`, `lvp_icd.json`, LLVM, and dependencies with a verified closure; the Wayland WSI resolves through the in-app client bridge to the `/dev/aether-display` endpoint. The completed Gate 4 run renders the real GUI through lavapipe; a GPU service remains missing | `tools/build-gate4-runtime.sh`, `runtime/wayland/wayland_shim.c` | HI-T24, `make gate4-test` | partial |
| HI-25 | X11/XCB fallback: `libX11.so.6`, `libX11-xcb.so.1`, `libxcb.so.1`, `libXcursor`, `libXi`, `libxkbcommon-x11`, `DISPLAY` | No X11 server; Wayland-only policy with a clean failure | planned `user/linuxd` | HI-T25 | deferred |
| HI-32 | Clipboard: `wl_data_device_manager`, `wl_data_source`, `wl_data_offer`, `arboard` | Revocable clipboard grant; absent in the first profile (D-04) | planned `user/waylandd`, `kernel/src/desktop.rs` | HI-T32 | deferred |

### Services, lifecycle, and policy

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-26 | D-Bus session bus: `libdbus-1.so.3`, `org.freedesktop.DBus*`, `zbus` | Per-domain message broker with an allowlisted portal interface | planned `user/dbusd` | HI-T26 | missing |
| HI-27 | XDG desktop portal: `org.freedesktop.portal.Desktop`, `FileChooser`, `Request` | Portal UI and grant records exposed through the broker | `kernel/src/portal.rs`, planned `user/portald` | HI-T09, HI-T26 | missing |
| HI-28 | Audio: ALSA/PipeWire symbols; no audio path observed in the CLI | Audio service only when a capability is granted | planned audio service | HI-T27 | deferred |
| HI-29 | Devices and GPU nodes: no `/dev/input`, `/dev/dri`, block devices, or raw PCI | Deny by default; no device capability in the first profile | `kernel/services`, planned `user/linuxd` | HI-T28 | denied |
| HI-30 | Lifecycle and limits: crash containment, restart budget, address-space/thread/fd/CPU quotas, `prlimit64`, `sched_*` | Supervisor plus per-domain resource accounting | `kernel/src/supervisor.rs`, `kernel/src/scheduler.rs` | HI-T29, HI-T30 | partial |
| HI-31 | Compatibility record: exact digest, runtime version, named tests | Database schema and classification rules | `tests/compatibility` | HI-T31 | existing (record is `limited` after the Gate 3 and Gate 4 runs) |

## Test plan

A test ID is a commitment to a harness, a pass condition, and a failure mode. A
planned test is not evidence until it runs in `make ci`.

| ID | Harness | What must pass | Status |
|---|---|---|---|
| HI-T01 | `make appimage-test`, `make squashfs-test` | Container and rootfs parsing, digest/signature checks, extraction limits, hostile archives | existing |
| HI-T02 | `make test`, `make glibc-test` | Builds, signs, imports, and consumes a runtime rootfs; the opt-in target packages host glibc and resolves `PT_INTERP` from it | partial |
| HI-T03 | `make test`, `make glibc-test` | The default fixture maps a test `ET_DYN` image through `PT_INTERP`; the opt-in target runs a host-glibc hello through `ld.so`, relocations, and shared `libc` (`AETHER_GLIBC_OK`). Broader `dlopen` coverage remains planned | partial |
| HI-T04 | `make test`; planned `make linux-proc-test` | Boot fixture covers fork, blocking `wait4` status/reap plus nonchild/selector checks, static/dynamic exec argv/envp, and bounded same-session process groups; broader APIs/cases remain planned | partial |
| HI-T05 | `make test`; planned `make linux-thread-test` | Boot fixture clones a thread with a shared address space and TLS, joins through clear-child-tid, and exits cleanly; detach/affinity/rseq and multi-thread stress remain planned | partial |
| HI-T06 | `make test`, `make glibc-test`; planned `make linux-env-test` | Auxv and a fixed library path reach `ld.so`; host path leakage, passwd, locale, and `getcwd` tests remain planned | partial |
| HI-T07 | `make test`; planned `make linux-fd-test` | Boot fixture exercises open/read/seek/dup/fcntl/close, pipe2 write/read, and poll; bad-descriptor and bounce-buffer matrices remain planned | partial |
| HI-T08 | `make test`; planned `make linux-fs-test` | Boot fixture exercises newfstatat/fstat/statx/readlink and getdents64 on `/` and `/usr/bin`; mutations and outside-path denial remain planned | partial |
| HI-T09 | `make gate4-portal-test`, `make gate4-gui-portal-test` | Static exact-file grant read, bounded `.sctmp` write, commit-by-rename, read-back, revocation, post-revocation rename/read/write denial, and host-tested traversal/root/file/rights/wrong-temp/cross-grant/quota/discard rejection. The GUI feature additionally seeds a real XLSX, exposes `/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx` for argv, validates ZIP/XLSX save bytes before an immediate save marker, and performs kernel-driven post-exit revocation. The windowed GUI is complete at QEMU prototype scope; the desktop file chooser/D-Bus portal remains planned | partial |
| HI-T10 | `make test`, `make unittest` | VMA host tests; boot fixtures cover bounded private-anonymous demand paging, thread-shared anonymous mapping, file map/unmap/reuse, growing `brk`, `mremap`, W^X denial; cross-process shared mappings and quotas remain | partial |
| HI-T11 | `make test`; planned `make linux-sync-test` | Boot fixture blocks on FUTEX_WAIT and wakes through FUTEX_WAKE during thread join; timeouts, robust lists, and condition variables remain planned | partial |
| HI-T12 | `make test`; planned `make linux-signal-test` | Boot fixtures cover self SIGUSR1 handler/return and cross-process SIGUSR1; siginfo/ucontext completeness, alt-stack delivery, and restart semantics remain planned | partial |
| HI-T13 | planned `make linux-time-test` | Monotonic/realtime clocks, sleep, poll timeout, and timer cancellation | planned |
| HI-T14 | planned `make linux-random-test` | `getrandom` fills a bounded buffer and fails closed at the domain boundary | planned |
| HI-T15 | `make test` | `AETHER_LINUX_PROC_OK` and `AETHER_LINUX_DEV_OK`: `/proc/self/{exe,maps,status,fd}` and `/dev/{null,zero,random,urandom}` through open/read/stat/readdir; no host process or device enumeration | partial |
| HI-T16 | `make test` | Boot fixture polls a pipe and drives eventfd through epoll. `AETHER_LINUX_INOTIFY_BLOCK_OK` covers blocking/nonblocking inotify, multiple file and directory watches, `IN_CREATE`/`IN_MODIFY`/`IN_OPEN`/`IN_IGNORED`, overflow, and a blocking wakeup. Blocking poll/epoll waits and timeouts remain planned | partial |
| HI-T17 | planned `make linux-socket-test` | Local socket pairing and `SCM_RIGHTS` validation; no network sockets | planned |
| HI-T18 | planned `make linux-net-deny-test` | AF_INET/AF_INET6 and DNS fail closed without a capability | planned |
| HI-T19 | planned `make wayland-test` | Registry, surfaces, commits, frame callbacks, xdg-shell toplevel, and teardown | planned |
| HI-T20 | planned `make wayland-protocol-test` | Required protocol subset and clean rejection of unknown/oversized messages | planned |
| HI-T21 | planned `make wayland-buffer-test` | Size/stride/format/lifetime validation and denial of compositor or DRM access | planned |
| HI-T22 | `make iso-test` plus planned `make linux-input-test` | Keyboard/pointer/focus events reach the Linux surface and never another domain | partial |
| HI-T23 | planned `make linux-gl-test` | Software GL context, swapchain presentation, and clean failure without a renderer | planned |
| HI-T24 | planned `make linux-vulkan-test` | lavapipe enumeration, `VK_KHR_wayland_surface`, swapchain, and readback checksum | planned |
| HI-T25 | planned `make linux-x11-deny-test` | With `DISPLAY` set, the runtime still refuses X11 and reports a Wayland-only error | planned |
| HI-T26 | planned `make linux-dbus-test` | Broker handshake, allowlisted portal method, denied arbitrary name/method, peer credentials | planned |
| HI-T27 | planned `make linux-audio-deny-test` | ALSA/PipeWire access fails closed without an audio grant | planned |
| HI-T28 | planned `make linux-device-deny-test` | `/dev/dri`, `/dev/input`, block devices, and raw PCI are absent or denied | planned |
| HI-T29 | planned `make linux-crash-test` | Fault retires only the domain; supervisor restarts within budget; quotas are enforced | planned |
| HI-T30 | planned `make linux-limit-test` | Address-space, thread, fd, and CPU limits return the documented error and do not leak authority | planned |
| HI-T31 | `make compat-test` + `make gate3-test` + `make gate4-test` | GridCraft is `limited` with named Gate 3 and Gate 4 evidence and is not upgraded above `limited` while the Gate 2 production follow-up is open | existing |
| HI-T32 | planned `make linux-clipboard-deny-test` | No clipboard grant is advertised; the application starts and copy/paste fails cleanly | planned |

## Decisions

These decisions close the Gate 1 policy questions for the first runtime profile.
Changing one requires an ADR update, because each removes ambient authority that
a compatibility shortcut would otherwise reintroduce.

| ID | Decision | Rationale | Affected |
|---|---|---|---|
| D-01 | **Wayland only.** Unset `DISPLAY`, expose only a Wayland socket, and fail cleanly if the application insists on X11. No X11 server or XCB connection path. | The payload's X11 fallback would add a second window-system surface and another ambient input path. Wayland is the smaller, reviewable boundary. | HI-25, HI-T25 |
| D-02 | **Private per-domain D-Bus broker.** Allowlist the portal interfaces, reject service activation, arbitrary name ownership, and eavesdropping. | A shared session bus is ambient IPC and a data-exfiltration channel. The broker keeps the portal path without granting general bus access. | HI-26, HI-27, HI-T26 |
| D-03 | **Same-domain spawn only.** A child inherits the verified root, overlay, capabilities, and quota. `setuid`, `setgid`, `setgroups`, and `chroot` return `EPERM`; no host path or new capability is inherited. | `execvp`/`posix_spawnp` are imported, but compatibility does not require privilege changes. Keeping the domain identity fixed preserves the application boundary. | HI-04, HI-T04 |
| D-04 | **No clipboard in the first profile.** Do not advertise `wl_data_device_manager`; copy/paste fails cleanly. Revisit after Gate 4 with a revocable grant. | The clipboard is a cross-domain data path and is not needed to prove the spreadsheet runtime. | HI-32, HI-T32 |
| D-05 | **Fixed path namespace.** User files are reachable only through portal paths or handles; `/home/user` is the per-app overlay, never the host home directory. | File authority must come from portal grants, not from a path string the application chooses. | HI-09, HI-T09 |
| D-06 | **Fixed environment and locale.** `C.UTF-8`, UTC, a bundled zoneinfo subset, and the environment in the table below; no host environment inheritance. | Host locale, timezone, and environment are ambient inputs that can change behaviour and leak host state. | HI-06, HI-T06 |
| D-07 | **Software Vulkan only.** Pin lavapipe with `VK_ICD_FILENAMES`; no `/dev/dri`, GPU passthrough, or host GL driver. | The first GUI milestone needs a reviewable renderer, not device ownership. Software rendering keeps the GPU outside the trusted path. | HI-23, HI-24, HI-T23, HI-T24 |
| D-08 | **Network denied.** AF_INET/AF_INET6 and DNS fail closed; AF_UNIX is allowed only to the Wayland and D-Bus brokers. | The application has no network permission in the first profile, and a local-socket bridge would recreate one. | HI-17, HI-18, HI-T17, HI-T18 |

### Path namespace

The runtime does not mount the host filesystem. It assembles the application
view from the signed runtime rootfs, the per-app overlay, and portal handles.

| Guest path | Backing | Authority |
|---|---|---|
| `/usr`, `/lib`, `/lib64`, `/etc` (selected), `/share` | Signed runtime rootfs | Read-only |
| `/` | Runtime rootfs view | Read-only; no host root |
| `/tmp`, `/var/tmp` | Per-domain overlay | Read/write, cleared on exit or fault |
| `/home/user` | Per-domain overlay | Read/write, private to the domain |
| `/run/aether/portal/<grant-id>/<file>` | Portal grant backing path | Read or read+write only for the exact granted file and caller root; revoked grants fail the next open |
| `/run/aether/portal/<grant-id>/<file>.sctmp` | Bounded RAM staging slot | Write-only save staging; commits only by rename to the exact granted file, discarded by unlink, and never a separate user file |
| `/proc/self` | `linuxd` synthesis | Read-only, bounded |
| `/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom` | Synthetic nodes | Fixed behaviour |
| `/run/user/1000` | Per-domain runtime directory | Wayland and D-Bus sockets only |
| `/dev/dri`, `/dev/input`, block devices, `/sys` | Not present | Denied |

### Runtime environment

The runtime profile sets these values explicitly and does not inherit host
variables. Values that name a resource point at the private namespace.

| Variable | Value |
|---|---|
| `HOME` | `/home/user` |
| `TMPDIR` | `/tmp` |
| `XDG_RUNTIME_DIR` | `/run/user/1000` |
| `XDG_CONFIG_HOME` | `/home/user/.config` |
| `XDG_CACHE_HOME` | `/home/user/.cache` |
| `XDG_DATA_HOME` | `/home/user/.local/share` |
| `WAYLAND_DISPLAY` | `wayland-0` |
| `DISPLAY` | Unset |
| `DBUS_SESSION_BUS_ADDRESS` | Private broker socket under `/run/user/1000` |
| `LANG`, `LC_ALL` | `C.UTF-8` |
| `TZ` | `UTC` |
| `VK_ICD_FILENAMES` | Pinned lavapipe ICD in the runtime rootfs |
| `LD_LIBRARY_PATH` | Unset; library search is constrained to the runtime rootfs |

## Harness plan

Every planned test uses one of four harness families. The QEMU family reuses
the existing serial-marker gate: a run fails when its `AETHER_LINUX_<SUITE>_OK`
marker is missing, when any `AETHER_LINUX_*_FAIL` marker appears, or when the
payload exits non-zero. No test is evidence until it runs in `make ci`.

| Family | Location | Existing targets | Planned targets |
|---|---|---|---|
| Host unit | `apps/linux/*`, `kernel/memory`, `kernel/fs` | `make appimage-test`, `make squashfs-test`, `make unittest` | ABI policy and path-translation unit tests inside `make unittest` |
| QEMU Linux payload | `tests/fixtures/linux/`, `tools/build-gate3-*.sh` | `make test` (static fixture), `make gate3-test` (real GridCraft CLI) | `linux-abi-test`, `linux-proc-test`, `linux-thread-test`, `linux-env-test`, `linux-fd-test`, `linux-fs-test`, `linux-portal-test`, `linux-mem-test`, `linux-sync-test`, `linux-signal-test`, `linux-time-test`, `linux-random-test`, `linux-procfs-test`, `linux-poll-test`, `linux-socket-test`, `linux-net-deny-test`, `linux-gl-test`, `linux-vulkan-test`, `linux-x11-deny-test`, `linux-dbus-test`, `linux-audio-deny-test`, `linux-device-deny-test`, `linux-crash-test`, `linux-limit-test`, `linux-clipboard-deny-test` |
| Desktop integration | `tools/desktop_smoke.py` | `make iso-test` | `linux-input-test`, `wayland-test`, `wayland-protocol-test`, `wayland-buffer-test` |
| Compatibility database | `tests/compatibility` | `make compat-test` | None; the Gate 3/Gate 4 record is `limited` and cannot rise above that while the Gate 2 production follow-up is open |

The target names above are reserved for the Linux layer; `make gate3-test` is
the first Gate 3 harness, and the remaining planned targets are added only with
their harness. A target must fail closed when the marker is absent.

## Security criteria review

The mapping covers each acceptance criterion in
[Linux compatibility](linux-compatibility.md#security-acceptance-criteria):

| Criterion | Facilities | Tests |
|---|---|---|
| Importer validates bounds before parsing | HI-01 | HI-T01 |
| Root is read-only; writes use the overlay | HI-02, HI-08, HI-10 | HI-T02, HI-T08, HI-T10 |
| User files only through portal grants | HI-09, HI-27 | HI-T09 |
| Network, audio, and devices denied; local sockets brokered | HI-17, HI-18, HI-28, HI-29 | HI-T17, HI-T18, HI-T27, HI-T28 |
| Crash terminates only the domain | HI-30 | HI-T29 |
| Malformed ELF, library, or Wayland message cannot escape | HI-03, HI-20, HI-21 | HI-T03, HI-T20, HI-T21 |
| AppImages and runtime packages are verified before use | HI-01, HI-02 | HI-T01, HI-T02 |
| No record upgrade without a completed run | HI-31 | HI-T31 |

The review confirms the inventory has no facility that requires ambient host
authority. The remaining risk is implementation, not an unmapped interface.

## Gate 1 acceptance checklist

- [x] Inspect the pinned GUI and CLI payloads and record direct dependencies.
- [x] Inventory imported libc symbols, `dlopen` targets, and protocol names.
- [x] Map every observed facility to a proposed Aether interface.
- [x] Assign an owner component to every facility.
- [x] Assign a test ID and pass condition to every facility.
- [x] Resolve the policy questions above.
- [x] Review the mapping against the runtime architecture and security criteria.
- [x] Confirm each planned test has a harness target and a failure mode.
- [x] Correct the stale blocker list and record the inventory as Gate 1 evidence
  in [Linux compatibility](linux-compatibility.md).

**Gate 1 is complete for the pinned artifact.** The 32 mapped facilities and
their test IDs are the Gate 2 backlog; implementation status does not change the
inventory result. Gate 3 has a completed headless run and Gate 4 has a
completed windowed run, so the GridCraft record is `limited`; the Gate 2
production follow-up is still required before any upgrade beyond that.

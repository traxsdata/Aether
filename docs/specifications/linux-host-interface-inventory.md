# Linux Host-Interface Inventory

**Document ID:** AETHER-LINUX-002
**Status:** Gate 1 inventory draft; no compatibility claim
**Related:** [Linux compatibility](linux-compatibility.md), [Linux layer](../architecture/linux-layer.md),
ADR-015, [system ABI](system-abi.md), [compatibility database](compatibility.md)

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

**Gate 1 is drafted, not accepted.** Every observed facility now has a proposed
Aether interface, an owner, and a test case; the acceptance checklist and the
policy questions at the end of this document are still open. Implementation
status is uneven:

| Status | Meaning | Facilities |
|---|---|---:|
| Existing | A tested interface exists and can be used by the runtime | 2 |
| Partial | Some interface or host tests exist; the runtime cannot use it yet | 7 |
| Missing | No interface exists | 18 |
| Denied | Deliberate absence; the runtime must fail closed | 2 |
| Deferred | Not required for Gate 3 or Gate 4 | 2 |

These counts are the planning baseline. They do not claim that the existing
interfaces are sufficient for GridCraft: Gate 2 proves that by running binaries.

## Facility inventory

Status values are `existing`, `partial`, `missing`, `denied`, or `deferred`.
Owners name the component that must provide the interface, including planned
user-space services. Test IDs are defined in the [test plan](#test-plan).

### Import, launch, and runtime environment

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-01 | AppImage container, digest, signed record, SquashFS import; `appimage-inspect`, `squashfs-extract` | `aether-appimage` + `aether-squashfs` + trust store | `apps/linux/*`, `aether-packaging` | HI-T01 | existing (fixture only) |
| HI-02 | Runtime rootfs: glibc 2.35, `ld.so`, `libm`, `libgcc_s`, Mesa, Wayland, D-Bus, X11 libraries | Signed runtime image installed as a package; version recorded in the database | `aether-packaging`, `tools/build-runtime-rootfs.sh` | HI-T02 | missing |
| HI-03 | Dynamic ELF loading: `PT_INTERP`, `ld-linux`, `dlopen`, `dlsym`, `dlclose`, `dl_iterate_phdr`, `__tls_get_addr` | `linuxd` loader plus a shared-library service over a read-only root | planned `user/linuxd`, `kernel/src/memory` | HI-T03 | missing |
| HI-04 | Process creation: `fork`, `execvp`, `posix_spawnp`, `posix_spawn_file_actions_*`, `waitpid`, `waitid`, `pidfd_spawnp`, `setsid`, `setpgid` | Domain-scoped process API over the kernel task model | `kernel/src/scheduler.rs`, planned `user/linuxd` | HI-T04 | missing |
| HI-05 | Threads: `pthread_create`, `pthread_join`, `pthread_detach`, `pthread_setname_np`, `sched_getaffinity`, `set_tid_address`, `rseq` | Thread primitive with per-thread kernel state | `kernel/src/scheduler.rs` | HI-T05 | missing |
| HI-06 | Environment and identity: `environ`, `getenv`, `getauxval`, `uname`, `sysconf`, `gnu_get_libc_version`, `getpwuid_r`, `getgrouplist`, `setlocale`, `realpath`, `getcwd` | Synthetic environment from the verified runtime profile; no ambient passwd or host paths | planned `user/linuxd` | HI-T06 | missing |
| HI-07 | Descriptor I/O: `open`/`open64`/`openat`, `read`, `write`, `writev`, `close`, `lseek64`, `dup`, `dup2`, `fcntl`, `ftruncate64`, `pipe2`, `poll` | Per-domain descriptor table over native file handles | `kernel/src/linux.rs`, `kernel/src/fs.rs` | HI-T07 | partial |

### Files, memory, and kernel primitives

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-08 | Metadata and directories: `stat64`, `statx`, `fstat64`, `readlink`, `mkdir`, `unlink`, `rename`, `opendir`, `readdir64`, `closedir`, `dirfd` | Filesystem service operations with per-domain roots | `kernel/fs`, `kernel/src/fs.rs` | HI-T08 | missing |
| HI-09 | User files: `HOME`, `open` of workbook paths, `org.freedesktop.portal.FileChooser`, save dialogs | Portal grant handles plus a synthetic `/home/user` mapping | `kernel/src/portal.rs`, planned `user/portald` | HI-T09 | missing |
| HI-10 | Memory: `mmap64`, `mprotect`, `munmap`, `posix_memalign`, `malloc`; file mappings; W^X | Memory-object capabilities and validated mapping syscalls | `kernel/src/memory`, `kernel/src/syscall.rs` | HI-T10 | partial |
| HI-11 | Synchronization: `futex`, pthread mutex/condition variables, robust lists, `eventfd` | Futex wait/wake queues with timeouts | `kernel/src/scheduler.rs`, `kernel/src/ipc.rs` | HI-T11 | partial |
| HI-12 | Signals: `sigaction`, `signal`, `sigemptyset`, `sigaddset`, `sigaltstack`, `kill`, `pause` | Fault-to-signal delivery and per-thread masks | `kernel/src/scheduler.rs`, planned `user/linuxd` | HI-T12 | missing |
| HI-13 | Time and waits: `clock_gettime`, `clock_nanosleep`, `poll` timeouts, `timerfd` | Time service plus a blocking wait queue | `kernel/src/scheduler.rs`, `kernel/src/syscall.rs` | HI-T13 | partial |
| HI-14 | Random: `getrandom` | Kernel random source | `kernel/src/syscall.rs` | HI-T14 | partial |
| HI-15 | Pseudo-files: `/proc/self/{exe,maps,status,fd}`, `/dev/{null,zero,random}`, bounded `/sys` | `linuxd` synthesis plus fixed device nodes | planned `user/linuxd` | HI-T15 | missing |
| HI-16 | Multiplexing and notification: `poll`, `epoll`, `eventfd`, `inotify` | Bounded wait/notification service | planned `user/linuxd`, `kernel/src/ipc.rs` | HI-T16 | missing |
| HI-17 | Unix sockets: `socketpair`, `connect`, `bind`, `listen`, `accept4`, `sendmsg`, `recvmsg`, `getsockopt`, `setsockopt` | Brokered local sockets with validated `SCM_RIGHTS` | planned `user/linuxd`, `kernel/src/ipc.rs` | HI-T17 | missing |
| HI-18 | Network: `socket`, `getaddrinfo`, `freeaddrinfo`, `gai_strerror`, `shutdown`; AF_INET/AF_INET6 | Network service with explicit capability | planned network service | HI-T18 | denied |

### Display, input, and graphics

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-19 | Wayland client transport: `libwayland-client.so.0`, `wl_display_connect`, `wl_display_*`, event queues, fd passing | `waylandd` service and per-domain socket | planned `user/waylandd` | HI-T19 | missing |
| HI-20 | Wayland protocols: `wl_compositor`, `wl_surface`, `wl_shm`, `wl_region`, `wl_callback`, `wl_seat`, `wl_keyboard`, `wl_pointer`, `wl_output`, `wl_data_device`, `xdg_shell`, `xdg_popup`, `xdg_activation`, `wp_viewporter`, `wp_fractional_scale`, `wp_cursor_shape` | Protocol subset implemented by `waylandd` | planned `user/waylandd` | HI-T20 | missing |
| HI-21 | Shared-memory buffers: `wl_shm`, `wl_shm_pool`, `wl_buffer`, `wl_egl_window_*` | Buffer validation (size, stride, format, lifetime) plus compositor presentation | planned `user/waylandd`, `kernel/src/desktop.rs` | HI-T21 | missing |
| HI-22 | Input and clipboard: keyboard, pointer, touch, keymap from `libxkbcommon`, cursor themes, `wl_data_device`/`arboard` | Input protocol, keymap data, and a revocable clipboard grant | `kernel/input`, planned `user/waylandd` | HI-T22 | partial |
| HI-23 | EGL/OpenGL ES: `libEGL.so.1`, `EGL_*` platform extensions, `wl_egl_window` | Software GL service (llvmpipe) over the Wayland surface | `kernel/src/graphics.rs`, planned graphics service | HI-T23 | missing |
| HI-24 | Vulkan: `libvulkan.so.1`, `VK_KHR_surface`, `VK_KHR_wayland_surface`, `VK_KHR_swapchain`, lavapipe | Pinned software Vulkan ICD inside the runtime; no `/dev/dri` | planned graphics service, `user/linuxd` | HI-T24 | missing |
| HI-25 | X11/XCB fallback: `libX11.so.6`, `libX11-xcb.so.1`, `libxcb.so.1`, `libXcursor`, `libXi`, `libxkbcommon-x11`, `DISPLAY` | No X11 server; Wayland-only policy with a clean failure | planned `user/linuxd` | HI-T25 | deferred |

### Services, lifecycle, and policy

| ID | Facility and artifact evidence | Aether interface | Owner | Test | Status |
|---|---|---|---|---|---|
| HI-26 | D-Bus session bus: `libdbus-1.so.3`, `org.freedesktop.DBus*`, `zbus` | Per-domain message broker with an allowlisted portal interface | planned `user/dbusd` | HI-T26 | missing |
| HI-27 | XDG desktop portal: `org.freedesktop.portal.Desktop`, `FileChooser`, `Request` | Portal UI and grant records exposed through the broker | `kernel/src/portal.rs`, planned `user/portald` | HI-T09, HI-T26 | missing |
| HI-28 | Audio: ALSA/PipeWire symbols; no audio path observed in the CLI | Audio service only when a capability is granted | planned audio service | HI-T27 | deferred |
| HI-29 | Devices and GPU nodes: no `/dev/input`, `/dev/dri`, block devices, or raw PCI | Deny by default; no device capability in the first profile | `kernel/services`, planned `user/linuxd` | HI-T28 | denied |
| HI-30 | Lifecycle and limits: crash containment, restart budget, address-space/thread/fd/CPU quotas, `prlimit64`, `sched_*` | Supervisor plus per-domain resource accounting | `kernel/src/supervisor.rs`, `kernel/src/scheduler.rs` | HI-T29, HI-T30 | partial |
| HI-31 | Compatibility record: exact digest, runtime version, named tests | Database schema and classification rules | `tests/compatibility` | HI-T31 | existing (record stays `unsupported`) |

## Test plan

A test ID is a commitment to a harness, a pass condition, and a failure mode. A
planned test is not evidence until it runs in `make ci`.

| ID | Harness | What must pass | Status |
|---|---|---|---|
| HI-T01 | `make appimage-test`, `make squashfs-test` | Container and rootfs parsing, digest/signature checks, extraction limits, hostile archives | existing |
| HI-T02 | `make test`, `tools/build-runtime-rootfs.sh` | Signed rootfs packaging and import markers; the current run covers the embedded fixture, not the populated glibc/Mesa image | existing (fixture) |
| HI-T03 | planned `make linux-abi-test` | `ld.so` maps a dynamically linked binary, resolves a library from the verified root, runs a constructor, and calls `dlopen`/`dlsym` | planned |
| HI-T04 | planned `make linux-proc-test` | Spawn, wait, exit status, descriptor inheritance, and denied spawn outside the domain | planned |
| HI-T05 | planned `make linux-thread-test` | Create/join/detach, per-thread TLS, affinity mask, timeout, and clean domain exit | planned |
| HI-T06 | planned `make linux-env-test` | Synthetic environment, auxv, `uname`, passwd substitute, and no host path leakage | planned |
| HI-T07 | planned `make linux-fd-test` | Read/write/seek/dup/pipe/poll on read-only root and overlay; bad descriptor rejection | planned |
| HI-T08 | planned `make linux-fs-test` | Stat/readdir/readlink/mkdir/unlink/rename within the domain root; outside paths denied | planned |
| HI-T09 | planned `make linux-portal-test` | File chooser grant, exact-file open, revocation, denial, and no ambient home | planned |
| HI-T10 | planned `make linux-mem-test` | Anonymous and file-backed maps, protect, unmap, W^X, and out-of-quota failure | planned |
| HI-T11 | planned `make linux-sync-test` | Futex wait/wake, timeout, robust list, eventfd, and thread wake ordering | planned |
| HI-T12 | planned `make linux-signal-test` | Handler delivery, alt stack, mask, fault-to-signal, and no delivery across domains | planned |
| HI-T13 | planned `make linux-time-test` | Monotonic/realtime clocks, sleep, poll timeout, and timer cancellation | planned |
| HI-T14 | planned `make linux-random-test` | `getrandom` fills a bounded buffer and fails closed at the domain boundary | planned |
| HI-T15 | planned `make linux-procfs-test` | Synthesized `/proc` and `/dev` nodes; no host process or device enumeration | planned |
| HI-T16 | planned `make linux-poll-test` | Poll/epoll/eventfd/inotify bounds and wake behavior | planned |
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
| HI-T31 | `make compat-test` | GridCraft remains `unsupported` until a completed gate has named evidence | existing |

## Open policy questions

These need a decision before Gate 1 can be accepted. Each proposal preserves the
default-deny rule and avoids adding authority to make an application start.

1. **X11 fallback.** The payload contains an X11 backend. Proposal: run
   Wayland-only, unset `DISPLAY`, and fail cleanly if the application insists on
   X11. Do not write an X11 server for the first milestone.
2. **D-Bus.** A shared session bus is ambient IPC. Proposal: run a per-domain
   broker and allowlist only the portal interfaces Gate 4 needs. Reject arbitrary
   service names, activation, and eavesdropping.
3. **Process spawning.** `execvp` and `posix_spawnp` are imported. Proposal:
   allow spawning only inside the same verified application root, with the same
   capabilities. Deny `setuid`, `setgid`, `setgroups`, and `chroot` with `EPERM`;
   record any spawn the application actually needs during Gate 3.
4. **Clipboard.** A shared clipboard is a data-exfiltration path. Proposal:
   defer it until after Gate 4; the first profile has no clipboard grant and the
   application must tolerate that.
5. **User paths.** Proposal: map `HOME` and workbook paths through portal grants
   and the per-app overlay; never expose a host home directory or a global mount
   namespace.
6. **Language and time.** Proposal: pin a `C.UTF-8` locale, UTC, and a small
   read-only zoneinfo subset in the runtime rootfs; do not read host locale or
   timezone data.
7. **GPU.** Proposal: lavapipe only, with a pinned ICD. No DRM device, no GPU
   passthrough, and no fallback that opens `/dev/dri`.
8. **Network.** Proposal: deny AF_INET/AF_INET6 entirely for Gate 3/4. Local
   sockets are allowed only to brokered services, never as a generic bridge.
9. **Remaining blockers.** The stale blockers list in
   [Linux compatibility](linux-compatibility.md) still mentions a missing
   SquashFS reader; that should be corrected when Gate 1 is accepted.

## Gate 1 acceptance checklist

- [x] Inspect the pinned GUI and CLI payloads and record direct dependencies.
- [x] Inventory imported libc symbols, `dlopen` targets, and protocol names.
- [x] Map every observed facility to a proposed Aether interface.
- [x] Assign an owner component to every facility.
- [x] Assign a test ID and pass condition to every facility.
- [ ] Resolve the open policy questions above.
- [ ] Review the mapping against the runtime architecture and security criteria.
- [ ] Confirm each planned test has a harness target and a failure mode.
- [ ] Correct the stale blocker list and record the inventory as Gate 1 evidence
  in [Linux compatibility](linux-compatibility.md).

Until this checklist is complete, Gate 1 remains open and GridCraft remains
`unsupported`.

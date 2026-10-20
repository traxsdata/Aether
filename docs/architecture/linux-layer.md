# Linux compatibility layer

Aether's Linux layer runs unmodified Linux x86-64 applications, including
AppImages, without a virtual machine. It is a user-space compatibility service,
not a Linux kernel and not a second operating system. The goal is to provide
Linux semantics to the application while Aether keeps its own capability,
filesystem, lifecycle, and compositor boundaries.

The architectural decision is recorded in
[ADR-017](../decisions/ADR-017-linux-app-runtime.md); the requirement inventory
and gates are in
[Linux application compatibility](../specifications/linux-compatibility.md),
with per-facility owners and test cases in
[the host-interface inventory](../specifications/linux-host-interface-inventory.md).

## Layers

```text
 application (unmodified Linux ELF / AppImage payload)
        |
        v
 linuxd  ── ELF loader, libc ABI, /proc, signals, threads, Wayland client side
        |
        +── IPC ──> fsd / portal / waylandd / input / software renderer
        |
        v
 narrow Aether kernel primitives
 address space · thread create · futex wait/wake · fault delivery · syscall trap
```

### Application domain

Each application gets its own address space, private application root, writable
overlay, runtime profile, and capability table. The layer cannot see another
application's root or capabilities. A crash retires the domain, not the desktop.

### `linuxd`

`linuxd` is the Linux personality. It owns:

- AppImage import and the read-only application root;
- ELF and shared-library loading, `ld.so` interaction, TLS, and environment;
- Linux syscalls that can be emulated in user space;
- a bounded synthetic `/proc`, `/dev/null`, `/dev/urandom`, and similar nodes;
- thread creation requests, `futex` policy, and signal delivery;
- the client side of the Wayland protocol used by `winit`.

It never receives ambient filesystem, network, or device authority. It asks
`fsd` and the portal for handles, exactly like a native application.

### Kernel primitives

The kernel must expose a small set of operations that cannot be implemented
safely in user space: map/unmap/protect user pages, create and schedule
threads in an address space, wait and wake on a futex word with a timeout,
deliver faults as signals, and enter the Linux syscall path. These are
validated primitives, not Linux semantics. They are security-critical and need
their own threat-model review before they land.

### Wayland bridge

`waylandd` is an Aether service that speaks the subset of Wayland used by
`winit`/`egui`: registry, compositor, shared memory, xdg-shell, seat, output,
and clipboard. Buffers are validated for size, stride, format, and lifetime
before the Aether compositor presents them. The application does not receive a
framebuffer, a DRM device, or a GPU command stream.

### Graphics

The first GUI target uses a bundled software Vulkan (lavapipe) or software GL
path selected explicitly by the runtime profile. It is slow but reviewable and
does not require a GPU service. Accelerated graphics is a later service with
its own capability and device model. A missing renderer is a clean failure, not
an attempt to open a host device.

## Security controls

| Control | How it applies to the Linux layer |
|---|---|
| Capability model | The domain receives only the endpoints and handles named in its runtime profile. The layer cannot mint capabilities. |
| Application identity | The domain identity is derived from the verified package/AppImage, never from an application-provided string. |
| Private storage | The imported root is read-only; writes go to a per-application overlay. Other app roots are denied by `fsd`. |
| File portal | User files are opened through explicit grants. A Linux path such as `/home/user/file.xlsx` is a runtime mapping, not host authority. |
| Network | Denied by default. Sockets fail with a clear error unless a network capability is granted. |
| Devices | No `/dev/dri`, `/dev/input`, block devices, or raw PCI access. Input and display are service protocols. |
| Resource limits | Address-space, thread, file, and CPU limits are enforced per domain; exhaustion retires the domain. |
| Update integrity | AppImages and runtime packages are digest- and publisher-verified before import. |
| Crash containment | A fault in the layer or application terminates only that domain; unrelated apps and the desktop continue. |

## Compatibility boundary

The layer is deliberately incomplete. Unsupported Linux behaviour must fail
closed with `ENOSYS` or the nearest Linux error, be recorded in the
compatibility database, and never be silently approximated in a way that grants
authority. A compatibility claim needs a completed run, a named test suite, the
exact AppImage digest, and the runtime version.

## Current state

- AppImage container inspection: implemented in `apps/linux/appimage`, with
  host tests, pinned digest verification, signed import records with Ed25519
  publisher/trust-store checks, and the `appimage-inspect` /
  `appimage-manifest` diagnostics.
- SquashFS/zstd import: implemented in `apps/linux/squashfs`, with a synthetic
  fixture, a real-AppImage host extraction, and `squashfs-extract`.
- Linux ELF loading into a private PML4: implemented for the boot test fixture.
- Dynamic image loading: `aether-linux-elf` parses `ET_DYN`/`ET_EXEC` and
  `PT_INTERP`; `kernel/src/linux_loader.rs` maps a main image and interpreter,
  builds the initial stack with auxv, and enters the interpreter, which hands
  off to the main entry. Relocations and shared-library loading are not part of
  this slice; the test interpreter only validates auxv and transfers control.
- Linux `syscall` entry: implemented for the tested ABI slices, including
  `mmap`/`munmap`/`mprotect`/`brk`, bounded private-anonymous demand paging,
  file-backed mappings, `mremap`, `clone` and bounded `clone3` threads, `fork`
  and blocking `wait4` (with nonchild/selector checks), static and dynamic
  `execve` with bounded argv/envp, bounded same-session process groups with
  `setpgid`/`getpgid`/`getsid`/`setsid` validation, and self/group and
  cross-process SIGUSR1 fixtures. Signal delivery builds an x86-64
  `rt_sigframe` with `siginfo_t` and `ucontext_t`, including
  `si_signo`/`si_code`/`si_pid`/`si_uid` for `kill`/`tgkill`, fault addresses,
  full GPR and signal-mask restore, `SA_SIGINFO`, `SA_ONSTACK` with safe
  fallback, and `SA_RESTART` for `wait4`. Default termination for
  `SIGKILL`/`SIGTERM` and default-ignored `SIGCHLD` are bounded to one thread
  group and session. Robust futex registration and exit-time
  `FUTEX_OWNER_DIED` wakeup are implemented for the exact 24-byte x86-64 head
  layout; `rseq` registration validates and stores a bounded area but never
  reports preemption. Descriptor/filesystem calls include `openat`, read/write,
  stat/statx, `readlink`, `fcntl`, dup, `getdents64`, pipes, `poll`,
  eventfd/basic epoll, and narrow inotify. Eventfd counters are shared by
  threads in one tgid; futex timeouts are implemented. Blocking waits are
  scheduler-parked: `poll` and `epoll_wait` accept finite and infinite
  timeouts, pipe storage is global so `fork`-inherited descriptors block and
  wake across processes, `epoll` reports level-triggered readiness, a bounded
  edge-transition slice, and one-shot re-arm, and `nanosleep`/
  `clock_nanosleep` implement relative and absolute deadlines with `EINTR`.
  `timerfd_create`/`settime`/`gettime` provide one-shot and periodic monotonic
  timers visible through `poll`/`epoll`. Inotify supports default blocking and
  `IN_NONBLOCK` instances, up to four watches per process, events on exact
  overlay files and on read-only/explicit `/tmp` directories (`IN_MODIFY`,
  `IN_ATTRIB`, `IN_OPEN`, `IN_CLOSE_WRITE`, `IN_CLOSE_NOWRITE`, `IN_CREATE`,
  `IN_IGNORED`), a bounded eight-event queue with `IN_Q_OVERFLOW`, and blocking
  reads woken by another `CLONE_VM` thread. Threads share the per-tgid queue;
  `fork` takes a snapshot, so cross-process sharing is not implemented. A
  bounded synthetic `/proc/self/{exe,maps,status,fd}`, `/proc/aether/syscalls`,
  and `/dev/{null,zero,random,urandom}` are served through
  open/read/stat/readdir; `/sys` and other device nodes are absent. Named
  fixture markers are enumerated in the roadmap and host-interface inventory.
  Shared mappings, cross-process eventfd counters, nested epoll watches,
  recursive/move/delete inotify events, high-resolution timers,
  stopped/continued `wait4`, and signal stopping remain missing.
- A fixed per-task VMA table tracks ranges and rejects writable+executable
  mappings. Anonymous private pages are faulted in on demand with bounded
  allocation and zero-fill; mapped pages are reclaimed on unmap/task exit.
  The root is imported read-only. `/tmp`, `/settings`, and `/logs` map to a
  per-application overlay store with a 1 KiB quota per application and a
  512-byte per-file limit. The store is shared by processes of the same
  application, survives task exit within a boot, and is erased through
  `linux-erase <app>`; the boot test checks persistence and erasure. It is a
  RAM-backed store: it does not survive a power cycle because no persistent
  block device exists.
- Runtime rootfs packaging and signing: `tools/build-runtime-rootfs.sh` builds
  a zstd SquashFS image and signs it as a `linux-rootfs` record.
  `tools/build-pinned-runtime.sh` plus `make runtime-rootfs-pinned` now produce
  the profile the boot test consumes. It is a **host-derived pin**: the host's
  x86-64 glibc and loader are copied, their SHA-256 values and version are
  recorded in `etc/aether-runtime.manifest`, and the manifest digest is written
  to a build pin. The kernel recomputes the digest and refuses to boot on a
  mismatch (`AETHER_RUNTIME_ROOT_OK`). Mesa and Wayland are recorded as absent;
  no unpinned download is used. A different host produces a different pin.
  `tools/build-glibc-rootfs.sh` plus `make glibc-test` remain the opt-in
  host-glibc hello smoke test.
- In-OS import: the kernel embeds a small signed `linux-rootfs` SquashFS image,
  verifies it against the test trust store, imports it with a 4 MiB bootstrap
  bump heap, and the Linux file syscalls read from the imported table
  (`AETHER_LINUX_IMPORT_OK`). A second, differently identified AppImage is
  staged by the UEFI loader, not embedded in the kernel: the loader reads
  `\linux\app.AppImage` and `\linux\app.import` from the boot volume, retains
  the buffers below 4 GiB, and publishes their physical extents in `BootInfo`
  v4 (`linux_appimage`, `linux_appimage_record`). The kernel verifies the
  signed `linux-appimage` record and image digest against the trust store
  before the container parser or SquashFS importer sees them, then imports the
  payload and launches `/usr/bin/app`
  (`AETHER_LINUX_APPIMAGE_STAGED_OK source=loader`,
  `AETHER_LINUX_APPIMAGE_OK`, `AETHER_LINUX_APPIMAGE_EXEC_OK`). The identity
  and version come from the signed record, not kernel constants, and the
  AppImage runtime is never executed. A missing or tampered file fails closed
  before parsing or launch. This is still a boot-volume artifact with no
  user-facing file picker or persistent block device.
- SquashFS reader and extraction: implemented in `apps/linux/squashfs` for
  zstd, gzip, lz4, lzo, xz, and lzma in `no_std` (metadata, file data,
  fragments, directories, symlinks, hardlink marking, xattr policy and
  extraction limits, host extractor). The importer applies an explicit xattr
  allowlist: only `user.*` is retained, while `security.capability`, other
  `security.*`, `trusted.*`, `system.*`, and unknown namespaces are dropped and
  counted. The host extractor does not apply retained attributes. Embedded signed-rootfs in-OS import is
  implemented; user-supplied AppImage import/opening and power-cycle-persistent
  overlay storage remain missing.
- `linuxd`: a bounded kernel-side supervisor service prototype owns runtime
  profile selection, verified AppImage identity, and launch policy
  (`AETHER_LINUXD_OK`). It is registered with the supervisor and exposes
  `linuxd` status. It is **not** yet a ring-3 process and Linux syscalls are
  still dispatched by the kernel; the user-space service boundary and syscall
  remoting remain Gate 2 work.
- Wayland bridge, software renderer, and Mesa/Wayland runtime content: not
  implemented.

The compatibility database records GridCraft 0.8.0 as `unsupported` until these
gates pass. A static test program is not an ArtCraft application.

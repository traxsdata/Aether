# Roadmap

This roadmap follows the phased plan and backlog execution rule in the
[technical specification](../Technical%20Specification%20and%20Initial%20Development%20Backlog.txt).
It is a planning model, not a delivery promise.

## Phase 0 — Specification and proof of feasibility

- [x] Architecture decision records for kernel, microkernel, platform,
  language, system interfaces, application isolation, Windows compatibility,
  graphics, updates, and governance.
- [x] Threat model and security boundaries for the implemented boot, memory,
  scheduler, capability, and IPC paths.
- [x] Pinned toolchain and QEMU target.
- [x] Boot ABI and defensive image parser.
- [x] Broader system ABI, capability, and IPC specifications.
- [x] Windows compatibility feasibility plan
  ([`specifications/windows-compatibility.md`](specifications/windows-compatibility.md)).
  The host-interface report remains an Epic H deliverable.
- [x] Local CI gate and repository checks for pull requests.
- [ ] GitHub Actions workflow publication.

## Phase 1 — Bootable kernel

- [x] UEFI boot path.
- [x] Serial console and panic reporting.
- [x] Memory-map parsing and kernel hand-off.
- [x] Automated boot test and CI.
- [x] GDT and IDT with structured exception reporting.
- [x] Physical page allocator and kernel-owned page tables.
- [x] NX and supervisor write-protect enabled explicitly.
- [x] Architecture/memory initialization tests (host unit tests and QEMU
  exception-path test).
- [x] Per-address-space page tables (first Phase 2 slice).
- [ ] Higher-half kernel mapping.

## Phase 2 — Isolated execution

- [x] Static task/thread objects and preemptive timer-driven scheduling.
- [x] Per-task address spaces with CR3 switching and a hardware isolation test.
- [x] Timer interrupt and legacy PIC/PIT setup.
- [x] Capability table, rights checks, and generation-checked handles.
- [x] Endpoint identity checks and atomic capability receive.
- [x] Bounded IPC endpoints and kernel-mediated capability transfer.
- [x] Host tests for capability and IPC validation plus boot-time adversarial
  rights/forged-handle/endpoint-confusion checks.
- [x] User/kernel separation, TSS/RSP0, user page permissions, and a ring-3
  native init process.
- [x] Syscall gate with user pointer, length, handle, and rights validation.
- [x] Per-task kernel stacks for ring-3 interrupts and syscalls.
- [x] Fault containment: a ring-3 fault retires the task instead of the kernel.
- [x] Revocation and object-lifetime rules for every implemented object type:
  generation-checked object destruction for endpoints, per-table alias
  revocation, endpoint release on exit/fault, and file handles released on
  task retirement.
- [x] Blocking IPC with tick timeouts and scheduler wakeups, plus adversarial
  tests for stale aliases, closed endpoints, and timeout behavior.

## Phase 3 — Minimal operating system

- [x] Supervisor and service startup/failure rules with restart budgets and
  quarantine.
- [x] Virtual storage driver and filesystem service over a RAM block device.
- [x] File handles and per-process access checks with generation-checked
  capabilities.
- [x] Console shell and application lifecycle manager.
- [x] Security-domain implementation through private application roots and
  service capability profiles.
- [x] Private application storage and automated sandbox tests.
- [x] Boot markers for the native application, filesystem, supervisor, shell,
  and sandbox boundary.

## Phase 4 — Native desktop prototype

- [x] Graphics and input services over a linear GOP framebuffer, PS/2 keyboard
  (IRQ1), and mouse (IRQ12).
- [x] Double-buffered software compositor with a host-tested window manager,
  bounded windows, focus/z-order, Spaces, menus, dock, and search.
- [x] Desktop shell, file manager, settings, and Security Center views.
- [x] Read-only Files folder navigation, selection, keyboard scrolling, refresh,
  and bounded text previews; no implicit application-vault access.
- [x] Cached wallpaper, damage-tracked scene rendering/presentation, cursor-only
  updates, elapsed-tick animation, and real-input ISO regression tests.
- [x] File portal with per-process, per-file grant records; revocation blocks
  future portal opens (`AETHER_REVOCATION_OK`).
- [x] Interactive portal consent UI with explicit approve/deny controls,
  shell equivalents, and exact-file grant records persisted through the
  filesystem service.
- [ ] Durable grant storage across power loss: the record codec and
  `/system/portal/grants` store are implemented, but the only block device is
  the current RAM disk, so a reboot still starts from the seeded record.
- [x] Initial native application SDK (`sdk/`) and user syscall ABI.
- [x] System image and recovery prototype with current/previous selection and
  preserved user data.
- [x] Timestamped UEFI El Torito ISO build (`make iso`).
- [x] UEFI El Torito ISO boot test under OVMF (`make iso-test`) and VMware
  Workstation VMX generation (`make vmware-vmx`). A VMware Workstation smoke
  test remains pending; see [the VMware notes](reference/vmware-workstation.md).
- [ ] Ring-3 compositor/application views with capability-scoped surface/input
  IPC; current ring-0 adapters are trusted, not compromise-contained.
- [x] Files copy, move, rename, and delete, with service-scoped mutations,
  duplicate naming, generation-safe open-file checks, and a boot gate.
- [ ] Files tabs/split panes and safe import/export.
- [ ] GPU acceleration, audio, networking, and multi-window application
  rendering.

## Phase 5 — Compatibility and developer alpha

This phase has two compatibility tracks. Windows compatibility is the target in
technical specification §7; the Linux AppImage layer was added by
[ADR-017](decisions/ADR-017-linux-app-runtime.md) and is specified in
[Linux compatibility](specifications/linux-compatibility.md). The Linux track
does not replace Windows work, give native Aether applications a second ABI, or
change the Phase 4 exit criterion.

A checked box means the slice is implemented in this tree; behaviour changes
also have a named test or boot marker. Partial work stays unchecked and names
what is missing.

### Native packaging and documentation

- [x] Signed application package format, verifier, installation, and runtime
  profiles ([package format](specifications/package-format.md)).
- [x] Initial compatibility test suite and database with named-test evidence
  ([compatibility](specifications/compatibility.md)).
- [x] Developer documentation for packaging, updates, hardware, and recovery.

### Windows compatibility

- [x] Host-interface inventory and feasibility report
  ([report](specifications/windows-host-interface-report.md)). The feasibility
  gate is not met, so no runtime is promised.
- [ ] Windows compatibility prototype and per-application prefixes.

### Linux application layer

The layer runs unmodified Linux x86-64 applications, including AppImages, in
user space. It is not a VM. The target and security gates are in
[Linux compatibility](specifications/linux-compatibility.md); the design is in
[ADR-017](decisions/ADR-017-linux-app-runtime.md) and
[the Linux layer](architecture/linux-layer.md).

#### Gates

- [x] Gate 0 — AppImage import inspection: validate the container without
  executing it, with bounds, compression, table offsets, pinned SHA-256, and
  signed import records.
- [x] Gate 1 — host-interface inventory:
  [the Linux host-interface inventory](specifications/linux-host-interface-inventory.md)
  maps 32 facilities from the pinned GUI and CLI to owners and test IDs, with
  decisions for X11, D-Bus, process spawning, clipboard, paths, locale, GPU, and
  network. Gate 1 is not an implementation gate: remaining ELF/runtime,
  process/thread, signal, Wayland, and Vulkan interfaces remain Gate 2 work.
- [x] Gate 2 — interface mapping and prototype (QEMU prototype scope): the
  kernel primitives, signed import path, and ring-3 `linuxd` launch policy are
  boot-tested. Evidence includes demand paging, cross-process shared anonymous
  mappings, read-shared rootfs mappings with per-domain quotas, `mremap`, W^X
  denial, static and dynamic `execve` with argv/envp, blocking `wait4` and
  process groups, `siginfo`/`ucontext`/alt-stack/`SA_RESTART`, robust futex,
  `clone3`/`rseq`, blocking and finite `poll`/`epoll_wait`, cross-process pipes,
  `nanosleep`/`timerfd`, bounded pseudo-files and synthetic devices, inotify
  with directory watches and overflow, a default-deny syscall classifier with
  denial/audit evidence, a digest-pinned host-glibc runtime rootfs, a signed
  AppImage staged through the boot loader, and a ring-3 `linuxd` that must
  approve the launch. The dynamic `ET_DYN`/`PT_INTERP` path and an opt-in
  host-glibc `ld.so` hello also pass. This is a prototype, not production
  compatibility: Linux syscall dispatch, ELF/library mapping, and VMA
  management remain in the kernel, Mesa/Wayland content is absent, the overlay
  is RAM-backed, and an installed AppImage cannot yet be selected from a
  persistent device.
- [ ] Gate 2 production follow-up: move Linux ABI emulation, ELF/library
  loading, and syscall remoting into the ring-3 `linuxd`, pin a
  Mesa/lavapipe/Wayland runtime rootfs, and make AppImage installation and the
  overlay durable on a persistent block device. These remain required for a
  production compatibility claim; the Gate 3 prototype run below does not
  satisfy them.
- [x] Gate 3 — headless GridCraft (QEMU prototype scope): import
  `gridcraft-0.8.0-linux-x86_64.AppImage` and run its bundled
  `gridcraft-cli` unmodified in one process with a read-only root and private
  `/tmp` overlay. `make gate3-test` verifies the exact digest
  `942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`
  (19,024,376 bytes), launches `/usr/bin/gridcraft-cli` through `PT_INTERP`,
  evaluates `=1+1` (`2.0`) and `=SUM(B2:B3)` (`3`), writes and prints the CSV
  round trip (`item,value`, `alpha,1`, `beta,2`, `total,3`), and exits zero.
  The kernel emits `AETHER_GATE3_APP_OK` only after the app exits successfully,
  and derives `AETHER_GATE3_DENIAL_OK portal=none native=denied root=readonly
  overlay=private` from the task table. The program bytes come from the
  verified AppImage; glibc, `ld.so`, `libm`, and `libgcc_s` come from the
  signed host-derived Gate 3 runtime rootfs. The imported AppImage tree is not
  yet merged into a per-application visible root, so this is a prototype run,
  not a production claim.
- [ ] Gate 4 — windowed GridCraft: run `usr/bin/gridcraft` with Wayland,
  software rendering, keyboard/pointer input, and workbook open/save through
  the portal.
  - [x] Gate 4 portal slice (QEMU prototype scope): `make gate4-portal-test`
    seeds an exact-file read+write grant for
    `/run/aether/portal/dd8f879ba78d207c/gate4.xlsx`, launches a small static
    Linux AppImage fixture from the signed import path, reads the workbook
    fixture, writes and renames a bounded `.sctmp` staging file to commit new
    bytes to `/home/gate4.xlsx`, reads the committed bytes back, revokes the
    grant through a test-only kernel control path, and requires fresh read and
    write opens plus a post-revocation rename to fail closed. The kernel emits
    `AETHER_GATE4_PORTAL_OK`, `AETHER_GATE4_SAVE_OK`, and
    `AETHER_GATE4_REVOKE_OK` only after validating the read, the staging write,
    the single successful rename, the saved bytes in the kernel filesystem,
    and the two post-revocation denials. Path parsing, temp/commit resolution,
    and quota policy have host tests in `aether-services`; traversal, duplicate
    slashes, extra components, overlong names, caller-root mismatch, file
    mismatch, wrong temp suffix, cross-grant rename, revocation, and temp quota
    are covered. The launch environment also carries `AETHER_PORTAL_WORKBOOK`
    for a future GUI launch.
  - [x] Gate 4 GUI portal contract (QEMU prototype scope):
    `make gate4-gui-portal-test` runs a static XLSX read/stage/commit fixture
    against the `gate4-gui-test` feature. It seeds the real 4,089-byte XLSX at
    `/home/gate4-gui.xlsx` for the demo root, exposes
    `/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx`
    (grant id `5b428ae53860eb72`) through `AETHER_PORTAL_WORKBOOK`, validates
    the committed ZIP/XLSX bytes (`PK\x03\x04`, size > 1024), and emits
    `AETHER_GATE4_SAVE_OK` immediately on the successful `.sctmp` rename.
    After the app exits, the kernel revokes the grant itself, attempts a fresh
    read, temp open, and rename, and emits `AETHER_GATE4_REVOKE_OK` only after
    all three deny. The windowed GridCraft target consumes this feature with
    the same namespace path as argv.
  - [ ] Gate 4 GUI remains open: the Wayland bridge, compositor presentation,
    software Vulkan/GL, keyboard/pointer input, the real windowed
    `usr/bin/gridcraft` run, and a pixel/checksum plus input-trace record have
    not been implemented. The workbook does not yet travel through a desktop
    file chooser or D-Bus portal.
- [x] Moved the GridCraft record from `unsupported` to `limited` after the
  Gate 3 run, with the exact digest, runtime profile, named tests, platform,
  and date.
- [ ] Raise the GridCraft record above `limited` only after the relevant gate
  has a completed run, named tests, the exact AppImage digest, the runtime
  version, and the platform/date. Gate 4 is still required for the GUI.

#### Import and runtime root

- [x] Validate AppImage type 2, ELF runtime, SquashFS bounds, and table offsets.
- [x] Pin and verify the artifact SHA-256 before parsing; a mismatch is rejected
  before the container is handed to a parser.
- [x] Signed import records binding identity, version, runtime, digest, and
  publisher with Ed25519, verified against a trust store, plus
  `appimage-manifest sign|verify`. The same record type covers
  `runtime=linux-rootfs`.
- [x] Bounded SquashFS 4 reader with zstd, gzip, lz4, lzo, xz, and lzma
  metadata/data/fragment decompression in `no_std`, inode and directory
  parsing, extraction limits, and a real-AppImage host extractor
  (`aether-squashfs`). Unsupported inode types are rejected.
- [x] Import policy: relative symlinks only, duplicate-path rejection, hardlinks
  marked and copied per path, setuid/setgid/sticky stripped, and extraction
  limits.
- [x] Explicit xattr policy: SquashFS xattrs are parsed with a `user.*`
  allowlist and a `security.*`/`trusted.*`/`system.*` denylist
  (`security.capability` in particular is never imported); retained and dropped
  values are reported and covered by host tests.
- [x] Import into a private read-only root; reject symlink escapes, hardlinks
  outside the root, duplicate paths, and oversized files.
- [x] In-OS import of an embedded signed rootfs: the kernel verifies the test
  `linux-rootfs` record, imports the SquashFS tree with a bounded bootstrap
  heap, and serves Linux file opens from that tree
  (`AETHER_LINUX_IMPORT_OK`, `AETHER_LINUX_ROOT_OK`).
- [x] Writable overlay: bounded entries under `/tmp`, created/truncated/read/
  written through the Linux file descriptors, with fault cleanup
  (`AETHER_LINUX_OVERLAY_OK`).
- [x] Overlay settings/logs paths, per-application quota, erasure, and
  persistence across task exit within one boot (`/tmp`, `/settings`, `/logs`;
  `linux-erase`; `AETHER_LINUX_OVERLAY_PERSIST_OK`,
  `AETHER_LINUX_OVERLAY_ERASE_OK`).
- [ ] Durable overlay across power loss. The filesystem service is still a RAM
  disk; this is blocked on the persistent block device in Platform follow-ups.
- [x] Runtime rootfs host packaging: `tools/build-runtime-rootfs.sh` builds a
  zstd SquashFS rootfs and signs it as a `linux-rootfs` import record.
- [x] Pinned runtime rootfs: `make runtime-rootfs-pinned` packages the
  host-derived glibc/`ld.so` profile with a SHA-256 version/build manifest and a
  signed `linux-rootfs` record; the kernel recomputes the manifest digest and
  reports the exact glibc version (`AETHER_RUNTIME_ROOT_OK`). The compatibility
  database records the profile. Mesa/lavapipe/Wayland content is still absent.
- [x] Loader-staged signed AppImage: the UEFI loader reads a distinct
  `\linux\app.AppImage` and its import record from the boot volume, publishes
  them through the boot ABI, and the kernel verifies digest and publisher
  before parsing or launch (`AETHER_LINUX_APPIMAGE_STAGED_OK`,
  `AETHER_LINUX_APPIMAGE_OK`, `AETHER_LINUX_APPIMAGE_EXEC_OK`). Missing and
  tampered staged files fail closed.
- [x] Real pinned GridCraft artifact: `make gate3-test` stages the 19,024,376
  byte GridCraft 0.8.0 AppImage, signs and verifies its import record, imports
  the SquashFS payload, and launches the bundled `gridcraft-cli` with a
  configured argv against the signed Gate 3 runtime rootfs
  (`AETHER_LINUX_APPIMAGE_STAGED_OK bytes=19024376`, `AETHER_GATE3_APP_OK`).
  The AppImage tool refuses any file whose SHA-256 differs from the pin.
- [ ] Merge the imported AppImage tree into the application's visible
  read-only root. Today the selected program is read from the verified
  AppImage, while data and libraries are resolved from the runtime rootfs; the
  CLI gate does not need the GUI's data files.
- [ ] Replace the 512 MiB fixed bootstrap heap. The allocation-only SquashFS
  importer accumulates every AppImage file and decompression buffer, so the
  real 19 MiB image forced a large static arena. A selective import of the
  launch program, or a reclaiming scratch allocator, should remove it.
- [ ] User-installed AppImage selection: no file picker, package channel, or
  writable install location exists yet; the AppImage is staged on the boot
  volume at build time.

#### Linux ABI

- [x] Static ELF loading into a private PML4, architectural `syscall` entry,
  validated user pointers, `write`/`exit`, and the `AETHER_LINUX_OK` boot gate.
  Linux tasks are denied the native `int 0x80` ABI.
- [x] Anonymous memory and TLS: reserved virtual window, `mmap`, `mprotect`,
  `brk`, `arch_prctl` FS base with context switching, `clock_gettime`,
  `getrandom`, `uname`, `getpid`/`gettid`, `set_tid_address`, and futex
  wait/wake across threads.
- [x] Read-only root file syscalls: validated absolute paths,
  `openat`/`read`/`pread64`/`close`/`lseek`/`access`, and a per-task descriptor
  table cleared on exit and fault.
- [x] Runtime frame allocator, per-task VMA table, eager file-backed `mmap`,
  `munmap` reclaim, `mprotect`, and growing `brk`; anonymous private pages are
  demand-allocated and zero-filled, then reclaimed on unmap or task exit.
- [x] `mremap` grow-in-place and may-move with content preservation.
- [x] Bounded demand paging for private anonymous mappings, with zero-fill and
  write/read fixture coverage (`AETHER_LINUX_DEMAND_PAGING_OK`), plus
  `MAP_SHARED|MAP_ANONYMOUS` visibility between `CLONE_VM` threads
  (`AETHER_LINUX_SHARED_MMAP_OK`). Reference-counted shared frames survive
  `fork` and are visible across processes (`AETHER_LINUX_SHARED_FORK_OK`);
  read-shared rootfs mappings alias a bounded page cache
  (`AETHER_LINUX_MAP_SHARED_FILE_OK`). Shared VMA and page quotas return
  `ENOMEM` and are reclaimed on unmap and task exit.
- [ ] File-backed shared writeback and shared `mremap`: writes affect the
  in-memory cache only, `msync` returns `EOPNOTSUPP`, and shared `mremap` is
  rejected.
- [x] Dynamic ELF slice: `ET_DYN` and `PT_INTERP` parsing, image mapping,
  auxv construction, runtime-rootfs interpreter resolution, and
  interpreter-to-main handoff (`AETHER_LINUX_DYN_*`).
- [x] Dynamic AppImage launch: the imported program is entered through the
  interpreter named by its `PT_INTERP`, with a compile-time `argv` and a fixed
  environment. The initial stack keeps argv/envp strings above the vector
  area, and `mprotect` now updates demand-paged ranges whose leaf entries do
  not exist yet. This is the path used by the real GridCraft CLI.
- [x] Host-glibc proof: `make glibc-test` builds a signed runtime rootfs from
  the host glibc and runs a dynamically linked hello through `ld.so`,
  relocations, and shared `libc` (`AETHER_GLIBC_OK`). This is not the pinned
  production runtime.
- [ ] Mesa/lavapipe/Wayland runtime content, package channel, broader
  `dlopen`/library coverage, and `LD_LIBRARY_PATH` policy. The pinned
  host-glibc runtime itself is implemented.
- [x] File metadata and descriptor calls: `stat`/`fstat`/`newfstatat`/`statx`,
  `readlink`, `getdents64`, `fcntl`, `dup`, `dup2`, `pipe2`, non-blocking
  `poll`, and descriptor table cleanup.
- [x] Overlay mutation and short I/O: `rename` and `unlink` are confined to the
  current application's `/tmp`, `/settings`, and `/logs` entries and refuse the
  read-only root with `EROFS`; `getcwd` reports `/`; `AT_EMPTY_PATH` `statx`
  delegates to `fstat`; and oversized `read`/`pread64`/`write` calls return a
  short count instead of `EINVAL`. The bounded overlay is 16 entries, 4 KiB per
  file, and 8 KiB per application, with the quota-denial fixture still
  exercised (`AETHER_LINUX_QUOTA_OK`).
- [x] Notification calls: non-blocking `poll`, `eventfd2`, and basic
  `epoll_create1`/`epoll_ctl`/`epoll_wait`.
- [x] Futex timed wait: the fixture verifies timeout with `ETIMEDOUT`
  (`AETHER_LINUX_FUTEX_TIMEOUT_OK`).
- [x] Robust futex lists: `set/get_robust_list` and owner-died wakeup on thread
  exit (`AETHER_LINUX_ROBUST_FUTEX_OK`). Condition variables and semaphores are
  user-space patterns over the implemented futex primitive; broader stress
  remains open.
- [x] Blocking and finite `poll`/`epoll_wait` over pipes, eventfds, and
  timerfds, including level-triggered re-reporting, a bounded edge-triggered
  slice, and one-shot re-arm (`AETHER_LINUX_POLL_BLOCK_OK`,
  `AETHER_LINUX_EPOLL_BLOCK_OK`). Cross-process pipe blocking read/write and EOF
  are covered (`AETHER_LINUX_PIPE_BLOCK_OK`). Eventfd counters are shared across
  threads in one tgid; fork retains a process-private counter snapshot.
- [ ] Nested epoll watches and complete cross-process edge refresh.
- [x] inotify: nonblocking and default-blocking instances, file and directory
  watches over the overlay and read-only root, `IN_MODIFY`/`IN_ATTRIB`/`IN_OPEN`/
  `IN_CLOSE_WRITE`/`IN_IGNORED`, bounded overflow, and a blocking read woken by
  a thread (`AETHER_LINUX_INOTIFY_OK`, `AETHER_LINUX_INOTIFY_BLOCK_OK`).
- [ ] Recursive watches, move/delete/unmount event families, cross-process watch
  sharing, and complete inotify semantics.
- [x] Thread creation: `clone` with `CLONE_VM|CLONE_THREAD`, per-thread
  kernel stacks, TLS, tgid/tid, and clear-child-tid join.
- [x] Process creation: eager-copy `fork`, blocking `wait4` with nonchild and
  selector checks, and descriptor inheritance.
- [x] Static `execve`: resolve a rootfs ELF, build a new address space, and
  enter its entry point.
- [x] Dynamic `execve` with bounded argv/envp propagation through a `PT_INTERP`
  fixture; bounded process groups within the same session.
- [x] Bounded `clone3`, `set/get_robust_list`, and `rseq` registration with
  explicit `ENOSYS`/`EINVAL` for unsupported layouts
  (`AETHER_LINUX_CLONE3_OK`, `AETHER_LINUX_ROBUST_FUTEX_OK`,
  `AETHER_LINUX_RSEQ_OK`). Cross-session and process-group validation is
  covered (`AETHER_LINUX_PROCESS_GROUP_KILL_OK`).
- [ ] Stopped/continued process waits (`WUNTRACED`/`WCONTINUED`), signal
  stop/resume, queued real-time signals, pidfds, and cgroup/namespace `clone3`
  flags.
- [x] Synchronization: futex wait/wake queues, timeout, robust owner-died
  handling, clear-child-tid join, thread wake ordering, and eventfd/timerfd
  readiness; condition variables and semaphores are user-space patterns over
  this primitive and broader stress remains open.
- [x] Signals: `rt_sigaction`/`rt_sigprocmask`/`sigaltstack`, self/group
  `kill`/`tgkill`, handler frames, `rt_sigreturn`, and fault-to-signal delivery.
- [x] Bounded `siginfo`/`ucontext` delivery for SA_SIGINFO handlers, alternate
  stack delivery with fallback, `SA_RESTART` versus `EINTR`, default `SIGTERM`/
  `SIGKILL` behavior, and cross-process delivery
  (`AETHER_LINUX_SIGINFO_OK`, `AETHER_LINUX_ALTSTACK_OK`,
  `AETHER_LINUX_RESTART_OK`, `AETHER_LINUX_SIGNAL_DEFAULT_OK`,
  `AETHER_LINUX_CROSS_PROCESS_SIGNAL_OK`).
- [ ] Remaining `siginfo` variants, signal stop/resume, and queued real-time
  signals.
- [x] Time and scheduling: `nanosleep` and `clock_nanosleep` with absolute/
  relative deadlines and `EINTR` (`AETHER_LINUX_NANOSLEEP_OK`), one-shot and
  periodic `timerfd` through `poll`/`epoll` (`AETHER_LINUX_TIMERFD_OK`),
  bounded affinity/rlimit queries, and descriptor/VMA/overlay/shared-page
  quotas (`AETHER_LINUX_QUOTA_OK`). CPU time quotas and high-resolution clocks
  remain open.
- [x] Pseudo-files and synthetic devices: `/proc/self/{exe,maps,status,fd}`,
  `/proc/aether/syscalls`, and `/dev/{null,zero,random,urandom}` through normal
  descriptors (`AETHER_LINUX_PROC_OK`, `AETHER_LINUX_DEV_OK`).
- [ ] Read-only `/sys` subset (none is required by the current fixture) and
  `/proc/self/fd` alias details beyond the tested listing.
- [x] Syscall policy: default-deny classification, `ENOSYS` for unknown calls,
  per-domain network/device denial, path/read-only/device denial tests, and
  per-class audit counters in `/proc/aether/syscalls`
  (`AETHER_LINUX_SYSCALL_POLICY_OK`, `AETHER_LINUX_DENIAL_OK`).

#### Display, input, and media

- [ ] Wayland service: registry, `wl_compositor`, `wl_surface`, `wl_shm`,
  xdg-shell, seat/keyboard/pointer, output, and clipboard.
- [ ] Validate shared-memory buffers (size, stride, format, lifetime) before the
  compositor presents them; no framebuffer or DRM/PCI access from the app.
- [ ] Input focus, keyboard layout, pointer capture, and drag-and-drop policy.
- [ ] XDG desktop portal subset for file open/save; all user files through
  `fsd`/portal grants, never an ambient home directory.
- [ ] Software renderer: bundle Mesa lavapipe (Vulkan) or llvmpipe (GL), expose
  `VK_KHR_wayland_surface`/EGL, and select it explicitly in the runtime profile.
- [ ] Accelerated GPU service is a later optimization, not a dependency for the
  first GridCraft run.

#### Application and denial tests

- [x] Denial evidence: no ambient network/device authority, no writable
  read-only root, no out-of-root paths, no native ABI from the Linux domain, and
  bounded descriptor/VMA/overlay/shared-page quotas
  (`AETHER_LINUX_SYSCALL_POLICY_OK`, `AETHER_LINUX_DENIAL_OK`,
  `AETHER_LINUX_QUOTA_OK`, `AETHER_LINUXD_DENY_OK`).
- [x] Gate 3 application denial evidence: the imported GridCraft task has an
  empty capability table, receives no portal handle, cannot use the native ABI,
  sees the imported tree as read-only, and writes only to its private overlay
  (`AETHER_GATE3_DENIAL_OK portal=none native=denied root=readonly
  overlay=private`).
- [ ] Dedicated crash-containment test: a Linux application fault retires only
  its domain and cannot mint capabilities or observe a sibling application root.
  The scheduler already contains Linux fault retirement, but there is no named
  Linux crash-domain boot test yet.

### Platform follow-ups

- [ ] Better storage (persistent block device), networking, audio, and
  accelerated graphics.
- [ ] External security review.

## Phase 6 — Public beta and release engineering

- [x] Stable supported-hardware list for the QEMU platform
  ([supported hardware](reference/supported-hardware.md)).
- [x] Signed release bundles and verification (`make release`,
  `make release-check`).
- [x] Signed update manifests, digest verification, rollback counters, version
  and channel policy ([signed updates](architecture/updates.md)).
- [x] Recovery workflow documentation and current/previous image selection
  ([installer and recovery](reference/installer-and-recovery.md)).
- [x] Public compatibility database format and records.
- [x] Security response process ([security response](security/security-response.md)).
- [x] Community contribution program ([community](community.md)).
- [x] Release qualification testing (`make qualify`).
- [ ] UEFI installer, persistent boot slots, and activation of a staged image.
- [ ] Secure Boot signing, measured boot, and an immutable rollback counter.
- [ ] Accessibility and usability review.
- [ ] External security review and a public beta.

## Current milestone

The tree implements the Phase 2 object-lifetime and blocking-IPC slices and
the Phase 4 consent and file-mutation slices, but the Phase 4 exit criterion is
still pending because application views remain ring-0 adapters. The
implementable Phase 5/6 slices are in place on the QEMU platform: a clean
checkout builds a FAT image and a UEFI ISO, boots through the loader, enforces
a ring-3 user/kernel boundary, runs a native application in a private storage
root, reports denied cross-application access, verifies and installs a signed
package, derives an isolated runtime profile, verifies a signed update while
rejecting replay, downgrade, and an unknown publisher, starts supervised
services, paints a double-buffered desktop with a tested window manager,
accepts keyboard and PS/2 mouse input, grants one file through the portal,
queues and decides consent requests, copies/renames/deletes scoped files,
boots the UEFI ISO under OVMF, and verifies the current/previous image records
without touching user data. VMware Workstation itself has not yet been
smoke-tested.

The Linux track has completed the Gate 1 inventory, the **Gate 2 prototype**
(QEMU scope), the **Gate 3 headless GridCraft run** (QEMU prototype scope), and
the **Gate 4 portal slice** (QEMU prototype scope). Boot markers cover signed
embedded-rootfs and loader-staged AppImage import, the pinned host-glibc runtime
manifest, ring-3 `linuxd` launch approval and denial, demand paging,
cross-process shared anonymous mappings, read-shared rootfs mappings,
per-domain quotas, blocking/finite `poll`/`epoll_wait`, cross-process pipes,
sleeps/timerfds, blocking and directory inotify, pseudo-files/devices,
default-deny syscall policy, `clone3`/robust futex/`rseq`,
`siginfo`/`ucontext`/alt-stack/`SA_RESTART`, process groups, blocking `wait4`,
static/dynamic exec, the `PT_INTERP` loader, and the real GridCraft CLI CSV
round trip. `make gate3-test` passes with the pinned 19 MiB AppImage, and
`make ci` (including lint, unit/tooling tests, QEMU boot, ISO/desktop/Files,
fault/rollback/negative paths, release checks, and the runtime-packaging tests)
passes at the commit that adds this run. The exact commit and marker transcript
belong in the PR, not this planning file.

`make gate4-portal-test` covers the portal half of Gate 4: a Linux AppImage
fixture opens a seeded workbook through
`/run/aether/portal/<grant-id>/gate4.xlsx`, reads it, writes a bounded
`.sctmp` staging file, commits it with `rename`, reads the committed bytes
back, and then fails closed on a fresh read, write, and rename after the kernel
revokes the grant. Kernel-owned markers `AETHER_GATE4_PORTAL_OK`,
`AETHER_GATE4_SAVE_OK`, and `AETHER_GATE4_REVOKE_OK` are emitted after the
kernel validates the read operation, the staging/rename transaction, the saved
bytes in its filesystem, and the denials. The GUI feature
`gate4-gui-test` adds a real 4,089-byte XLSX grant at
`/home/gate4-gui.xlsx` (namespace
`/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx`), emits the save marker
immediately after validating the committed ZIP/XLSX bytes, and performs the
post-exit revocation and fresh read/write/rename denials in the kernel. The
GUI, Wayland bridge, software renderer, input routing, and a real windowed
GridCraft run remain open; the compatibility record stays `limited`.

Neither the Gate 2 prototype nor the Gate 3 run is a production compatibility
claim. Linux syscall dispatch, ELF/library mapping, and VMA management remain in
the kernel, so `linuxd` is a ring-3 launch-policy manager rather than the full
ABI service described by the architecture. The imported AppImage tree is not yet
merged into a per-application visible root, and the runtime pin is host-derived.
Mesa/lavapipe/Wayland runtime content, durable storage, a user-installed
AppImage picker, complete inotify/signal/epoll semantics, and the Wayland
portal/renderer stack remain open. These are tracked as the Gate 2 production
follow-up and Gate 4 work; Gate 3's prototype scope does not make the GUI or
arbitrary Linux applications compatible.

The remaining Phase 5/6 work is blocked on user-space process creation, a
durable block store, network/audio services, the Wayland/software-renderer
stack, a user-installed AppImage/package channel, an installer, ring-3
compositor surfaces and application views, and external review. Higher-half
kernel mapping, Files tabs/split panes, durable grant storage across power
loss, SMP, and GPU acceleration remain follow-up work.

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
- [ ] Revocation and complete object-lifetime rules for every object type.
- [ ] Blocking IPC, wakeup queues, timeouts, and broader adversarial IPC tests.

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
- [x] Initial native application SDK (`sdk/`) and user syscall ABI.
- [x] System image and recovery prototype with current/previous selection and
  preserved user data.
- [x] Timestamped UEFI El Torito ISO build (`make iso`).
- [x] UEFI El Torito ISO boot test under OVMF (`make iso-test`) and VMware
  Workstation VMX generation (`make vmware-vmx`). A VMware Workstation smoke
  test remains pending; see [the VMware notes](reference/vmware-workstation.md).
- [ ] Interactive portal consent UI and persistent grant storage.
- [ ] Ring-3 compositor/application views with capability-scoped surface/input
  IPC; current ring-0 adapters are trusted, not compromise-contained.
- [ ] Files copy/move/rename/delete, tabs/split panes, and safe import/export.
- [ ] GPU acceleration, audio, networking, and multi-window application
  rendering.

## Phase 5 — Compatibility and developer alpha

- [x] Signed application package format, verifier, installation, and runtime
  profiles ([package format](specifications/package-format.md)).
- [x] Initial compatibility test suite and database with named-test evidence
  ([compatibility](specifications/compatibility.md)).
- [x] Windows host-interface inventory and feasibility report
  ([report](specifications/windows-host-interface-report.md)). The feasibility
  gate is not met, so no runtime is promised.
- [x] Developer documentation for packaging, updates, hardware, and recovery.
- [ ] Windows compatibility prototype and per-application prefixes.
- [x] Linux AppImage container inspection (Gate 0): validated ELF/SquashFS
  bounds, compression, table offsets, and pinned SHA-256 digest
  ([plan](specifications/linux-compatibility.md)).
- [x] Linux ABI entry slice: static ELF loading, validated Linux `syscall`
  entry, `write`/`exit`, and a boot gate (`AETHER_LINUX_OK`).
- [x] Linux memory and TLS slice: boot-reserved anonymous arena, `mmap`,
  `mprotect`, `brk`, `arch_prctl` FS base, single-task futex, time, and random.
- [x] Linux read-only root and file slice: validated paths, `openat`, `read`,
  `pread64`, `close`, `lseek`, and `access`; no writable root.
- [ ] Linux compatibility layer: complete the steps below.

### Linux application layer

The layer runs unmodified Linux x86-64 applications, including AppImages, in
user space. It is not a VM. The target and security gates are in
[Linux compatibility](specifications/linux-compatibility.md); the design is in
[ADR-015](decisions/ADR-015-linux-app-runtime.md) and
[the Linux layer](architecture/linux-layer.md).

#### Import and runtime root

- [x] Validate AppImage type 2, ELF runtime, SquashFS bounds, and table offsets.
- [x] Pin and verify the artifact SHA-256 before parsing.
- [x] Bounded SquashFS 4 reader with zstd, gzip, lz4, lzo, xz, and lzma
  metadata/data/fragment decompression in `no_std`, inode and directory
  parsing, extraction limits, and a real-AppImage host extractor
  (`aether-squashfs`). Unsupported inode types are rejected.
- [x] Import policy: relative symlinks only, duplicate-path rejection, hardlinks
  marked and copied per path, setuid/setgid/sticky stripped, xattrs counted and
  dropped, extraction limits.
- [ ] Add explicit xattr policy controls beyond count-and-drop.
- [ ] Import into a private read-only root; reject symlink escapes, hardlinks
  outside the root, duplicate paths, and oversized files.
- [x] Per-task writable overlay prototype: bounded entries under `/tmp`,
  created/truncated/read/written through the Linux file descriptors, cleared on
  exit and fault.
- [ ] Extend the overlay to settings/logs paths, persistent per-application
  storage, user-visible erasure, and quotas that survive a crash.
- [x] Signed import records with pinned digest, identity, version, runtime, and
  publisher; Ed25519 verification against a trust store, plus
  `appimage-manifest sign|verify`.
- [ ] Sign and verify the runtime rootfs package itself; record the runtime
  version in the compatibility database.
- [ ] Package the Linux runtime rootfs (glibc, `ld.so`, Mesa/lavapipe, Wayland
  client libraries) as a signed Aether runtime package.

#### Linux ABI

- [ ] Runtime frame allocator and per-task VMA table.
- [ ] File-backed `mmap`, `munmap` reclaim, `mprotect`, `mremap`, and page-fault
  handling; preserve W^X.
- [ ] Dynamic ELF loader: `PT_INTERP`, auxv, `ld.so`, relocations, library
  search, TLS, and `LD_LIBRARY_PATH` restrictions.
- [ ] File syscalls: `stat`/`fstat`/`statx`, `readlink`, `getdents64`, `fcntl`,
  `dup`, `pipe`, `poll`/`epoll`, `eventfd`, `inotify` (bounded).
- [ ] Process and thread syscalls: `clone`, `clone3`, `wait4`, `execve`,
  `set_tid_address`, `set_robust_list`, `rseq`, thread-local storage.
- [ ] Synchronization: real futex wait/wake queues, timeouts, robust lists;
  semaphores and condition variables through glibc.
- [ ] Signals: `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`,
  `rt_sigreturn`, fault-to-signal delivery, and per-thread masks.
- [ ] Time and scheduling: `nanosleep`, `clock_nanosleep`, `timerfd`,
  `sched_getaffinity`, `prlimit64`, and bounded CPU/memory quotas.
- [ ] Pseudo-files: `/proc/self/{exe,maps,status,fd}`, `/dev/{null,zero,random}`,
  and a read-only `/sys` subset only where required.
- [ ] Syscall policy: default-deny allowlist, `ENOSYS` for unsupported calls,
  per-domain network/device denial, and an audit counter per syscall class.

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

#### Application milestones

- [ ] Gate 3: import `gridcraft-0.8.0-linux-x86_64.AppImage` and run its
  `gridcraft-cli` headlessly (`eval`, CSV import/export) with a read-only root
  and private overlay.
- [ ] Gate 4: run `usr/bin/gridcraft` windowed with Wayland, software rendering,
  keyboard/pointer input, and workbook open/save through the portal.
- [ ] Test security denials: no ambient files, network, devices, sibling app
  roots, or native ABI from the Linux domain; crash containment and quotas.
- [ ] Update the compatibility database to `limited`, `compatible`, or
  `verified` only with named tests, exact AppImage digest, runtime version,
  platform, date, and documented limitations.

- [ ] Better storage (persistent block device), networking, audio, and
  accelerated graphics.
- [ ] Interactive permission consent and grant revocation UI.
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

The tree implements the Phase 4 deliverables except interactive portal consent
and launching a packaged application from the desktop, so its exit criterion is
still pending. The implementable Phase 5/6 slices are in place on the QEMU
platform: a clean checkout builds a FAT image and a UEFI ISO, boots through the
loader, enforces a ring-3 user/kernel boundary, runs a native application in a
private storage root, reports denied cross-application access, verifies and
installs a signed package, derives an isolated runtime profile, verifies a
signed update while rejecting replay, downgrade, and an unknown publisher,
starts supervised services, paints a double-buffered desktop with a tested
window manager, accepts keyboard and PS/2 mouse input, grants one file through
the portal, runs a static Linux ELF through the Linux ABI entry and a read-only
root, boots the UEFI ISO under OVMF, and verifies the current/previous
image records without touching user data. VMware Workstation itself has not yet
been smoke-tested.

The remaining Phase 5/6 work is blocked on user-space process creation, a
persistent store, network/audio services, an installer, and external review.
Persistent storage, interactive consent, SMP, blocking IPC, and capability
revocation remain follow-up work.

# ADR-017: Linux application and AppImage runtime

- **Status:** Accepted; Gate 3 headless GridCraft CLI and Gate 4 windowed
  GridCraft run at QEMU prototype scope; the production runtime remains
  incomplete. The Linux ABI currently runs in ring 0, which deviates from
  layer 2 of this decision (see
  [Current deviation](#current-deviation-linux-abi-in-ring-0))
- **Date:** 2026-10-20
- **Supersedes:** none
- **Related:** [Linux compatibility](../specifications/linux-compatibility.md),
  [host-interface inventory](../specifications/linux-host-interface-inventory.md)

## Context

The goal is to run unmodified Linux applications distributed as AppImages, with
GridCraft 0.8.0 as the first target. An AppImage is a packaging format, not a
runtime: it is an ELF launcher with a SquashFS filesystem appended. Running it
requires a Linux userspace and a Linux graphics stack.

Aether is not Linux. It has its own syscall ABI, capability model, filesystem
service, compositor, and scheduler. Native reimplementations of an application
are not acceptable as a substitute: the application must be the upstream
binary, with its own version and behaviour.

The tested target is
`gridcraft-0.8.0-linux-x86_64.AppImage`, 19,024,376 bytes, SHA-256
`942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`.
Its payload is a dynamically linked x86-64 ELF requiring
`ld-linux-x86-64.so.2`, glibc up to `GLIBC_2.35`, `libm`, and `libgcc_s`. The
GUI uses `winit` with Wayland/X11, `wgpu` with Vulkan, D-Bus, and the XDG
desktop portal. glibc, Wayland, and Vulkan are not bundled by the AppImage.

## Decision

Build a user-space **Linux compatibility runtime** with an AppImage importer.
Do not use a virtual machine for the first implementation.

The runtime has four layers:

1. **Import.** Validate the AppImage container, verify its digest and
   publisher, and unpack the SquashFS filesystem into a private read-only root.
   The runner does not execute the embedded AppImage launcher and does not
   require FUSE.
2. **Linux ABI.** Load the Linux ELF, its interpreter, and its shared libraries
   in an isolated address space. Provide Linux process, thread, memory,
   synchronization, signal, time, and file semantics. The kernel gains only a
   narrow, documented syscall entry and the primitives that cannot be
   implemented in user space (address-space mapping, thread scheduling, futex
   wait/wake, fault delivery); policy and Linux-specific emulation live in a
   user-space runtime service, not in the kernel.
3. **Files and devices.** The unpacked application root is read-only. Per-app
   writes go to a private writable overlay. User files are opened only through
   the file portal or a scoped grant. Network, audio, and devices are denied by
   default and require explicit capabilities.
4. **Display and input.** Implement a Wayland compositor service that exposes
   the subset used by `winit`/`wgpu`, with shared-memory buffers presented by
   the Aether compositor. Provide software Vulkan (or software GL as a fallback
   selected through `WGPU_BACKEND`) inside the Linux runtime. No Linux graphics
   driver runs in the kernel.

Each application runs in its own security domain with its own root, overlay,
runtime configuration, and capability profile. A crash terminates that domain,
not the desktop.

The Gate 1 policy decisions (Wayland only, private D-Bus broker, same-domain
spawn, no clipboard in the first profile, fixed paths and environment, software
Vulkan, and network denial) are recorded in the
[host-interface inventory](../specifications/linux-host-interface-inventory.md#decisions)
and are part of this decision.

## Gates

Compatibility is claimed only after the gates in
[`../specifications/linux-compatibility.md`](../specifications/linux-compatibility.md)
are met. Gate 0 (container inspection) is implemented. Gate 1 (host-interface
inventory) is complete in
[`../specifications/linux-host-interface-inventory.md`](../specifications/linux-host-interface-inventory.md);
its missing interfaces are Gate 2 work. Gate 3 now runs the pinned
`gridcraft-cli` from the AppImage headlessly at QEMU prototype scope (formula
evaluation and a CSV round trip through the private overlay). The Gate 4
windowed slice — the in-app Wayland client bridge, the kernel display
endpoint, the GUI portal contract, and the `make gate4-test` harness — is
implemented and passed at QEMU prototype scope on 2026-10-21 at tested commit
`716dcb4` (see the Gate 4 status section below). The production runtime and a
merged per-application AppImage root remain open.

## Gate 4 windowed prototype (status)

The Gate 4 slice keeps this ADR's shape: no Linux graphics driver, no
hypervisor, no Wayland wire server in the kernel, and no host GPU access. What
exists in the tree:

- **Wayland client bridge.** `runtime/wayland/wayland_shim.c` builds as a
  drop-in `libwayland-client.so.0` (plus a `libwayland-cursor.so.0` stub and
  an embedded US keymap). `make gate4-runtime` installs it at the host
  client's SONAME path inside the signed Gate 4 rootfs, so the unmodified
  GridCraft GUI resolves the bridge instead of the host client. It keeps the
  Wayland client object model (registry, compositor, `wl_shm` pools over
  `memfd`, seat keyboard/pointer, xdg-shell toplevel configure, frame
  callbacks) and maps presentation and input onto `/dev/aether-display`.
- **Kernel display endpoint.** Bounded `CREATE`/`PRESENT`/`SET_TITLE`
  /`DESTROY` records and focus-scoped input routing into the ring-0
  compositor (`make gate4-display-test`).
- **GUI portal contract.** The `gate4-gui-test` feature seeds the real XLSX
  workbook behind `/run/aether/portal/<grant-id>/gate4-gui.xlsx`, validates
  the committed bytes, and revokes the grant after exit.
- **Run harness.** `make gate4-test` launches `/usr/bin/gridcraft` from the
  signed AppImage against that rootfs and drives the window with a pointer
  click and the keys `4`, `2`, `ctrl-s`, requiring the kernel-owned present,
  input, save, and revoke markers plus a non-blank screenshot.

The completed windowed run recorded the kernel-owned present, input, save,
portal, revoke, and boot markers, the QEMU screenshots, and the tested commit
(see the [PR #20 Gate 4 review](../reviews/pr-20-gate4.md)). GridCraft stays
`limited`, this is not Wayland or GUI compatibility, and arbitrary Linux
applications remain unsupported. The limits that follow the run are unchanged:
clipboard, interactive resize and window chrome, a D-Bus desktop file chooser,
high-resolution timers, file-backed shared writeback, durable overlay storage,
a user-supplied AppImage picker, a reproducible cross-host runtime pin, nested
epoll, and complete inotify/signal semantics.

## Current deviation: Linux ABI in ring 0

Layer 2 of the decision puts Linux-specific emulation in a user-space runtime
service, and the alternatives below reject the Linux ABI inside the kernel.
The shipped prototype does not meet that yet. Only launch approval runs in the
ring-3 `linuxd` service. The Linux `syscall` trap and dispatcher, ELF and
`PT_INTERP` loading, VMA and page-table management, file descriptors, pipes,
epoll, inotify, timers, futexes, signal frames, `fork`/`execve`/`wait4`, and
the synthetic `/proc` and `/dev` nodes all run in the kernel, mainly in
`kernel/src/syscall.rs`, `kernel/src/scheduler.rs`, `kernel/src/linux.rs`, and
`kernel/src/linux_loader.rs`.

This is recorded as a deviation, not a change of decision. While it stands:

- **Trusted computing base.** Every Linux syscall handler is kernel code. A
  memory-safety or logic bug in one is a kernel compromise, not a contained
  `linuxd` failure.
- **Crash containment.** A fault in user mode retires only the faulting
  domain. A fault the kernel takes while servicing a Linux syscall halts the
  machine, because ring-0 exceptions have no recovery path. The layer avoids
  such faults by validating every user pointer, page by page, before it uses
  it, and by refusing guard pages. A missed check still stops the whole
  system.
- **Controls that apply now.** The syscall entry masks IF, TF, DF, NT, AC, and
  IOPL and clears DF; NMI, #DF, and #MC run on a separate IST stack;
  `rt_sigreturn` keeps only the user-changeable flags and forces the user code
  and data selectors; writable+executable memory requires a signed
  `memory=jit` import record; `PROT_NONE` pages are supervisor-only guard
  pages; futex waiters are keyed by physical address; and signals and
  process groups are scoped to the application domain.
- **Claims.** No compatibility record or release note may describe the Linux
  layer as a user-space service or as outside the kernel's trusted computing
  base while this deviation stands.

The deviation closes in one of two ways:

1. **Move the emulation out.** `linuxd` receives Linux syscalls through a
   remoting path, owns descriptor, signal, and loader state, and drives the
   kernel only through the primitives listed in layer 2. The kernel-side
   handlers are removed as each facility moves.
2. **Accept a kernel-resident ABI.** A new ADR supersedes layer 2 and the
   rejected alternative, with its own threat-model review, a ring-0
   exception-fixup mechanism so that a faulting user copy returns `EFAULT`
   instead of halting, and fuzzing of the syscall surface.

Until one of these lands, new Linux facilities should keep their logic in
self-contained, host-testable code (as `apps/linux/appimage` and
`apps/linux/squashfs` already do) so they can move to `linuxd` later.

## Consequences

- The work is multi-stage and large. Shipping a spreadsheet window that merely
  looks like GridCraft is explicitly out of scope.
- The kernel must grow a second syscall personality and new memory/thread
  primitives. Those changes are security-critical and require their own
  threat-model review.
- The graphics path is the highest-risk item. Software Vulkan through a ported
  Mesa/lavapipe stack is preferred over writing a Vulkan implementation.
- AppImage import needs a SquashFS reader with zstd support. FUSE is not
  required and should not be added just for this.
- The compatibility database carries a GridCraft record no stronger than the
  named tests: `unsupported` before the gates, `limited` after the Gate 3
  headless run, and no GUI upgrade until the Gate 4 windowed run completes. A
  record is evidence, not a roadmap promise.

## Alternatives considered

- **Application VM.** Reuses a Linux guest and its graphics stack, but adds a
  hypervisor, virtio devices, a guest image, and snapshot/recovery work. It
  remains the fallback if the ABI and graphics gates fail. It is not the first
  implementation.
- **Native reimplementation.** Rejected: it does not run the requested
  application and creates a compatibility claim with no upstream evidence.
- **Linux ABI and drivers inside the kernel.** Rejected: it would enlarge the
  trusted computing base and undermine the capability model.
- **FUSE-based AppImage launcher.** Rejected: it requires a FUSE service and
  executes the embedded launcher instead of importing the filesystem through a
  reviewed path.

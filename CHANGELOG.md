# Changelog

All notable changes are recorded here. This project follows a prototype
versioning scheme until the kernel ABI stabilises.

## [Unreleased]

### Added

- Linux Gate 4 windowed-GridCraft slice (QEMU prototype scope; the completed
  run is pending): `runtime/wayland/wayland_shim.c` builds as a drop-in
  `libwayland-client.so.0` (plus a `libwayland-cursor.so.0` stub and an
  embedded US keymap) implementing the client ABI that winit/Smithay and Mesa's
  Vulkan Wayland WSI use, and `make gate4-runtime` installs it at the host
  libraries' SONAME path inside the signed Gate 4 rootfs, so the unmodified
  GUI resolves the bridge instead of the host client. The bridge keeps the
  Wayland client object model — registry, `wl_compositor`, `wl_shm` pools over
  `memfd`, `wl_seat` keyboard/pointer, xdg-shell toplevel configure, and frame
  callbacks — and maps presentation and input to fixed `PRESENT`/input records
  on the existing `/dev/aether-display` endpoint. The new `make gate4-test`
  target stages the pinned GridCraft 0.8.0 AppImage, launches `/usr/bin/gridcraft`
  from the signed import path against that rootfs with the portal workbook
  namespace as argv, and `tools/gate4_smoke.py` waits for the kernel-owned
  `AETHER_GATE4_PRESENT_OK`, `AETHER_GATE4_INPUT_OK`, `AETHER_GATE4_SAVE_OK`,
  and `AETHER_GATE4_REVOKE_OK` markers, captures a QEMU screenshot at
  `build/gate4/gridcraft.ppm` with a non-black pixel check, drives a pointer
  click plus the keys `4`, `2`, `ctrl-s` through the emulated PS/2 path, and
  requires the post-exit revocation after the window close. This is not a
  Wayland wire server, not a GUI compatibility claim, and the completed
  windowed run with its marker/screenshot evidence is still pending.
- Gate 4 kernel display slice: `/dev/aether-display` request/event records, a
  `WindowKind::Linux` compositor window with a kernel-owned presented surface,
  focus-scoped keyboard/pointer routing with window-relative coordinates, and
  `memfd_create`/`ftruncate`/`fstat`/`mmap(MAP_SHARED)` bounded shared
  buffers. `make gate4-display-test` requires the kernel-owned create, present
  (with checksum), and input markers after a static Linux fixture presents a
  deterministic pattern and reads a synthetic event routed through the real
  PS/2/desktop path. This is the display endpoint only; the windowed-GridCraft
  entry above adds the Wayland client bridge and the Mesa/GUI path on top of
  it.
- Per-object capability revocation: `revoke_object` invalidates every alias in
  a table and advances slot generations. Endpoints now have explicit close and
  destroy semantics, task exit/fault releases endpoint aliases and destroys
  owned objects, and stale handles fail with `EBADF` instead of naming a
  recycled endpoint.
- Blocking endpoint receive with tick timeouts (`endpoint_receive_timeout`),
  scheduler yield/wakeup while the queue is empty, and `ETIMEDOUT`. The boot
  gate now requires real block, timeout, close, and stale-handle evidence from
  ring 3.
- Interactive file-portal consent: `portal_request` queues a request that
  grants nothing; the Portal window has approve/deny controls and the console
  has `portal approve`/`portal deny`. Approvals install exact-file records
  through a host-tested codec and rewrite `/system/portal/grants`
  (`AETHER_PORTAL_CONSENT_OK`).
- Scoped filesystem mutations: `rename`, `copy_file`, `remove`, and `rmdir`,
  with existing-destination refusal, directory-into-itself protection, and
  open-file deletion checks. Files exposes duplicate (`F3`), rename (`F2`),
  and delete (`Delete`); the console exposes `cp` and `mv`
  (`AETHER_FILES_MUTATE_OK`).
- Host tests for revocation aliases/generation reuse, grant-record parsing and
  corruption rejection, filesystem copy/rename/delete, browser mutations, and
  the Delete scancode.
- Linux Gate 3 headless GridCraft prototype: `make gate3-test` stages the
  pinned 19,024,376-byte GridCraft 0.8.0 AppImage, verifies its signed import
  record and SHA-256, imports the SquashFS payload, and launches the bundled
  `gridcraft-cli` through `PT_INTERP` with a configured argv. One process
  evaluates `=1+1` (`2.0`) and `=SUM(B2:B3)` (`3`) and writes a 38-byte CSV
  through the private `/tmp` overlay (`wrote /tmp/gate3-out.csv`). The kernel
  emits `AETHER_GATE3_APP_OK` only after a zero exit and derives
  `AETHER_GATE3_DENIAL_OK portal=none native=denied root=readonly
  overlay=private` from the task table. Supporting changes include dynamic
  AppImage launch, an argv-string stack-layout fix, `mprotect` over
  demand-paged ranges, bounded overlay `rename`/`unlink`, short `read`/`write`
  counts, and a 16-entry/4 KiB-per-file overlay. The imported AppImage tree is
  not yet merged into a per-app root, the runtime pin is host-derived, and the
  overlay is RAM-backed; the GUI remains Gate 4.
- Linux Gate 4 runtime rootfs packaging: `make gate4-runtime` builds and signs
  a host-derived Mesa lavapipe/LLVM/Wayland-client rootfs (40 files,
  164,543,720 bytes uncompressed, 45,268,992 bytes compressed) and
  `make gate4-runtime-test` boots it through a new `BootInfo` v5
  runtime-rootfs extent staged by the UEFI loader. The kernel verifies the
  signed `linux-rootfs` record, the manifest pin, and every `file_sha256`
  entry, emits `AETHER_GATE4_RUNTIME_OK`, and runs the real GridCraft AppImage's
  `gridcraft-cli --version` through the rootfs. Missing and tampered runtime
  images fail closed. The allocation-only importer needs a 1.5 GiB heap and a
  3 GiB guest for this tree; the Wayland bridge and GUI launch were still open
  at this slice and are covered by the windowed-GridCraft entry above.
- Linux Gate 2 prototype: blocking/finite `poll`/`epoll_wait` and
  cross-process pipes, `nanosleep`/`clock_nanosleep`/`timerfd`, full-enough
  inotify (blocking mode, directory watches, overflow), `siginfo`/`ucontext`,
  alternate stacks, `SA_RESTART`, `clone3`, robust futex lists, `rseq`,
  pseudo-files and synthetic devices, a default-deny syscall classifier with
  audit counters and quotas, cross-process shared anonymous and read-shared
  rootfs mappings, a digest-pinned host-glibc runtime rootfs, a signed AppImage
  staged through the UEFI loader, and a ring-3 `linuxd` launch-policy manager.
  Linux syscall dispatch and ELF/library mapping remain kernel-side; Mesa/
  Wayland content, durable storage, and user-installed AppImage selection
  remain open. The boot gate now covers the new markers and a larger boot stack
  for the deeper import path.
- Added a narrow Linux inotify prototype: one nonblocking instance per task and
  one exact existing `/tmp` watch for `IN_MODIFY`. `inotify_init1` accepts only
  `IN_NONBLOCK`; default blocking mode and blocking reads are unsupported. The
  boot fixture checks rejection of flags 0, `EAGAIN` with no pending event, and
  the 16-byte event record (`AETHER_LINUX_INOTIFY_OK`). Directory/rootfs watches,
  other masks, event overflow, and shared-thread identity remain unsupported.
- `aether-appimage`: a `no_std`, `unsafe`-free, allocation-free AppImage type 2
  container inspector. It validates the ELF runtime, locates a structurally
  valid SquashFS 4 superblock (rejecting false magic in runtime code), and
  checks compression, block geometry, filesystem size, and table offsets. A
  host `appimage-inspect` binary reports the metadata without executing the
  image. This is Gate 0 of the Linux layer; no Linux binary runs yet.
- The first Linux ABI execution slice: a static freestanding x86-64 ELF runs at
  CPL3 in a private PML4, enters through the architectural `syscall`
  instruction, calls `write` with a validated user pointer, exits cleanly, and
  gates the boot with `AETHER_LINUX_OK`. The native `int 0x80` ABI is denied to
  Linux tasks. Dynamic libraries, threads, futex, signals, Wayland, and Vulkan
  are not implemented.
- Linux memory and TLS primitives for the layer: a boot-reserved anonymous
  arena, `mmap`/`mprotect`/`brk`, FS base through `arch_prctl` with context
  switching, single-task futex, `clock_gettime`, `getrandom`, and `uname`. The
  boot fixture now exercises allocation, TLS, write, and exit in one run.
  Real threads remain missing.
- Linux VMA and mapping slice: `aether-memory::vma` is a host-tested,
  fixed-capacity range table that rejects overlaps and writable+executable
  mappings. Each Linux task tracks its own areas; `mmap` supports anonymous and
  file-backed private mappings, fixed-address reuse, and executable file maps;
  `munmap` removes and clears ranges; `mprotect` updates permissions; `brk`
  grows a reserved heap window. The boot fixture maps `/etc/motd`, reuses a
  fixed address after `munmap`, grows the break, and proves W^X denial
  (`AETHER_LINUX_VMA_OK`). The frame allocator is now reachable at runtime:
  `mmap` and `brk` allocate zeroed frames on demand, and `munmap`, task exit,
  and faults unmap the range, flush the TLB, and return the frames. `mremap`,
  page-fault handling, and shared mappings remain missing.
- Linux read-only root and file syscalls for the compatibility layer:
  validated absolute paths, a fixed embedded file set, `openat`, `read`,
  `pread64`, `close`, `lseek`, and `access`, with a per-task descriptor table
  cleared on exit and fault. The static fixture now reads `/etc/motd` and the
  boot gate requires `AETHER_LINUX_ROOT_OK`. No writable root, directories,
  `stat`, or portal integration yet.
- `aether-squashfs`: a bounded, `unsafe`-free, `no_std` SquashFS 4 reader with
  zstd, gzip, lz4, lzo, xz, and lzma metadata/data/fragment decompression, basic and extended
  file/directory inodes, symlinks, directory validation, extraction limits, and
  a host `squashfs-extract` tool. Import rejects duplicate paths and escaping
  symlinks, marks hardlinks and copies their data per path, strips
  setuid/setgid/sticky bits, and counts/drops xattrs. It extracts 16 files
  (48,350,419 bytes) and two symlinks from the tested GridCraft AppImage.
  in-OS integration and persistent overlay storage remain missing.
- Per-task Linux writable overlay prototype: bounded `/tmp` entries created,
  truncated, read, and written through the Linux file descriptors, with
  `AETHER_LINUX_OVERLAY_OK` boot evidence. The overlay is cleared on exit/fault;
  persistence, settings/logs paths, and user-visible erasure remain follow-up.
- In-OS signed rootfs import: a 4 MiB kernel bootstrap bump heap, Ed25519 record
  verification against the test trust store, SquashFS import at boot, and Linux
  file opens served from the imported tree (`AETHER_LINUX_IMPORT_OK`). The
  image is the embedded boot fixture, not yet a user-supplied AppImage.
- Runtime rootfs packaging and signing: `tools/build-runtime-rootfs.sh` builds
  a zstd SquashFS image and signs it as a `linux-rootfs` import record. The
  populated glibc/Mesa/Wayland rootfs and in-OS consumption remain follow-up.
- Signed AppImage import records: identity, version, runtime, digest, and
  publisher are covered by Ed25519; verification checks the publisher key and
  name against a trust store before comparing the image digest.
  `appimage-manifest sign|verify` is the host tooling.
- Pinned SHA-256 verification for AppImage import (`Digest`,
  `parse_verified`) and an optional digest argument to `appimage-inspect`.
  A mismatch is rejected before the container is parsed. Publisher signatures
  and the trust store remain runtime-manager work.
- `docs/decisions/ADR-017-linux-app-runtime.md`,
  `docs/specifications/linux-compatibility.md`, and
  `docs/architecture/linux-layer.md`: the no-VM Linux compatibility layer,
  host-interface inventory for GridCraft 0.8.0, security controls, and staged
  gates. GridCraft is recorded as `unsupported`.
- Linux `execve` slice: resolve a static `ET_EXEC` image from the verified
  rootfs, build a fresh address space and stack, and enter its entry point.
  The boot fixture execs `/usr/bin/exec` and reports
  `AETHER_LINUX_EXECVE_OK image=ok`. Dynamic exec, argv/envp propagation, and
  close-on-exec remain missing.
- Linux process slice: eager-copy `fork` into a fresh address space, a child
  task table with descriptor inheritance, and `wait4` status/reap (polled with
  `sched_yield`). The boot fixture forks, exits 42 in the child, and reaps the
  encoded status in the parent (`AETHER_LINUX_FORK_OK`). `execve`, blocking
  waits, and process groups remain missing.
- Linux signal slice: `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`, self and
  thread-group `kill`/`tgkill`, user-stack signal frames, `rt_sigreturn`, and
  fault-to-signal delivery. The boot fixture installs SIGUSR1, raises it, runs
  the handler, and returns through the restorer (`AETHER_LINUX_SIGNAL_OK`).
  siginfo/ucontext completeness, alt-stack delivery, and cross-process signals
  remain missing.
- Linux `mremap` slice: grow a mapping in place when possible, otherwise
  move it to a new VMA and preserve its contents; shrinking releases the tail.
  The boot fixture grows a 4 KiB mapping to 8 KiB and checks both pages
  (`AETHER_LINUX_MREMAP_OK`). Page-fault and shared mappings remain missing.
- Linux thread and futex slice: `clone` with `CLONE_VM|CLONE_THREAD`,
  per-thread kernel stacks, TLS, tgid/tid, and clear-child-tid join; real
  `FUTEX_WAIT`/`FUTEX_WAKE` queues that block and wake tasks. The boot fixture
  creates a thread, signals through futex, joins it, and exits
  (`AETHER_LINUX_THREAD_OK`). File/capability tables are copied per thread for
  now, and fork/exec/signals remain missing.
- Linux notification slice: `eventfd2` counters and basic
  `epoll_create1`/`epoll_ctl`/`epoll_wait` over files, directories, and pipes.
  The boot fixture watches an eventfd through epoll and reads it back
  (`AETHER_LINUX_EVENT_OK`). Blocking waits and inotify remain missing.
- Linux directory and pipe slice: `getdents64` over the imported root,
  bounded `pipe2` rings, and non-blocking `poll` for files, directories, and
  pipes. The boot fixture enumerates `/` and `/usr/bin`, writes and reads a
  pipe, and polls it (`AETHER_LINUX_DIR_OK`, `AETHER_LINUX_PIPE_OK`). Blocking
  waits, epoll, eventfd, and inotify remain missing.
- Linux runtime-rootfs and glibc proof: the build stages a signed runtime
  rootfs, imports it at boot, and the loader resolves `PT_INTERP` through it
  instead of embedded interpreter bytes. `make glibc-test` packages the host
  glibc and a dynamically linked hello, then boots it through `ld.so`,
  relocations, and shared `libc` (`AETHER_GLIBC_OK`). This is an opt-in
  host-ABI smoke test; the pinned production runtime rootfs is still missing.
- Linux file-metadata and descriptor slice: `stat`, `fstat`, `newfstatat`,
  `statx`, `readlink` for `/proc/self/{exe,cwd}`, `fcntl`, `dup`, and `dup2`,
  plus signal/affinity/limit stubs needed by a dynamic loader. The boot fixture
  checks stat size, fstat on an open descriptor, `F_GETFD`, duplication, and
  `readlink` (`AETHER_LINUX_FILEMETA_OK`). Directory listing, pipes, and polling
  remain missing.
- Dynamic Linux loader slice: `aether-linux-elf` is a host-tested, allocation-
  free parser for `ET_EXEC`/`ET_DYN` images with bounded `PT_LOAD` segments and
  `PT_INTERP`. `kernel/src/linux_loader.rs` maps a main image and interpreter
  into a fresh address space, builds argc/argv/envp/auxv, and enters the
  interpreter. A test interpreter validates the auxv and jumps to the main
  image's `AT_ENTRY` (`AETHER_LINUX_DYN_INTERP_OK`,
  `AETHER_LINUX_DYN_MAIN_OK`, `AETHER_LINUX_DYN_OK`). glibc, relocations,
  shared-library loading, and a user-supplied interpreter remain missing.
- `docs/specifications/linux-host-interface-inventory.md`: the completed Gate 1
  mapping from the pinned GridCraft GUI and CLI payloads to 32 Aether
  interfaces, with an owner, test ID, implementation status, and an accepted
  decision for each policy question.
- `aether-wm`, a `no_std`, `unsafe`-free, allocation-free window-manager crate:
  geometry, responsive layout, window and workspace state, focus and z-order,
  menus, dock targeting, Spotlight-style search, settings, hit testing, and
  fixed-point animation, with host tests across 640×480 through 1920×1080.
- A clean-room Golden Gate-inspired desktop: top menu bar with working menus,
  floating magnifying dock, rounded windows with traffic-light controls,
  drag/resize/zoom/minimize, four Spaces, a workspace overview, a global
  Clear/Balanced/Tinted Liquid Glass setting, high-contrast mode, reduced
  motion, and an original procedural wallpaper and vector icon set.
- A bounded software compositor with a 1920×1200 back buffer and wallpaper
  cache, damage-tracked repaint/presentation, rounded content clipping, batched
  input, elapsed-tick animation, and a small cursor-only path. GOP copies are
  not atomic/vblank-synchronized and do not guarantee tear-free scanout.
- Working read-only Files folder navigation, selection, keyboard scrolling,
  refresh, and bounded text previews, with denied application-vault access and
  `AETHER_FILES_OK` service-path evidence.
- `aether-input`: host-tested set-1/E0 keyboard decoding, independent modifiers,
  nine-bit mouse decoding, and bounded FIFO event queues.
- A host test harness for the actual kernel graphics module, checking RGB/BGR
  output, padded strides/guards, clipping, wallpaper/cursor restoration, and
  partial presentation; Python tooling regression tests.
- A bounded PS/2 mouse driver on IRQ12 with controller initialization, 9-bit
  packet decoding, pointer clamping, and a deterministic pointer self-test.
- `AETHER_WM_OK` and `AETHER_POINTER_OK` boot markers, plus a live window-state
  view in the `desktop` shell command.
- `make iso-test`, which boots the generated El Torito ISO through UEFI and
  requires the desktop markers, and `make vmware-vmx`, which writes a VMware
  Workstation VM definition for the newest ISO.
- `docs/reference/vmware-workstation.md` and ADR-015.
- ADR-016: responsive desktop, functional Files MVP, correct GOP layout/format
  semantics, and explicit specification/Workstation qualification gaps.
- `aether-memory`: host-testable bitmap frame allocator and x86-64 page-table
  encoding, with tests for reservations, exhaustion, double free, and layout.
- Kernel physical frame allocator over the validated boot memory map, with
  explicit kernel/hand-off reservations and deferred release of boot-services
  memory until after the CR3 switch.
- Kernel-owned four-level page tables: 4 KiB W^X mappings for kernel text,
  rodata, and data, plus a 2 MiB identity map for the rest of RAM.
- Flat kernel GDT and an IDT covering all 32 architectural exception vectors,
  with vector/error/RIP/RFLAGS/CR2 diagnostics and a failure marker.
- `make fault-test`: a boot-time `int3` self-test proving exceptions reach the
  kernel handler after paging is active.
- Capability, IPC, vision, security-boundary, release-policy, system-ABI, and
  development documentation.
- `docs/architecture/memory.md`.
- A 100 Hz PIT/PIC timer and preemptive round-robin scheduler with static task
  control blocks and per-task kernel stacks.
- Per-task PML4 address spaces, a private physical frame mapped at the same
  virtual address, and CR3 switching on context switches.
- A boot-time address-space isolation test across three kernel tasks.
- `docs/architecture/scheduler.md`.
- `aether-ipc`: host-tested generation-checked capability tables and bounded
  endpoint queues.
- Per-task capability tables, endpoint rights, kernel-mediated capability
  transfer, and a boot-time capability/IPC test with insufficient-rights and
  forged-handle checks.
- `AETHER_CAP_OK` and `AETHER_IPC_OK` boot markers.
- Explicit `IA32_EFER.NXE` and `CR0.WP` setup, with a boot failure when NX is
  unavailable.
- Hand-off validation for reserved fields, memory-map alignment, region
  ordering, and overlapping regions.
- Endpoint identity checks and atomic capability receive on table exhaustion.
- Expanded local CI gate with `make docs-check`, `make license-check`, and
  `make negative-test`; a GitHub Actions workflow publication remains a
  follow-up.
- ADR-005 through ADR-010 and a Windows compatibility feasibility plan.
- Ring-3 native application support: user GDT/TSS descriptors, TSS `RSP0`
  kernel stacks, user PML4s, an `int 0x80` syscall gate, `InterruptFrame`
  user-context fields, and fault containment that retires only the faulting
  task.
- User syscall ABI and `aether-sdk`: yield, exit, console write, pid, uptime,
  file open/read/write/close/list/stat, portal open, and diagnostics.
- `aether-fs`: a 128 KiB RAM block device and bounded filesystem with 32
  inodes, contiguous file blocks, directories, generation-checked open handles,
  root-scoped path resolution, quotas, and host tests.
- Kernel file service with per-process roots, file capabilities, and denied
  cross-application and root-escape checks (`AETHER_APP_OK`, `AETHER_FS_OK`,
  `AETHER_SANDBOX_OK`).
- `aether-services`: host-tested service supervisor with dependency order,
  restart budgets, and quarantine; application lifecycle table.
- Console shell with `help`, `ls`, `cat`, `write`, `rm`, `apps`, `services`,
  `security`, `input`, `uptime`, `recovery`, `desktop`, and `clear`.
- `make shell-test` socket-driven console acceptance test.
- BootInfo version 2 with a validated `FramebufferInfo` descriptor and loader
  GOP query.
- BootInfo version **3**, correcting GOP format semantics to `0 = RGBX`,
  `1 = BGRX`. Loader/kernel must be rebuilt together. The GOP binding now has
  one pixel mask and the correct stride offset; short information/apertures are
  rejected rather than guessed, and dimensions/aperture size are bounded.
- Framebuffer mapping (cache-disabled, non-executable) and a linear graphics
  service with a 5x7 font and pixel checksum.
- Software compositor with Files, Security Center, File Portal, Settings,
  Recovery, and System Log windows; focus and keyboard movement.
- PS/2 IRQ1 keyboard service with a bounded queue and a deterministic boot
  self-test (`AETHER_INPUT_OK`).
- Exact-path file portal grants and a portal-granted read from the native
  application (`AETHER_PORTAL_OK`).
- Portal grant revocation (`AETHER_REVOCATION_OK`), the `portal` and
  `portal revoke <index>` console commands, and a Security Center portal view
  derived from the live grant table.
- Recovery prototype with current/previous image markers, selection, and
  preserved user data (`AETHER_RECOVERY_OK`).
- `make iso`: timestamped UEFI El Torito ISO with an embedded FAT EFI system
  image under `build/ISOs/`.
- Architecture notes for the filesystem and desktop.
- Boot markers for graphics, desktop, input, portal, recovery, supervisor, and
  shell.
- `aether-crypto`: dependency-free SHA-256, SHA-512, and Ed25519 verification
  and signing, tested against RFC 8032 vectors and an independent
  implementation.
- `aether-packaging`: signed `.aether` container, canonical manifest with
  reverse-domain identity, architecture/runtime policy, permission allowlist,
  resource limits, publisher trust store, bounded payload archive, and a single
  verifier shared by the kernel and tooling.
- Kernel package installation into `/apps/<id>` with boot-time rejection of an
  unknown publisher, a modified payload, and a zeroed signature
  (`AETHER_PACKAGE_OK`), plus the `packages` shell command.
- Per-application runtime profiles with derived prefixes, declared permissions
  and limits, path containment, and consent-based prefix sharing
  (`AETHER_RUNTIME_OK`), plus the `runtime` shell command.
- `aether-compat`: compatibility database format, classification rules, named
  test evidence, and the shipped records under `tests/compatibility/`.
- `aether-update`: signed update manifests, publisher pins, image digests,
  monotonic rollback counters, version and channel policy, and an explicit
  recovery override.
- Kernel update verification self-test accepting a newer image and rejecting
  replay, downgrade, and an untrusted publisher (`AETHER_UPDATE_OK`), plus the
  `updates` shell command.
- `aether-admin` host tool for package, update, and release signing; release
  bundles with `SHA256SUMS`, `release-info.txt`, and a signed manifest.
- `make release`, `make release-check`, `make release-demo`, `make compat-test`,
  `make update-test`, and `make qualify`.
- `make qualify` writes `build/qualification/report.md` with commit, toolchain,
  emulator, step results, boot markers, and artifact digests.
- Documentation: package format, signed updates, compatibility database,
  Windows host-interface report, supported hardware, installer and recovery,
  security response process, community program, and ADR-013/ADR-014.
- Ring-3 endpoint IPC syscalls: `endpoint_create` (15), `endpoint_send` (16),
  and `endpoint_receive` (17), with a four-slot kernel endpoint pool, bounded
  queues of length four, generation-checked handles (`(generation<<8)|index`),
  and per-direction right bits so send and receive authorization are separate
  capabilities.
- `aether-sdk` endpoint wrappers (`endpoint_create`, `endpoint_send`,
  `endpoint_receive`) and the `EAGAIN` errno for a full or empty queue.
- A ring-3 IPC and sandbox demonstration program that exercises a hello
  round trip, bounded-queue fill and drain (`EAGAIN` both directions), a
  length-violating send (`EINVAL`), forged, wrong-type, and
  file-handle-as-endpoint sends (`EBADF`), a capability transfer that the
  receiver sees without transfer rights, open-file quota denial at the fifth
  concurrent open, root-escape refusal, and granted and denied portal paths.
- A scheduler boot gate that requires the demonstration program's IPC
  counters (seven sends, seven receives, seven denials) and at least one
  open-file quota denial, printing `AETHER_IPC_USER_OK` and
  `AETHER_BOOT_FAILED reason=user-ipc …` otherwise.
- A feature-gated recovery rollback self-test (`recovery-rollback-selftest`,
  exercised by `make rollback-test`) that discards the current image marker,
  requires the previous marker and preserved user data, and prints
  `AETHER_RECOVERY_ROLLBACK_OK`.
- Live Security Center and System Log desktop windows fed from the kernel's
  quota, portal, IPC, exit, and fault counters, with a closing warning roll-up
  of denied events.
- `tools/release-tamper-test.sh`: a poisoned `SHA256SUMS` entry and a flipped
  image byte must both fail `release verify`.
- `fs::seed_application_roots`, which provisions the installed application's
  private directory after package installation; the `memory` shell command;
  and quota, portal, IPC-denial, exit, and fault counters in the `security`
  shell command.

### Changed

- Portal grant records are fixed-size validated records rather than
  `&'static str` boot seeds. Corrupt or duplicate stores fail closed instead of
  loading partially.
- The system ABI documents calls 18–20 and `ETIMEDOUT`; the SDK exposes
  `endpoint_close`, `endpoint_receive_timeout`, `syscall5`, and
  `portal_request`.
- The desktop paints through a bounded back buffer where possible and presents
  damaged regions. The graphics service honors both UEFI RGBX/BGRX layouts;
  assuming BGRX for both codes hid an incorrect ABI enum instead of fixing it.
- The boot test requires `AETHER_WM_OK` and `AETHER_POINTER_OK` in addition to
  the existing graphics, desktop, and input markers.
- The `desktop` shell command reports the active Space, window count, focus,
  mode, pointer position, checksum, and display size, followed by per-window
  state.
- `make ci` and `make qualify` boot the distribution ISO through UEFI El Torito
  in addition to the FAT-image boot test.
- The loader keeps `BootInfo` and memory-map metadata below 4 GiB.
- The identity map is built from mappable regions only; reserved holes and
  device MMIO stay unmapped, and a machine with more than 16 GiB still boots
  using the low 16 GiB.
- Boot tests require the full Phase 1–4 marker set: memory, scheduler,
  isolation, capabilities, IPC, native application, filesystem, sandbox,
  supervisor, recovery, graphics, desktop, input, portal, shell, and boot.
- `make ci` runs documentation and licence checks, the shell test, the
  exception-path test, and the negative boot test in addition to the normal
  boot test.
- The loader queries GOP and writes BootInfo version 3 (136 bytes).
- QEMU test harnesses start a headless VNC display and emulated VGA adapter so
  OVMF exposes a linear framebuffer.
- `make ci` also runs the compatibility suite and the package/update policy
  crates, and `make test` requires `AETHER_PACKAGE_OK`, `AETHER_RUNTIME_OK`, and
  `AETHER_UPDATE_OK`.
- The shell exposes `packages`, `runtime`, and `updates`; the shell test checks
  them.
- Package and update verification use one `no_std` implementation in both the
  kernel and host tooling.
- `make lint` also checks host test targets (`--all-targets`).
- The `list` syscall copies the path before creating the mutable output slice,
  so overlapping user ranges cannot create aliasing references.
- File-service read/write/list/stat errors preserve their specific errno instead
  of collapsing to `EBADF`.
- The `yield` and `sleep` syscalls now enter the scheduler instead of returning
  immediately.
- Update verification rejects a non-empty unsigned record payload, and the
  selected-update record is derived from the verified manifest.
- Update image names have their own bounded validator, so version strings such
  as `aether-0.10.0.iso` are accepted without widening package payload names.
- The update boot self-test also replays the activated manifest after the
  counter is advanced, and reports
  `AETHER_UPDATE_OK accepted=1 rejected=4 replay_protected=true`.
- The demonstration package fixture now grants `portal.read` and
  `portal.write` so the ring-3 program exercises both granted and denied
  portal paths.
- The `demo` counters used by the boot gate are printed once, by the kernel,
  after the user program exits.

### Fixed

- ISO pointer assertions wait for the guest's workspace slide to finish instead
  of treating logical workspace selection as completed on-screen movement.
- The release-tamper harness reads the first checksum record only, ensuring a
  zero-prefixed image digest is actually changed. A regression exercises all
  16 hexadecimal prefixes with a multiline `SHA256SUMS`; verification policy is
  unchanged and both real bundle mutations must still be rejected.
- Full-screen redraw on every pointer/key event no longer stalls the normal
  interaction path; rendering caches, clipping, damage and event batching have
  regression coverage and live timings.
- Dedicated E0 arrows, right Alt/Super and independent modifier releases are
  decoded correctly. Mouse output is source/status checked; queued button
  edges and overflow reconciliation avoid lost clicks and stuck drags.
- Minimize immediately transfers focus, reverse cycling reaches every eligible
  window, restore preserves zoom, closing windows remain painted during their
  transition, and wrapped window IDs cannot alias live windows.
- Search-field/menu-padding clicks no longer execute unrelated actions; Tinted
  menu alpha cannot overflow. F10/Left/Right reach all menus and Shift+arrows
  resize without a pointer.
- `make iso-test` requires all boot/security markers and rejects any subsystem
  failure, including Files/input failures followed by a global success marker.
- The Workstation VMX generator safely handles quoted/space-containing paths,
  accepts an explicit ISO, keeps the tested artifact instead of rebuilding it,
  and captures serial diagnostics. Qualification now includes the rollback gate.
- The idle loop now drains IRQ1 scancodes into the desktop after boot. Physical
  PS/2 keyboard input previously filled the queue without moving focus or
  windows because only the synthetic boot self-test consumed keys. The `input`
  shell command now reports received and dropped counts.
- The loader sorts the firmware memory map before merging regions. VMware's
  UEFI returns an unsorted map, which previously failed the hand-off contract
  with `memory regions are not sorted` even though the map itself was valid.
- Filesystem inode metadata is validated before use. An oversized size, an
  out-of-device block number, or a malformed directory size is rejected with
  `Corrupt` instead of panicking the kernel. Unformatted devices and unknown
  superblock versions are rejected before any inode is trusted.
- Replacing file contents now allocates and writes the new blocks before
  committing the inode and releasing the old blocks. A failed write preserves
  the previous file instead of leaving it pointing at freed storage.
- Directory and file creation release their inode/block allocations when a
  later step fails.
- A file with live open handles cannot be unlinked, so a handle cannot outlive
  its inode or read blocks recycled by another file.
- A restarted service must still have all dependencies running, matching the
  first-start rule. Failed services must use the bounded restart path, and
  quarantined services cannot be started again.
- File handles are closed when a task exits or faults. Bounded open-file slots
  are no longer leaked by a crashing task.
- Filesystem open-file ids are converted from capability object ids with a
  checked conversion (`EBADF` on overflow) instead of silent `as u8`
  truncation.
- Capability lookup reports a wrong object type as `WrongObjectType` before
  any rights check, so using a file handle as an endpoint fails with `EBADF`
  rather than a misleading permission error.
- Portal permission enforcement checks every requested right: a manifest that
  declares `portal.write` but not `portal.read` can no longer obtain a
  readable capability through a combined `READ|WRITE` request. Permission is
  also checked before the exact-path grant, so an undeclared application gets
  a consistent `EACCES` instead of learning whether a grant exists.
- File-handle decoding rejects reserved zero and values above the 24-bit
  encoding before lookup, matching the endpoint decoder and the system ABI
  contract instead of silently truncating high bits.
- The release tamper test removes both temporary bundle copies (the second
  assignment previously replaced the only `EXIT` trap).
- The ring-3 diagnostics no longer duplicate the expected errno in the
  too-long IPC payload line; the serial transcript, Phase 4 status, escape
  count, boot-gate wording, and memory-view documentation now match the tested
  output.

### Known limitations

- Grant records live on the RAM disk, so they survive service restarts but not
  a power cycle until a durable block device exists.
- Blocking IPC uses timer-driven scheduler wakeups rather than a per-endpoint
  wait list; cancellation and SMP wake IPIs remain follow-up work.
- Files tabs/split panes, safe import/export, ring-3 compositor surfaces,
  higher-half mapping, durable storage, networking, audio, and accelerated
  graphics remain unimplemented.
- Services and the desktop still execute as kernel modules; only the native
  application runs at CPL3. Moving service bodies into supervised user
  processes is follow-up work.
- There is no USB HID, touchpad, multi-touch, screen reader, or IME support;
  keyboard and standard PS/2 mouse input are supported.
- The serial console shell is a ring-0 diagnostic surface with full filesystem
  authority. It is not the ordinary-user terminal or an elevation boundary.
- Storage is a volatile RAM disk. There is no persistent block device,
  networking, audio, GPU acceleration, or general graphical package manager.
- The filesystem is a bring-up format: 32 inodes, 4 KiB per file, one
  directory block per directory, and no crash-consistency protocol.
- The scheduler has no priorities or dynamic task creation; blocking IPC uses
  timer-driven wakeups rather than per-endpoint wait lists.
- Identity map only; no higher-half kernel, guard pages, or kernel heap.
- No IST for double-fault and stack-overflow recovery.
- The kernel image is embedded in the loader, so updating the kernel requires
  rebuilding the loader and boot image.
- The frame bitmap covers at most 16 GiB.
- Single-core only; no SMP startup.
- NX and WP are enabled, but there is no fault-injection test that attempts to
  write kernel text or execute data.
- No Secure Boot signing, measured boot, or TPM-backed rollback counter yet.
  Installed update state lives in the writable RAM image.
- Packaged applications are verified and installed but their entry points are
  not executed; the kernel still embeds one native program.
- Packages do not persist across reboot, and update activation/staging and a
  general UEFI installer are not implemented.
- Release signing uses a key file outside the repository; the checked-in keys
  are test keys and provide no security. The ISO embeds build timestamps, so
  the signed artifact digest is recorded per release rather than claimed
  bit-for-bit reproducible.
- No Windows or other compatibility runtime exists; the compatibility database
  records Windows applications as unsupported and the package verifier rejects
  `runtime=windows`.
- No external security review has been completed.
- The ISO is boot/interaction-tested under OVMF on VGA/SVGA emulation; VMware
  Workstation itself still needs a recorded smoke test before qualification.

## [0.1.0] - 2026-10-19

### Added

- Initial repository structure and contribution/security/governance documents.
- Pinned Rust 1.99.1 toolchain with `x86_64-unknown-none` and
  `x86_64-unknown-uefi` targets.
- `no_std` UEFI loader with a defensive ELF64 parser, page loading, memory-map
  conversion, and boot-services exit.
- `no_std` x86-64 kernel skeleton with a dedicated stack, COM1 serial output,
  hand-off validation, and panic reporting.
- Versioned `BootInfo` ABI shared by the loader, kernel, and host tests.
- QEMU/OVMF boot harness (`make test`) and local CI gate (`make ci`).
- Architecture, threat-model, and ADR documentation.

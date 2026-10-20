# Changelog

All notable changes are recorded here. This project follows a prototype
versioning scheme until the kernel ABI stabilises.

## [Unreleased]

### Added

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

### Changed

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
- The loader now queries GOP and writes BootInfo version 2 (136 bytes).
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

### Fixed

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

### Known limitations

- Services and the desktop still execute as kernel modules; only the native
  application runs at CPL3. Moving service bodies into supervised user
  processes is follow-up work.
- The serial console shell is a ring-0 diagnostic surface with full filesystem
  authority. It is not the ordinary-user terminal or an elevation boundary.
- Storage is a volatile RAM disk. There is no persistent block device,
  networking, audio, GPU acceleration, or package manager.
- The filesystem is a bring-up format: 32 inodes, 4 KiB per file, one
  directory block per directory, and no crash-consistency protocol.
- Portal grants are seeded at boot and can be revoked from the console; there
  is no interactive consent UI, persistent grant store, or graphical revocation
  control, and revocation does not invalidate an already-open capability.
- The scheduler has no priorities, sleeping/wakeup queues, or dynamic task
  creation; IPC is polled rather than blocking.
- Capability revocation, endpoint destruction, and complete object-lifetime
  behavior are not implemented.
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
- The ISO is generated but automated CD/ISO boot is not part of `make ci`.

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

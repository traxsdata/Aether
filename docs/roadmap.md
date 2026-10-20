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

- [x] Graphics and input services over a linear GOP framebuffer and PS/2 IRQ1.
- [x] Basic software compositor with bounded windows and focus handling.
- [x] Desktop shell, file manager, settings, and Security Center views.
- [x] File portal with per-process, per-file grant records; revocation blocks
  future portal opens (`AETHER_REVOCATION_OK`).
- [x] Initial native application SDK (`sdk/`) and user syscall ABI.
- [x] System image and recovery prototype with current/previous selection and
  preserved user data.
- [x] Timestamped UEFI El Torito ISO build (`make iso`).
- [ ] Interactive portal consent UI and persistent grant storage.
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

The tree reaches the Phase 4 exit criterion and the implementable Phase 5/6
slices on the QEMU platform: a clean checkout builds a FAT image and a UEFI ISO,
boots through the loader, enforces a ring-3 user/kernel boundary, runs a native
application in a private storage root, reports denied cross-application access,
verifies and installs a signed package, derives an isolated runtime profile,
verifies a signed update while rejecting replay, downgrade, and an unknown
publisher, starts supervised services, paints a composited desktop, accepts
keyboard input, grants one file through the portal, and verifies the
current/previous image records without touching user data.

The remaining Phase 5/6 work is blocked on user-space process creation, a
persistent store, network/audio services, an installer, and external review.
Persistent storage, interactive consent, SMP, blocking IPC, and capability
revocation remain follow-up work.

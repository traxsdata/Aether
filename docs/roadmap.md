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
- [ ] User/kernel separation and user-space loader.
- [ ] Expose capability and IPC operations through validated syscalls.
- [ ] Revocation and object-lifetime rules.
- [ ] Blocking IPC, wakeup queues, timeouts, and broader adversarial IPC tests.

## Phase 3 — Minimal operating system

- [ ] Supervisor and service startup/failure rules.
- [ ] Virtual storage driver and filesystem service.
- [ ] File handles and access checks.
- [ ] Console shell and application lifecycle manager.
- [ ] Private application storage and sandbox tests.

## Phase 4 — Native desktop prototype

- [ ] Graphics/input service and basic compositor.
- [ ] Shell, file manager, settings, security center.
- [ ] File portal and native SDK.
- [ ] Image/recovery prototype.

## Phase 5 and later

Windows compatibility research, packaging, update verification, recovery,
external review, and release qualification. Full integration waits until the
Aether interfaces it depends on exist.

## Current milestone

Phase 1 is complete and Phase 2 is underway: a clean checkout builds an image,
boots under QEMU, owns its page tables with NX and write-protect enabled,
reports exceptions, preemptively runs tasks in separate address spaces, and
enforces capability rights, endpoint identity, and bounded IPC with a boot-time
transfer test. The next milestone is user-mode entry: TSS/RSP0, user page
permissions, a syscall gate for capability/IPC operations, and per-task kernel
stacks.

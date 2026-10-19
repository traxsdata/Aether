# Roadmap

This roadmap follows the phased plan and backlog execution rule in the
[technical specification](../Technical%20Specification%20and%20Initial%20Development%20Backlog.txt).
It is a planning model, not a delivery promise.

## Phase 0 — Specification and proof of feasibility

- [x] Architecture decision records for kernel, microkernel, platform, and
  language.
- [x] Threat model and security boundaries for the implemented boot path.
- [x] Pinned toolchain and QEMU target.
- [x] Boot ABI and defensive image parser.
- [ ] Broader system ABI, capability, and IPC specifications.
- [ ] Windows compatibility feasibility report (can run in parallel).

## Phase 1 — Bootable kernel

- [x] UEFI boot path.
- [x] Serial console and panic reporting.
- [x] Memory-map parsing and kernel hand-off.
- [x] Automated boot test and CI.
- [ ] IDT and structured exception reporting.
- [ ] Physical page allocator and kernel-owned page tables.
- [ ] Architecture initialization tests.

## Phase 2 — Isolated execution

- [ ] Task/thread objects and address-space ownership.
- [ ] Preemptive timer-driven scheduler.
- [ ] User/kernel separation and user-space loader.
- [ ] Capability object model, tables, and rights checks.
- [ ] IPC endpoints, bounded messages, capability transfer.
- [ ] Revocation and object-lifetime rules.
- [ ] Adversarial capability and IPC tests.

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

The first-sprint acceptance criteria are met when a clean checkout builds an
image, boots under the documented QEMU configuration, prints the startup
message on serial, and CI detects a deliberately broken build or boot. The next
milestone is a kernel that owns its page tables and reports faults without
resetting.

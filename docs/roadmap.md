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
- [x] Broader system ABI, capability, and IPC specifications.
- [ ] Windows compatibility feasibility report (can run in parallel).

## Phase 1 — Bootable kernel

- [x] UEFI boot path.
- [x] Serial console and panic reporting.
- [x] Memory-map parsing and kernel hand-off.
- [x] Automated boot test and CI.
- [x] GDT and IDT with structured exception reporting.
- [x] Physical page allocator and kernel-owned page tables.
- [x] Architecture/memory initialization tests (host unit tests and QEMU
  exception-path test).
- [ ] Higher-half kernel and per-address-space page tables.
- [ ] Interrupt controller and timer.

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

The Phase 1 boot and memory milestones are met: a clean checkout builds an
image, boots under the documented QEMU configuration, owns its page tables,
reports exceptions, and runs automated boot and fault tests. The next milestone
is a higher-half mapping, timer interrupts, and the task/address-space model.

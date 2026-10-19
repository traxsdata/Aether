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

- [x] Static task/thread objects and preemptive timer-driven scheduling.
- [x] Per-task address spaces with CR3 switching and a hardware isolation test.
- [x] Timer interrupt and legacy PIC/PIT setup.
- [x] Capability table, rights checks, and generation-checked handles.
- [x] Bounded IPC endpoints and kernel-mediated capability transfer.
- [x] Host tests for capability and IPC validation plus boot-time adversarial
  rights/forged-handle checks.
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
boots under QEMU, owns its page tables, reports exceptions, preemptively runs
tasks in separate address spaces, and enforces capability rights on bounded IPC
with a boot-time transfer test. The next milestone is user-mode entry: TSS/RSP0,
user page permissions, a syscall gate for capability/IPC operations, and
per-task kernel stacks.

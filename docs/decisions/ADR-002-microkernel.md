# ADR-002: Capability-based microkernel

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Putting filesystems, networking, device drivers, the application manager,
permission policy, and the desktop in the kernel would enlarge the trusted
computing base and make isolation harder to reason about. Aether needs
mechanisms that can enforce boundaries without embedding every policy in
privileged code.

## Decision

Use a microkernel architecture with capability-based resource access. The kernel
implements CPU scheduling, address spaces, IPC primitives, capability
management, interrupts/exceptions, timers, controlled hardware mapping, and
task lifecycle. Filesystems, networking, most drivers, the application manager,
the permission broker, and the desktop run in user space.

A capability is an unforgeable reference with explicit rights. Possessing no
capability means no authority.

## Consequences

- The kernel is small enough to audit, but the system needs carefully designed
  IPC, service supervision, and capability distribution.
- Service startup, failure, and restart semantics become first-class concerns.
- IPC performance and denial-of-service resistance matter from the beginning.
- Filesystem and driver bugs should be contained if their service boundaries
  are correct; the security of the whole system depends on those boundaries and
  on the kernel's validation.
- The initial system will be less capable than a monolithic OS at the same
  engineering effort.

## Alternatives considered

- **Monolithic kernel:** simpler early on, but violates the security and
  auditability goals.
- **Exokernel/library OS:** interesting for flexibility, but pushes too much
  policy into every application and complicates the common desktop model.
- **Formally verified microkernel as a dependency:** not chosen as a
  production dependency; reference material only.

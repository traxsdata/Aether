# ADR-001: Independent kernel

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Aether's premise is control over its process model, memory management,
scheduling, capabilities, and security enforcement. Using Linux, BSD, or an
existing microkernel as the production kernel would make those properties
depend on another project's architecture and release process.

## Decision

Implement an original Aether kernel. Reference material and development tools
may be consulted where their licences permit, but reused code must be
identified and must not be represented as original Aether work.

## Consequences

- The project owns the kernel ABI and can design capabilities and isolation
  from first principles.
- Driver and hardware support become a large, long-term risk; the initial target
  is QEMU with a tightly constrained virtual platform.
- Kernel maturity will lag a general-purpose OS. The first release is a kernel
  and security research prototype, not a daily-use desktop.
- Every privileged subsystem must be designed, reviewed, and tested here rather
  than inherited.

## Alternatives considered

- **Linux-based distribution:** rejected because it does not provide the
  project's intended kernel independence or capability model.
- **seL4 or another microkernel as production kernel:** rejected for production
  use; the concepts may inform the design, but the kernel remains Aether's own.

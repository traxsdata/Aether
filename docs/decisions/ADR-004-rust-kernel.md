# ADR-004: Rust for kernel and boot code

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Kernel code must be memory-safe where possible, reviewable, and testable on the
host. The team also needs low-level control over page tables, CPU state, and
device access.

## Decision

Use Rust with `no_std` for the kernel and loader, with minimal architecture
assembly. Pin the toolchain and targets in `rust-toolchain.toml`. Permit
`unsafe` only where hardware interaction or a validated invariant requires it;
every `unsafe` block must document its safety argument. Do not assume Rust
makes the kernel secure: integer handling, concurrency, capability validation,
and hardware programming remain security-critical.

Host-testable logic (ELF parsing, ABI validation, arithmetic) lives in separate
crates that can be exercised without emulation.

## Consequences

- Memory-safety bugs are reduced but not eliminated; `unsafe` review is still
  mandatory.
- The project depends on a Rust toolchain and bare-metal targets; CI and
  contributor documentation must pin them.
- Host tests are fast and can cover parsers and policy; QEMU tests cover the
  parts that cannot run on the host.
- Assembly is kept to entry, CPU control, and port I/O where practical.

## Alternatives considered

- **C:** broad tooling and familiarity, but weaker default memory safety and a
  larger review burden for parser-heavy boot code.
- **C++:** similar concerns with a much larger language and runtime surface.
- **Rust with `std`:** unavailable and inappropriate for a kernel; `no_std`
  keeps dependencies and runtime assumptions explicit.

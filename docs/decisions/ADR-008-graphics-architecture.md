# ADR-008: Graphics architecture

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

A desktop operating system needs display and input, but a complete GPU driver
ecosystem is a multi-year effort. The kernel must stay small, and graphics
policy must not become part of the privileged core.

## Decision

Begin with a simple framebuffer console and a minimal user-space graphics
service.

- The first milestone uses a framebuffer or virtual display device and basic
  input.
- A user-space compositor owns window composition and display policy.
- Applications draw through a documented graphics protocol; they do not receive
  raw framebuffer or GPU access by default.
- Hardware acceleration, multiple GPUs, and vendor driver ecosystems are later
  milestones. The initial design must not promise them.
- The kernel may expose a controlled mapping for a display device to the
  authorized graphics service; it does not contain a GUI toolkit or window
  manager.

## Consequences

- Early graphics will be unaccelerated and limited to the supported virtual
  hardware.
- The compositor is a trusted service for display integrity but not a security
  authority for files or capabilities.
- The graphics protocol must define buffer ownership, input focus, and
  cross-application isolation.
- A native SDK and toolkit can be built on the service protocol without
  exposing device details.

## Alternatives considered

- **In-kernel framebuffer console forever:** rejected as a final architecture;
  useful for bring-up, but not a desktop.
- **Port an existing GPU driver stack into the kernel:** rejected for the first
  prototype; it enlarges the trusted core and creates dependencies Aether cannot
  yet support.
- **Require a full GPU driver ecosystem before a desktop:** rejected; it would
  block the usable-system milestone on the hardest driver work.

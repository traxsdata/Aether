# Aether Vision

Aether is a general-purpose desktop operating system that combines broad
application compatibility with strict, inspectable security boundaries.

Unlike a conventional Linux distribution, Aether owns its kernel, system
interfaces, process model, security enforcement, and desktop experience. The
goal is not to rebuild every application from scratch, but to provide a
platform on which native software, Windows-compatible runtimes, and virtual
machines can operate without implicit access to the user's data.

## The defining property

Applications do not automatically receive access to the filesystem, other
applications' data, devices, or privileged services. Every application runs in
a distinct security domain with private storage and explicit capabilities.
Access to user files is mediated by a trusted portal controlled by the user.

A desktop dialog is not enforcement. Enforcement lives in the kernel and in
resource-owning services; the desktop may request an action, but cannot grant
authority by itself.

## Principles

1. Least authority by default.
2. Private application data is not readable by unrelated applications.
3. User files are accessed through explicit, revocable grants.
4. No silent privilege elevation.
5. The kernel stays small and auditable.
6. Network, device, clipboard, and IPC access are capabilities.
7. Security claims are backed by tests and documented limitations.
8. The system remains usable without a mandatory online account.
9. Updates are authenticated, recoverable, and preserve user data.

## Non-goals for 0.1

Aether 0.1 is a kernel and security research prototype. It will not provide:

- Complete Windows compatibility.
- Support for all PCs, GPUs, or peripherals.
- A production anti-cheat solution.
- A complete POSIX implementation.
- A from-scratch browser, office suite, or media suite.
- Formal verification of the whole system.
- A from-scratch general-purpose hypervisor.
- Protection against every physical, firmware, or side-channel attack.

## How progress is measured

Every feature is assessed on two axes:

- **Capability:** what the system can do.
- **Assurance:** what evidence shows it cannot do unauthorized things.

A feature that increases capability while weakening the security model must be
redesigned or rejected.

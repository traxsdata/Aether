# ADR-005: Aether-native system interfaces

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Aether already defines a versioned loader-to-kernel boot ABI. It does not yet
define the interface applications will use. Kernel-internal structures must not
become the application ABI: that would expose implementation details, make
compatibility promises impossible to keep, and give native applications an
unnecessary path to privileged kernel behaviour.

## Decision

Define Aether's own system ABI and native application SDK.

- The kernel ABI and the user-space ABI are separate contracts.
- Applications use stable, versioned interfaces, not raw kernel pointers or
  internal object layouts.
- Capability handles, IPC endpoints, memory objects, and service requests are
  represented by documented value types and validated at the syscall or service
  boundary.
- The SDK will cover process/task creation, capability handling, file and
  directory access, windows and graphics, input and accessibility, networking,
  audio, notifications, IPC, and portal requests.
- Native applications do not receive unrestricted raw kernel access.

The first user-space ABI will be introduced with the user-mode loader and
syscall gate. Until then, the boot ABI in `abi/` is the only stable ABI.

## Consequences

- The user-space loader, syscall entry, and capability/IPC marshalling must be
  designed together before native applications are supported.
- ABI changes need a version bump and a migration note; they cannot silently
  leak kernel layout changes to applications.
- The SDK can evolve independently from kernel internals.
- The Windows compatibility runtime must use the same documented interfaces as
  native software, not a private privileged path.

## Alternatives considered

- **Expose kernel structures directly to applications:** rejected; it couples
  applications to unstable internals and weakens the privilege boundary.
- **Adopt a POSIX ABI as the native interface:** rejected as the primary ABI.
  POSIX compatibility may be provided by a runtime, but it is not Aether's
  security or capability model.
- **Define the application ABI only after the desktop exists:** rejected;
  retrofitting capability semantics into an ambient-authority API is harder and
  tends to preserve unsafe defaults.

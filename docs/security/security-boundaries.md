# Security Boundaries

This document states what each boundary is expected to enforce and what
evidence is required. A boundary that is not enforced by the kernel or a
resource-owning service is not a security boundary.

## 1. Kernel / application boundary

**Enforced by:** CPU privilege levels, per-address-space page tables, syscall
validation, capability checks.

**Requirements:**

- User code cannot read or write kernel memory.
- Applications cannot name another task's memory or objects without a
  capability.
- System calls validate every pointer, length, handle, and right.
- Faults terminate or isolate the faulting task; they do not panic the kernel.

**Evidence:** Address-space isolation tests (AT-002), fault-containment tests
(AT-010), adversarial syscall tests.

## 2. Application / application boundary

**Enforced by:** separate address spaces, separate capability spaces, separate
private storage.

**Requirements:**

- No ambient filesystem or IPC visibility.
- No access to another application's private data through paths, handles, or
  service APIs.
- Sharing occurs only through explicit capabilities or mediated services.

**Evidence:** AT-005 and sandbox-escape regression tests (AET-056).

## 3. Application / user data boundary

**Enforced by:** `portald` and `fsd` capability checks, not the desktop.

**Requirements:**

- An application has no general access to user directories.
- A grant covers a specific object and specific rights.
- A selected file does not imply access to its parent directory or siblings.
- Persistent grants are visible and revocable.
- Revocation prevents future access according to documented semantics.

**Evidence:** AT-006 and AT-007.

## 4. Service / service boundary

**Enforced by:** capability profiles assigned at service start.

**Requirements:**

- Every service has a documented capability profile.
- Starting early does not confer all rights.
- A compromised service cannot use another service's capabilities.
- Unrelated services continue operating when one is compromised or crashes,
  where dependencies allow.

**Evidence:** AT-008 and per-service review.

## 5. Desktop / policy boundary

**Enforced by:** `securityd`, `portald`, and resource-owning services.

**Requirements:**

- The shell/compositor cannot mint capabilities.
- UI consent is an input to policy, not proof of authorization.
- A compromised desktop cannot silently grant filesystem or device access.

**Evidence:** Code review and adversarial desktop tests.

## 6. Boot boundary

**Enforced by:** loader validation of the kernel image and kernel validation of
the hand-off.

**Requirements:**

- Malformed ELF images are rejected before memory is written.
- BootInfo magic, version, size, map bounds, and region sanity are checked.
- Failure is diagnosable on serial and fails closed.

**Evidence:** Host unit tests for the ELF parser and region sink; boot smoke
test; exception-path self-test.

## 7. Update boundary

**Enforced by:** `updated` and an offline/independent verifier.

**Requirements:**

- Artifacts are authenticated before activation.
- Version downgrade and rollback attacks are rejected.
- Interrupted updates are recoverable.
- User data is preserved across rollback.

**Evidence:** AT-011, AT-012, AT-013.

## Non-boundaries

- A desktop confirmation dialog by itself.
- A pathname check performed only by the application.
- A process identifier or user-supplied object name.
- Documentation claims without a corresponding test.

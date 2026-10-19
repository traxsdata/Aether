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

**Current status:** A ring-3 native application runs in a private PML4 whose
only user mappings are in the validated user window. TSS `RSP0` supplies a
per-task kernel stack, the `int 0x80` gate validates every pointer, length,
handle, generation, object type, and right, and a user fault retires only the
faulting task. The boot test attempts a cross-root read and a kernel-pointer
syscall and requires both to be denied. Kernel text, stacks, page tables, and
the identity map remain supervisor-only.

**Remaining gap:** services still execute as kernel modules; moving them to
ring 3 is the next boundary slice. There is no kernel heap, SMP, or general
capability revocation yet.

## 2. Application / application boundary

**Enforced by:** separate address spaces, separate capability spaces, separate
private storage.

**Requirements:**

- No ambient filesystem or IPC visibility.
- No access to another application's private data through paths, handles, or
  service APIs.
- Sharing occurs only through explicit capabilities or mediated services.

**Current status:** Each application has a kernel-owned root (`/apps/demo`,
`/apps/other`, and `/apps/<package id>` for installed packages). Path
resolution rejects `..`, and file handles are per-task generation-checked
capabilities. The native sandbox test attempts a sibling path and a root
escape; both fail with `EPERM`. A packaged application's runtime prefix is
derived from its identity, not from the manifest, and prefix sharing requires
an explicit consent decision. The filesystem is a volatile RAM disk, so this is
not yet persistent private storage.

**Evidence:** AT-005 and the boot-time sandbox test (`AETHER_SANDBOX_OK`).

## 3. Application / user data boundary

**Enforced by:** `portald` and `fsd` capability checks, not the desktop.

**Requirements:**

- An application has no general access to user directories.
- A grant covers a specific object and specific rights.
- A selected file does not imply access to its parent directory or siblings.
- Persistent grants are visible and revocable.
- Revocation prevents future access according to documented semantics.

**Current status:** The portal holds exact `(process root, path, rights)`
grants. The native application receives one read grant for
`/home/welcome.txt`; it cannot name other user files. Grant records are visible
in the Security Center. Interactive consent and revocation UI are follow-up
work.

**Evidence:** AT-006 and the `AETHER_PORTAL_OK` boot marker.

## 4. Service / service boundary

**Enforced by:** capability profiles assigned at service start.

**Requirements:**

- Every service has a documented capability profile.
- Starting early does not confer all rights.
- A compromised service cannot use another service's capabilities.
- Unrelated services continue operating when one is compromised or crashes,
  where dependencies allow.

**Current status:** The supervisor registers storaged, fsd, appd, consoled,
inputd, displayd, desktopd, portald, and securityd with declared capability
profiles and dependency order. A failing probe is restarted once and then
quarantined; the boot test requires that outcome. Service bodies still run in
the kernel, so this is a policy prototype rather than a completed isolation
boundary.

**Evidence:** AT-008, `AETHER_SUPERVISOR_OK`, and host tests for dependency
and restart rules.

## 5. Desktop / policy boundary

**Enforced by:** `securityd`, `portald`, and resource-owning services.

**Requirements:**

- The shell/compositor cannot mint capabilities.
- UI consent is an input to policy, not proof of authorization.
- A compromised desktop cannot silently grant filesystem or device access.

**Current status:** The compositor paints pixels and displays supervisor,
filesystem, application, and portal state. It cannot mint a file capability or
bypass the filesystem root check; portal opens are decided by the portal
service. There is no interactive consent dialog yet.

**Evidence:** Code review and the portal/sandbox boot markers.

## 6. Boot boundary

**Enforced by:** loader validation of the kernel image and kernel validation of
the hand-off.

**Requirements:**

- Malformed ELF images are rejected before memory is written.
- BootInfo magic, version, size, map bounds, and region sanity are checked.
- Failure is diagnosable on serial and fails closed.

**Evidence:** Host unit tests for the ELF parser and region sink; boot smoke
test; exception-path self-test.

## 7. Package boundary

**Enforced by:** `aether-packaging` verification in the kernel, the publisher
trust store, and manifest policy.

**Requirements:**

- A package is installed only after framing, manifest policy, publisher trust,
  signature, and payload digest checks.
- An application cannot request network, device, or system-service authority in
  a manifest.
- Installation does not execute anything.
- The storage root follows from the application identity.

**Current status:** Implemented for `format=1`. The kernel installs a verified
package at `/apps/<id>` and rejects an unknown publisher, a modified payload,
and a zeroed signature at boot (`AETHER_PACKAGE_OK`). Requested permissions are
an allowlist and limits are range-checked. Executing a packaged entry point and
persisting packages are not implemented.

**Evidence:** Host tests in `packaging/`, `updates/`, and the boot self-test.

## 8. Update boundary

**Enforced by:** `aether-update` verification and activation policy, run by the
kernel and by release tooling.

**Requirements:**

- Artifacts are authenticated before activation.
- Version downgrade and rollback attacks are rejected.
- An unknown publisher cannot install an image.
- Interrupted updates are recoverable.
- User data is preserved across rollback.

**Current status:** The update verifier checks framing, manifest fields,
publisher trust, Ed25519 signature, and image digest. Policy requires a strictly
increasing counter, a newer version unless a recovery override is explicit, and
a matching channel. The boot test accepts a newer signed image and rejects
replay, downgrade, and an untrusted key (`AETHER_UPDATE_OK`). The recovery
prototype still verifies current/previous image markers and selects the previous
one when the current marker is missing (`AETHER_RECOVERY_OK`).

**Remaining gap:** Activation, staging, and reboot into a pending image are not
implemented, and the installed counter lives in the writable image. Secure Boot,
measured boot, and TPM-backed counters are required before this is a meaningful
boundary against an attacker who can rewrite storage.

**Evidence:** Host tests in `updates/`, the boot self-test, and
`make release-check`.

## Non-boundaries

- A desktop confirmation dialog by itself.
- A pathname check performed only by the application.
- A process identifier or user-supplied object name.
- A package signature without a pinned publisher key and a digest check.
- A compatibility prefix presented as a sandbox.
- Documentation claims without a corresponding test.

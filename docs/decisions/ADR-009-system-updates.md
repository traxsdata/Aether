# ADR-009: System updates

- **Status:** Accepted for the prototype; implementation pending
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

An operating system that cannot be updated safely is not maintainable, and an
update mechanism that can be tricked into activating a corrupted or rolled-back
image can destroy the system. User data must survive both updates and recovery.

## Decision

Use signed, versioned system images with a recoverable update process.

- System images and update manifests are authenticated before activation.
- Version policy rejects unauthorized downgrades and rollback attacks.
- Applications and user data are stored separately from the immutable system
  image.
- An interrupted update leaves either the previous working system or a
  recoverable state; it must not leave an unbootable hybrid.
- A recovery image can select a previous valid image without deleting user data.
- Cryptographic keys are not exposed to ordinary applications.
- The update verifier and recovery workflow are user-space services; the kernel
  provides only the primitives required to boot a verified image where the
  platform supports it.

This decision does not claim that Secure Boot, measured boot, or a complete
update implementation exists. Those are release-engineering deliverables.

## Consequences

- Release engineering must produce reproducible artifacts, checksums, and
  signatures.
- Key management and revocation procedures are required before public release.
- Update and recovery tests (AT-011, AT-012, AT-013) are mandatory.
- The system image must be designed as immutable from the start; retrofitting
  separation between system files and user data is risky.

## Alternatives considered

- **Mutable system installation updated in place:** rejected as the default;
  it complicates rollback, integrity verification, and recovery.
- **Unsigned development images distributed as releases:** rejected; they may
  be acceptable for local development but cannot support a security claim.
- **Update the kernel alone without a recovery path:** rejected; a failed kernel
  update can leave the machine unusable.

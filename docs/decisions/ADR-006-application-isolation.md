# ADR-006: Strict application isolation

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Desktop operating systems commonly give applications ambient access to user
files, devices, and other applications' data. Aether's defining security
property is the opposite: an application has no authority unless it has been
granted a capability for a specific object and operation.

## Decision

Deny filesystem and device access by default.

- Every application runs in a distinct security domain with private storage.
- Private application storage is readable and writable only by the owning
  application, subject to policy and administrative controls.
- User files are reached through explicit, scoped grants mediated by a trusted
  portal or resource-owning service.
- The kernel and trusted services enforce the boundary. A desktop dialog is not
  enforcement.
- Network, clipboard, device, and IPC access are capabilities, not ambient
  privileges.
- Grants can be temporary or persistent, are visible to the user, and have
  defined revocation semantics.
- A grant for one file does not imply access to its parent directory, siblings,
  or unrelated files.

## Consequences

- The file portal, filesystem service, security policy service, and application
  lifecycle manager are required before native applications can safely open user
  files.
- Compatibility runtimes must route file, network, and device access through the
  same boundaries; Wine prefixes are not treated as security sandboxes.
- Sandbox-escape and unauthorized-access tests are mandatory release gates.
- Some applications will require explicit user grants or will be unsupported
  where the required authority cannot be safely mediated.

## Alternatives considered

- **Traditional user/group permissions as the primary model:** rejected as
  insufficient; they give broad ambient authority and do not bind access to a
  specific selected object.
- **Path-based sandboxing:** rejected as the sole mechanism; path validation is
  vulnerable to links, replacement, traversal, and confused-deputy bugs.
- **Trust the application's manifest:** rejected; declared permissions are a
  request, not proof of enforcement.

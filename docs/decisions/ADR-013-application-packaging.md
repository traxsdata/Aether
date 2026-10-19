# ADR-013: Application packaging and runtime isolation

- **Status:** Accepted; `format=1` implemented
- **Date:** 2026-10-19
- **Supersedes:** none
- **Related:** ADR-006, [package format](../specifications/package-format.md)

## Context

Applications need a verifiable identity, a declared set of permissions, and a
storage environment that is not chosen by the application itself. Without a
format, "install an app" means copying an arbitrary file into an arbitrary
directory and running it with whatever authority the system happens to have.

The security model requires that:

- installation does not execute anything;
- a package cannot request network, device, or system-service authority;
- the storage root follows from the identity, not from a manifest field;
- the verifier that runs in the kernel is the verifier that runs in release
  tooling.

## Decision

Adopt a signed `.aether` container with a canonical text manifest, a bounded flat
file archive, and an Ed25519 signature over the manifest. The manifest carries:

- reverse-domain identity and semantic version;
- architecture and runtime (`native` only);
- entry point and publisher key;
- update channel;
- requested permissions from a fixed allowlist;
- memory, open-file, and task limits;
- the SHA-256 digest of the payload.

Installation verifies framing, manifest policy, publisher trust, signature,
payload digest, archive structure, and entry-point presence, in that order. The
runtime prefix is derived as `/apps/<id>`. Prefix sharing is an explicit,
consented exception. Network and device access are not permissions, so they
cannot be granted by a manifest.

`aether-packaging` is `no_std`, dependency-free apart from `aether-crypto`, and
shared by the kernel and host tooling. The signing tool is a separate host
binary; the kernel links only the verifier.

## Consequences

- A package signature is evidence of authorship and integrity, not of safety.
- Manifest changes require a format version bump; unknown keys and unknown
  permissions fail closed.
- Executing a packaged entry point is not implemented yet. Installation and
  isolation are implemented and tested; claiming application launch would be
  false.
- The filesystem's 16-character name and 4 KiB file limits constrain payloads
  until persistent storage exists.
- A release trust root and key rotation are separate release-engineering work
  (ADR-014).

## Alternatives considered

- **Unsigned archives with a checksum only:** rejected; integrity without
  authorship does not stop a malicious mirror.
- **Manifest chooses the install path:** rejected; it lets one application place
  files in another's root.
- **Permissions granted at first launch by the application:** rejected; the
  package must be reviewable before installation, and consent UI does not exist.
- **Separate host and kernel parsers:** rejected; divergence is a
  verification-bypass risk.
- **Accept a `windows` runtime now:** rejected; no compatibility environment
  exists, so the package would install a promise the system cannot keep.

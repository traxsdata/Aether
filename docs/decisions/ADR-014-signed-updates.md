# ADR-014: Signed updates and rollback protection

- **Status:** Accepted; verifier and policy implemented, activation pending
- **Date:** 2026-10-19
- **Supersedes:** none
- **Related:** ADR-009, [signed updates](../architecture/updates.md)

## Context

ADR-009 requires authenticated system images, version and rollback protection,
recoverable updates, and preservation of user data. A verifier that only checks
a checksum cannot detect a correctly formatted but malicious or replayed image.
A verifier that accepts an older signed image can be used to reintroduce a fixed
vulnerability.

The release process also needs a single artifact that ties a commit, a toolchain,
an ISO, and a publisher key together without relying on a manual checklist.

## Decision

Use a signed update manifest (`AETHERUP`, format 1) as both the update and
release descriptor:

- image name and version;
- monotonically increasing `u64` rollback counter;
- channel;
- publisher name and Ed25519 key;
- SHA-256 digest of the image.

The verifier checks framing, manifest policy, trust-store membership, publisher
name, signature, and digest. Activation policy is a separate step and requires:

1. the counter to be greater than the installed counter (replay/downgrade
   protection);
2. the version to be greater than the installed version unless an explicit
   recovery option is set;
3. the channel to match the installed channel.

Release bundles are assembled by `aether-admin`, which signs with a key read from
a file outside the repository and refuses to produce an unsigned bundle. The
bundle carries `SHA256SUMS`, `release-info.txt`, the image, and its manifest. A
verification command checks the whole bundle.

The kernel runs the same `aether-update` crate at boot against embedded fixtures
to prove acceptance and all three rejection paths, and records the accepted
manifest in `/system/updates/selected`.

## Consequences

- A signed manifest that is replayed or downgraded is rejected by policy, not
  only by signature.
- Counters must be managed; the default is the git commit count, which is
  monotonic on the main line and deterministic for a commit.
- The installed counter lives in the writable system image today. An attacker
  who can rewrite storage can reset it. TPM-backed or immutable counters are
  required before the protection is meaningful against that adversary.
- Activation, staging, and reboot into a pending image remain unimplemented;
  the recovery selector and verification exist.
- The ISO digests are recorded per release rather than claimed bit-for-bit
  reproducible, because the ISO embeds build timestamps.
- Secure Boot, measured boot, and offline key custody are still release
  engineering work.

## Alternatives considered

- **Checksums only:** rejected; no authorship and no replay protection.
- **Version-only policy:** rejected; a re-signed older version with a lower
  number is detectable, but replay of a valid image with the same version is
  not.
- **Counter-only policy:** rejected; it allows a higher-counter older version to
  reintroduce fixed bugs without an explicit recovery decision.
- **Automatic downgrade allowance to simplify recovery:** rejected; recovery
  must be a deliberate, auditable action.
- **Signing in the kernel or on the build host by default:** rejected; secret
  keys stay in an offline developer workflow and never in the kernel image.

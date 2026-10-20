# Signed Updates and Recovery

**Related:** ADR-009, ADR-014, [release policy](../security/release-policy.md),
technical specification §7 and §8.3

## Purpose

A system image is activated only when a signed manifest authenticates it and
activation policy accepts it. This document defines the manifest, the verifier,
the rollback rules, the release bundle, and the recovery procedure that exists
today.

## Non-claims

- There is no Secure Boot signing, measured boot, TPM-backed counter, or
  bootloader verification yet.
- The release bundle is produced and verified by host tooling; the running
  system verifies the *manifest* against image bytes it embeds for the test.
  Replacing the running image is not implemented.
- Keys in `tests/fixtures/keys` are test keys. A release requires a key outside
  the repository.
- An attacker who can rewrite the boot media and its update state is out of
  scope for this prototype.

## Update manifest

An update (and a release) manifest is a signed record:

```text
magic       8 bytes  "AETHERUP"
version     u16 LE   1
body        u32 LE length, then UTF-8 manifest text
payload     u32 LE length, zero for update manifests
signature   64 bytes Ed25519 over the body
```

Manifest fields:

```text
format=1
image=image-0.2.0.bin
version=0.2.0
counter=2
channel=developer
publisher=Aether Test Publisher
publisher-key=<64 hex characters>
image.sha256=<64 hex characters>
```

- `image` is a safe relative name recorded for the artifact.
- `version` is a `MAJOR.MINOR.PATCH` version.
- `counter` is a monotonically increasing `u64` chosen by release engineering.
  The default in `tools/release.sh` is the git commit count, which is
  monotonic on the main line and deterministic for a given commit.
- `channel` is `developer`, `beta`, or `stable`.
- The signature covers the exact manifest bytes; the digest covers the image.

## Verification

`aether_update::verify_update` checks framing, manifest fields, trust-store
membership, publisher name, signature, and the image digest. It also rejects a
non-empty record payload: the update format carries no unsigned data, and
accepting trailing bytes would make the record larger than the authenticated
manifest describes. The kernel runs the same crate as the release tooling.

## Activation policy

`aether_update::check_policy` rejects:

| Rule | Rejection |
|---|---|
| `counter` must be greater than the installed counter | `Rollback` (replay or downgrade attack) |
| `version` must be greater than the installed version | `Downgrade` |
| `channel` must equal the installed channel | `ChannelMismatch` |

A deliberate recovery action may pass `allow_older_version`, which still
requires a newer counter. A normal update never enables it.

Installed state is read from `/system/install.txt`:

```text
version=0.1.0
counter=1
channel=developer
```

## Boot self-test

The boot verifies four embedded fixtures:

| Fixture | Result |
|---|---|
| `current` (0.2.0, counter 2, test key) | accepted |
| `previous` (0.1.0, counter 1) | `Rollback` |
| `downgrade` (0.0.9, counter 3) | `Downgrade` |
| `forged` (0.3.0, counter 4, untrusted key) | `UnknownPublisher` |

After recording the accepted manifest in `/system/updates/selected`, the boot
also replays that same manifest against the state that is now installed: the
activation advanced the installed counter to the accepted manifest's counter,
so presenting it again must be refused as a `Rollback`. This is the evidence
that replay protection survives the activation record.

The kernel prints `AETHER_UPDATE_OK accepted=1 rejected=4
replay_protected=true` and the `updates` shell command reports installed and
available state.

## Recovery prototype

The recovery module keeps current/previous image markers under
`/system/images/`, selects the previous marker when the current one is missing,
records the selection, and verifies that the user-data sentinel is readable
(`AETHER_RECOVERY_OK`). User data is outside the image set and is never
overwritten by selection.

The update verifier and the recovery selector are separate steps today. The
intended order for an activation path is:

1. verify the candidate manifest and image digest;
2. apply activation policy against installed state;
3. stage the image and write a *pending* marker;
4. boot the pending image; on failure, the recovery selector falls back to the
   previous verified image;
5. commit the new version and counter only after a successful boot.

Steps 3–5 are documented here and not implemented beyond the selection markers.

## Release bundle

```sh
# Signing key: a 32-byte hex seed, kept outside the repository.
make release \
    AETHER_SIGNING_KEY=/secure/keys/aether-release.seed \
    AETHER_PUBLISHER="Aether Contributors" \
    AETHER_VERSION=0.2.0 \
    AETHER_RELEASE_CHANNEL=developer
```

`tools/release.sh` builds the ISO, then `aether-admin release build` writes:

```text
build/release/
├── aether-0.2.0.iso
├── aether-0.2.0.iso.manifest   signed update manifest
├── SHA256SUMS                  image and manifest digests
└── release-info.txt            version, counter, channel, commit, image digest
```

Unsigned releases are refused. Verification:

```sh
make release-check AETHER_TRUST="Aether Contributors:<public key hex>"
```

Tampered images, unknown publishers, malformed bundles, and mismatched digests
are rejected. `make release-demo` performs the same flow with the checked-in
test key to prove the mechanism; it is not a release.

## Key management

- Release seeds are generated with `openssl rand -hex 32` on an offline host.
- The seed never enters the repository, the environment of a build service
  without access control, or a command line. `aether-admin` reads only a file.
- The public key and publisher name are published and pinned in the trust
  store. Key rotation uses an overlap window in which both old and new keys are
  trusted, then removes the old key in a later release.
- Compromise of a signing key follows
  [`security-response.md`](../security/security-response.md).

## Reproduction and digests

The kernel and loader ELF are built from a pinned toolchain and `--locked`
dependencies. The ISO embeds build timestamps, so its digest (and therefore the
signed artifact) is recorded per release rather than claimed to be bit-for-bit
reproducible. `make qualify` records the commit, toolchain, emulator, artifact
digests, and required boot markers in `build/qualification/report.md`.

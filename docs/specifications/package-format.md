# Application Package Format

**Document ID:** AETHER-PKG-001
**Status:** Implemented for `format=1`; treat as unstable until 1.0
**Related:** ADR-013, [package verification](#verification), technical
specification §8

## Purpose

An `.aether` package carries a native application's metadata, its files, its
requested permissions, and its resource limits. Installation is allowed only
after a signature and a digest have been verified, and the installed runtime
environment is derived from the manifest rather than chosen by the application.

The same `aether-packaging` crate parses packages in the kernel and in the
release tooling. There is no second, more permissive host-side parser.

## Non-claims

- A valid signature proves authorship and integrity, not that the software is
  benign.
- The current verifier accepts only compiled native applications; there is no
  Windows or other compatibility runtime.
- Package installation does not execute anything. Executing a packaged entry
  point is not implemented: the kernel still embeds its single native program.
- The filesystem is a volatile RAM disk, so installed packages do not survive a
  reboot.

## Container

All integers are little-endian. The container is a canonical signed record:

| Offset | Size | Field |
|---|---:|---|
| 0 | 8 | magic `AETHERPK` |
| 8 | 2 | framing version (`1`) |
| 10 | 4 | manifest length |
| 14 | 4 | payload length |
| 18 | *manifest length* | manifest text (UTF-8) |
| — | *payload length* | payload archive |
| — | 64 | Ed25519 signature over the manifest bytes |

The signature covers the manifest, and the manifest contains the SHA-256 digest
of the payload. One signature therefore authenticates both. The verifier rejects
unknown framing versions, truncated records, and trailing bytes.

Limits: manifest ≤ 4 KiB, payload ≤ 64 KiB, at most 16 files, each ≤ 4 KiB, and
each file name ≤ 16 bytes (a limit of this packaging framing, independent of
the filesystem's 255-byte name limit).

## Manifest

The manifest is a fixed set of `key=value` lines. The order below is canonical
and is what [`Manifest::render`](../../packaging/src/manifest.rs) emits.

```text
format=1
id=org.aether.demo
version=0.2.0
arch=x86_64
runtime=native
entry=demo.bin
publisher=Aether Test Publisher
publisher-key=<64 hex characters>
channel=developer
permissions=file.read,file.write
limits.memory_kib=256
limits.open_files=4
limits.tasks=1
payload.sha256=<64 hex characters>
```

| Key | Rule |
|---|---|
| `format` | Must be `1`. |
| `id` | Reverse-domain identity: lowercase letters, digits, `.`, `-`; 2–4 labels, each ≤ 20 characters, no leading/trailing hyphen; ≤ 63 characters. Display names and install paths are not identity. |
| `version` | `MAJOR.MINOR.PATCH`, decimal, no leading zeros. |
| `arch` | `x86_64`. Any other value is unsupported. |
| `runtime` | `native`. `windows` is recognised and rejected until a runtime exists; unknown values are rejected. |
| `entry` | Relative path (1–4 components, each ≤ 16 safe characters), must name a payload file. |
| `publisher` | Printable ASCII, 1–64 characters, must match the trusted publisher entry for the key. |
| `publisher-key` | 32-byte Ed25519 public key in hex. |
| `channel` | `developer`, `beta`, or `stable`. |
| `permissions` | Comma-separated allowlist below, or `none`. Duplicates and unknown names are rejected. |
| `limits.memory_kib` | 64–4096. |
| `limits.open_files` | 1–8. |
| `limits.tasks` | 1–8. |
| `payload.sha256` | SHA-256 of the payload bytes. |

Unknown keys, duplicate keys, empty values, non-printable characters, and
trailing data are rejected. The parser fails closed; it never ignores a field it
does not understand.

### Permissions

| Name | Meaning |
|---|---|
| `file.read` | Read inside the application root. |
| `file.write` | Write inside the application root. |
| `file.create` | Create files inside the application root. |
| `portal.read` | Read a file through a portal grant. |
| `portal.write` | Write a file through a portal grant. |
| `input.read` | Read input events granted by the session. |
| `surface.compose` | Compose its own window surface. |

There is deliberately no network, device, package-manager, system-image,
`grant.issue`, `process.spawn`, or policy-administration permission. A package
requesting one of those is rejected; an application cannot become a system
service by declaring it in a manifest.

## Payload

The payload is a flat archive:

```text
u16 count
repeat count times:
    u16 name_length, name bytes
    u32 data_length, data bytes
```

Names are relative, unique, lowercase, and must not contain `/`, `.`, or `..`.
A payload that encodes a path traversal is rejected after its digest matches, so
the rejection exercises the archive rules rather than the digest check.

## Verification

`aether_packaging::verify_package` runs, in order:

1. framing, version, and length bounds;
2. manifest structure and value policy;
3. trust-store lookup of `publisher-key`;
4. publisher name equality with the trusted entry;
5. Ed25519 signature over the exact manifest bytes;
6. SHA-256 of the payload;
7. payload archive structure and entry-point presence.

Any failure returns an error and writes nothing.

## Installation and runtime environment

After verification, the kernel creates `/apps/<id>` and writes each payload file
there. The identity also determines the runtime prefix; a package cannot place
itself in another application's storage.

`aether-services::runtime` derives a profile from the verified manifest:

- permissions and limits are copied from the manifest, never widened;
- paths are checked component-by-component before the filesystem sees them;
- default profiles are isolated, and adopting another application's prefix
  requires an explicit consent decision;
- network and device access do not exist, so they are denied by construction.

The boot self-test (`AETHER_PACKAGE_OK`) requires one installation, three
rejections (unknown publisher, modified payload, zeroed signature), and checks
runtime containment (`AETHER_RUNTIME_OK`).

## Tooling

```sh
# Build and sign a package with a key outside the repository.
cargo run -q -p aether-admin -- package build \
    --manifest app.txt --payload-dir payload --seed /path/to/key.seed --out app.aether

# Verify a package against a trusted publisher.
cargo run -q -p aether-admin -- package verify \
    --package app.aether --trust "Publisher Name:<public key hex>"
```

`app.txt` omits `publisher-key` (the tool derives it from the seed) and
`payload.sha256` (the tool computes it). The tool renders the canonical
manifest, signs it, and writes the container.

## Test fixtures

`tests/fixtures/` contains packages signed with checked-in **test** keys. They
exist to exercise the verifier in the boot test. They are not a release trust
root; see [`tests/fixtures/README.md`](../../tests/fixtures/README.md).

## Follow-up work

- Execute packaged applications at CPL3 from a verified entry point.
- Persistent installation and per-application storage.
- Revocation of installed packages and their grants.
- Interactive permission consent for `portal.*` and `input.read`.
- A real release trust root and key-rotation policy (see ADR-014).

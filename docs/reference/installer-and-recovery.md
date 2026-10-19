# Installer and Recovery Workflow

**Status:** Recovery prototype and signed release verification are implemented.
A general installer is not.

This document describes what a user or release engineer can do today, in exact
commands, and what is still missing. Nothing here should be used with real data.

## Build and boot

```sh
git clone https://github.com/traxsdata/Aether.git
cd Aether
. "$HOME/.cargo/env"
make            # build/aether.img (64 MiB FAT)
make iso        # build/ISOs/aether-<UTC>.iso
make test       # boot under QEMU/OVMF and require every marker
```

Booting the ISO or FAT image on the supported QEMU platform is the only
installation procedure. There is no disk partitioner or system-image writer.

## Release bundle

```sh
make release \
    AETHER_SIGNING_KEY=/secure/keys/aether-release.seed \
    AETHER_PUBLISHER="Aether Contributors" \
    AETHER_VERSION=0.2.0
make release-check AETHER_TRUST="Aether Contributors:<public key hex>"
```

A bundle contains the ISO, its signed update manifest, `SHA256SUMS`, and
`release-info.txt`. The verification command rejects a modified image, a wrong
publisher key, or a malformed manifest. See
[signed updates](../architecture/updates.md).

`make release-demo` runs the same flow with the checked-in test key. It proves
the mechanism and is not a releasable artifact.

## What the system verifies at boot

1. Loader: embedded kernel ELF structure and bounds.
2. Kernel: hand-off, memory map, page tables, and NX/WP.
3. Recovery: current/previous image markers and the user-data sentinel
   (`AETHER_RECOVERY_OK`).
4. Updates: signed manifests, image digests, publisher trust, and rollback or
   downgrade rejection (`AETHER_UPDATE_OK`).
5. Packages: signature, digest, publisher, permissions, and runtime profile
   (`AETHER_PACKAGE_OK`, `AETHER_RUNTIME_OK`).

The `recovery`, `updates`, and `packages` shell commands report the state.

## Recovering from a failed image (prototype)

The recovery module selects the previous image when the current marker is
missing and records the choice in `/system/images/selected`. User data under
`/home` is outside the image set and is never overwritten.

Manual procedure on the current tree:

1. Boot the image and open the console.
2. Run `recovery` and confirm `selected` and `rollback`.
3. Run `updates` to confirm the installed version/counter and the accepted
   candidate.
4. Rebuild or re-apply the previous image from the release bundle and reboot.

Because storage is a volatile RAM disk, "recovery" is a boot-time selection
exercise; it does not survive a power cycle. A reboot returns to the seeded
state.

## Qualification

```sh
make qualify
```

This runs the full gate and writes `build/qualification/report.md` with the
commit, toolchain, emulator, step results, boot markers, and release digests.
The report is the release-manager input; a green run is automated evidence, not
an external audit.

## Missing pieces

- A UEFI installer that writes system and user partitions.
- A persistent boot loader menu with pending/confirmed image slots.
- Updating the running image after verification (staging and activation).
- A signed recovery environment separate from the installed system.
- Rollback counters stored outside the writable system image.
- Secure Boot signing and measured boot.

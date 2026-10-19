# Secure Boot and the release trust chain

æther does not need Secure Boot to enforce its own rules: the loader checks
the system image's RSA-2048 signature and hash before it maps the kernel, and
the kernel re-checks app packages. Secure Boot adds the first link: firmware
only starts an approved loader, so an attacker cannot replace the loader and
skip those checks.

## What is signed

| Piece | Signed by | Checked by |
|---|---|---|
| `BOOTX64.EFI` (loader) | the machine owner's Secure Boot key (`db`) | firmware |
| `aether-system-x86_64.img` | the offline æther release key | loader and kernel |
| app packages from a release | the same release key (1.4) | kernel `SYS_APP_INSTALL` with `APP_INSTALL_SIGNED` |
| the ESP boot image hash | pinned inside the signed system image | loader |

The kernel is not a PE image; firmware never starts it directly. It is part
of the signed system image, and the loader verifies that image before
jumping to the kernel. This is the same chain whether the machine uses
Secure Boot or not.

## Signing the loader

```sh
make
tools/sign-secureboot.sh /secure/db.key /secure/db.crt
make boot-image images
```

`tools/sign-secureboot.sh` replaces `build/BOOTX64.EFI` with an `sbsign`
copy and removes the derived images. Enroll `db.crt` in the firmware setup
(or with `sbctl enroll-keys`) as an owner action. Rebuilding the ISO without
re-signing produces a loader that firmware refuses, which is intentional.

## Key rotation

The release key is replaced in this order:

1. Generate a new offline key with `tools/make-signing-key.py --key ...`.
2. Generate two system images: one signed by the old key and one by the new
   key, both with the same `version_code`.
3. Ship an update signed by the old key; it does not need the new public key.
4. Update `common/release_key.h` to the new modulus. From this release on,
   both the loader and kernel trust only the new key.
5. Cut the next release with the new key and keep the old private key only
   long enough to sign the transition image.

A loader built with the new key cannot boot images signed by the old one, so
step 3 must complete before step 4 is published. The rollback floor
(`bootcontrol.min_version`) prevents an old signed image from being installed
after the transition.

## Recovery key

Keep a second Secure Boot certificate and private key offline. Losing the
loader key is recoverable only by enrolling the recovery certificate in
firmware and booting a recovery loader. Losing the release key means existing
installs cannot accept updates that use a new trust anchor; publish the new
modulus in a transition release and document the manual recovery path.

# Review of 1.4: trust and release

Based on `v1.3` (PR #24), with the 1.x plan restored from `v1.1` (PR #23).
This slice covers signed packages, Secure Boot signing and reproducible
release evidence; it does not claim the 1.1 runtime or 1.2 Claude Code work.

## Changes

- **App package signatures.** The app header gains signature fields and an
  optional 256-byte RSA-2048 PKCS#1 v1.5 SHA-256 trailer after the icon. The
  trailer covers the header, ELF and icon with the signature fields filled in,
  so any change to the program, icon or metadata invalidates it. The kernel
  verifies it against `common/release_key.h`; an invalid signature is always
  refused, and `APP_INSTALL_SIGNED` makes it mandatory on the App Store's
  network path. Local files remain installable and are not silently treated as
  publisher-authenticated.
- **Build tooling.** `tools/mkapp.py --key` signs and refuses a key that does
  not match the kernel's release anchor. `AETHER_APP_SIGNING_KEY` (defaulting
  to `AETHER_SIGNING_KEY`) signs the ISO's system packages. The release
  workflow materializes the key before the first build and removes it at the
  end.
- **Secure Boot.** `tools/sign-secureboot.sh` signs the UEFI loader with
  `sbsign`; `docs/SECUREBOOT.md` records what is signed, the limits of signing
  the loader only, and the key-rotation order.
- **Reproducible release.** `SOURCE_DATE_EPOCH` controls the build date, and
  `docs/RELEASE-CHECKLIST.md` requires two clean builds with matching
  `SHA256SUMS`.
- **Tests.** The kernel verifies a fixed known-good RSA signature and rejects a
  changed message or signature, and checks the package signature trailer's
  bounds and terms. `make unittest` is 58 kernel / 70 terminal.

## Evidence

```text
make            PASS
make unittest   58 kernel / 70 terminal, 0 failed
tools/mkapp.py --key ... (verified with openssl dgst -verify)
```

The local test key was generated outside the tree and the release anchor was
restored afterwards; no private key is committed.

## Not done

- The release workflow was changed but not run here (no repository secret).
- Physical Secure Boot enrolment and a TPM-backed rollback counter.
- Kernel/app ASLR, KASLR and the remaining 1.5 hardware work.

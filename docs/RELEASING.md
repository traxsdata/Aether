# Releasing æther

Two GitHub Actions workflows publish what the installer and the App Store's
OS tab download:

- `.github/workflows/release.yml` builds a normal `v*` OS release with
  `aether-x86_64.iso`, `aether-boot-x86_64.img`,
  `aether-system-x86_64.img` and `SHA256SUMS`. The system image is
  RSA-2048-signed with the release key.
- `.github/workflows/netboot.yml` builds the stable netboot bootstrap only on
  a deliberate `netboot-v*` tag (or a manual run from `main`) and publishes it
  as a pre-release, so it is excluded from `releases/latest`.

Both jobs run in the `release` environment. The OS workflow refuses to
publish if the signing secret is missing. The netboot workflow builds only
the unsigned bootstrap ISO and does not read the key.

## One-time setup (repository owner)

1. **Settings → Environments → New environment** named `release`:
   - add yourself as a **required reviewer**, so every release run waits for
     your approval;
   - under deployment branches and tags, allow only `v*` and `netboot-v*`
     tags (and `main` for the manual netboot run).
2. In that environment, add the secret **AETHER_SIGNING_KEY**: the PEM text of
   the offline RSA-2048 private key whose public modulus is in
   `common/release_key.h`. Do not add it as a repository-wide secret.
3. **Settings → Rules → Rulesets → New tag ruleset** targeting `v*` and
   `netboot-v*`: restrict creation, update and deletion to yourself.

## Cutting a release

```sh
git tag v0.8.0 && git push origin v0.8.0          # OS release (signed images)
git tag netboot-v1 && git push origin netboot-v1  # only when the installer changes
```

Approve the waiting run under **Actions**. The tag's version (without the
`v`) becomes the system image version; it must be higher than the
`min_version` floor of installs that should accept it.

## Checking a release

- `SHA256SUMS` matches the downloaded assets.
- Boot `aether-netboot-x86_64.iso` in VMware with a blank disk, install, and
  check the installed system boots and unlocks.
- Tag a newer `v*` release, run **Check for OS updates** in App Store, and
  check the update lands in the other slot and is marked good on reboot.

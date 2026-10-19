# Release review checklist

Two people review a release that changes keys, boot control or update code.
The release owner runs the commands; the reviewer checks the evidence.

## Build

- [ ] `git status` is clean and the tag points at the reviewed commit.
- [ ] `SOURCE_DATE_EPOCH` is the tag's commit time. Build twice from two
      clean checkouts and compare `SHA256SUMS`.
- [ ] `make test`, `make unittest`, `make test-lock`, `make test-power` pass.
- [ ] `AETHER_APP_SIGNING_KEY` and `AETHER_SIGNING_KEY` point at the offline
      release key, and every app package is reported as `signed`.
- [ ] `tools/mksysimage.py` confirms the key matches
      `common/release_key.h`; the release workflow refuses a mismatch.
- [ ] `SHA256SUMS` matches every published asset.

## Update and rollback

- [ ] A netboot install on a blank disk reaches the desktop and unlocks.
- [ ] An older signed image is refused by both the loader and
      `SYS_UPDATE_INSTALL` (rollback floor).
- [ ] An update lands in the inactive slot, is tried twice, and is marked
      good; an image that cannot mount storage falls back.
- [ ] A package with a changed ELF, icon or header is refused by the kernel.
- [ ] An unsigned download is refused by the App Store path; a local unsigned
      file is clearly presented as unsigned before install.

## Keys

- [ ] The private release key was never in the repository, CI logs or an
      artifact.
- [ ] The `release` environment required reviewer approval for this run.
- [ ] The tag ruleset blocks updates and deletions.
- [ ] If a key changed, `docs/SECUREBOOT.md`'s rotation order was followed.

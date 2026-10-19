# Governance

Aether is community-governed. The technical specification is the baseline
architecture; changes to it require a public decision record.

## Roles

- **Project maintainers** manage releases, repository health, and the roadmap.
- **Kernel maintainers** review privileged code, the boot path, memory
  management, and the kernel ABI.
- **Security team** handles private reports and reviews changes that cross a
  trust boundary.
- **Subsystem maintainers** own defined services, drivers, or desktop
  components once those components exist.
- **Release manager** coordinates qualification and release notes.
- **Contributors** submit issues, documentation, tests, and code.

Roles may be held by the same person while the project is small. New
maintainers are added by consensus of the existing maintainers based on a
sustained contribution record.

## Decisions

- Architecture decisions are recorded in `docs/decisions/ADR-NNN-*.md`.
- An ADR states context, decision, consequences, and alternatives considered.
- Accepted ADRs are not silently edited. A later ADR supersedes an earlier one
  and links to it.
- Security-critical changes require review by a kernel or security maintainer.
- Mandatory security tests cannot be waived without a documented exception in
  the pull request and release notes.

## Contribution and conduct

All participants must follow [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).
Contribution requirements are in [`CONTRIBUTING.md`](CONTRIBUTING.md). Security
reports follow [`SECURITY.md`](SECURITY.md).

## Licensing and project assets

The current repository licence is Apache-2.0. The specification calls for a
final licence and contributor-agreement review before a public release.
Signing keys, release credentials, and private vulnerability reports are never
committed to the repository.

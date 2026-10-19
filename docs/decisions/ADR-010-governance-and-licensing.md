# ADR-010: Governance and licensing

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Aether is a community-governed open-source project. The specification calls for
public decision records, a private security-reporting path, documented release
criteria, and a contributor-agreement or DCO decision before substantial
external contributions are accepted.

## Decision

Publish the project under an open-source governance model.

- The current repository licence is Apache-2.0. The technical specification
  requires a final licence and contributor-agreement review before a public
  release; this ADR records the current project default, not a legal conclusion.
- Architecture decisions are recorded as ADRs in `docs/decisions/`. Accepted
  ADRs are not silently rewritten; a new ADR supersedes them.
- Security reports use GitHub's private vulnerability reporting, as described
  in `SECURITY.md`.
- Mandatory security tests cannot be waived without a documented exception,
  owner, and expiry.
- Before accepting substantial external contributions, the maintainers will
  select and document either a Developer Certificate of Origin sign-off or a
  contributor licence agreement. Until then, contributions are accepted under
  the repository licence with the understanding recorded in `CONTRIBUTING.md`.
- Code, documentation, and release artifacts must carry compatible licences and
  attribution. Dependency and licence checks run in CI.

## Consequences

- Public decisions and known limitations are part of the project record.
- A licence or governance change requires an ADR and a migration note for
  contributors.
- Maintainers must keep the security-reporting path and release criteria
  current as the project grows.
- The project does not require a single commercial sponsor or a mandatory
  online account for users.

## Alternatives considered

- **Closed governance with public source:** rejected; it conflicts with the
  project's stated community-governed model.
- **Defer all governance until after 0.1:** rejected; contribution and security
  expectations need to exist before external code is accepted.
- **Adopt a copyleft licence without review:** not selected. The specification
  leaves the final licence open, so the project records Apache-2.0 as the
  current default and requires review before a public release.

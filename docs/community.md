# Community and contribution program

**Related:** [CONTRIBUTING.md](../CONTRIBUTING.md),
[GOVERNANCE.md](../GOVERNANCE.md), [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md)

Aether is a small, security-focused project. The contribution program is
designed to make useful work reviewable, not to maximise the number of open
issues. This page describes how to start, what good contributions look like, and
how review works.

## Ways to contribute

| Contribution | Where to start |
|---|---|
| Documentation and corrections | Fix a broken command or clarify a limitation in `docs/`; open a PR with the exact change. |
| Tests | Add a host test for parsing, arithmetic, or policy, or a boot-test assertion for a hardware path. |
| Reproducibility | Build the same commit twice and report differences, with toolchain and environment. |
| Compatibility records | Run the suite on a documented platform and propose one database record. |
| Small fixes | Comment on an issue with a reproduction before writing a large patch. |
| Security reports | Follow [`SECURITY.md`](../SECURITY.md); never open a public issue for an unfixed vulnerability. |

Windows runtime integration, GPU drivers, networking, and audio are large
projects that depend on interfaces that do not exist yet. Discuss an ADR first;
see the [feasibility report](specifications/windows-host-interface-report.md).

## Finding work

- Issues labelled `good first issue` are scoped to one file or one test and have
  an acceptance check.
- Issues labelled `help wanted` are accepted but unowned.
- The roadmap lists what is planned; a plan is not a commitment.

If you want to take on an unlabelled issue, comment first so two people do not
duplicate work.

## Contribution expectations

- One focused commit series per pull request; explain *why* in the body.
- Every `unsafe` block has a `SAFETY:` comment.
- Parsing and policy logic has host tests. Hardware and boot paths have a
  marker-based test.
- Public interfaces are documented; ABI changes are versioned.
- Run `make ci` locally and include the result and the tested commit in the PR.
  A boot claim includes the marker transcript.
- Do not commit keys, tokens, personal data, build outputs, or generated
  binaries. Test fixtures are the exception and are reviewed.
- A change that weakens a claimed security property needs a security review and
  must not be merged with a green test suite alone.

## Review process

1. A maintainer acknowledges the pull request and checks scope.
2. One reviewer is enough for documentation and host-test-only changes.
3. Two reviewers are required for the boot path, memory management, capability
   checks, package/update verification, or system ABI.
4. The author responds to review in the same branch; do not force-push a branch
   under review unless the change is your own unpublished commit.
5. A failed mandatory test blocks merge. A waiver requires a public exception in
   the PR and release notes.

## Contributor ladder

| Level | Evidence | What it unlocks |
|---|---|---|
| Contributor | One merged PR following the checklist | Listed in release notes if desired |
| Regular contributor | Sustained, well-reviewed changes in one area | Triaged issues in that area |
| Subsystem reviewer | Ownership of a component and a record of careful review | Review authority for that component |
| Maintainer | Consensus of existing maintainers, including security judgement | Merge rights and release participation |

Promotion is based on sustained work, not on a single large patch. Anyone may
decline a role at any time.

## Communication

- Issues for bugs, proposals, and scoped tasks.
- Pull requests for concrete changes.
- Private reporting for vulnerabilities.
- No private side channels for technical decisions; decisions that affect the
  architecture are recorded in `docs/decisions/`.

## Recognition

Release notes credit contributors who want to be credited. Test reports,
reproductions, and documentation fixes count as contributions.

## Licensing

Contributions are accepted under Apache-2.0. The final licence and contributor
agreement still require review before a public release; see
[GOVERNANCE.md](../GOVERNANCE.md).

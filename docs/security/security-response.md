# Security Response Process

**Related:** [SECURITY.md](../../SECURITY.md),
[release policy](release-policy.md), [threat model](threat-model.md)

Aether is a research prototype and has not received an external security review.
This process exists so reports are handled consistently and fixes are not
silently waived.

## Intake

- **Private report:** GitHub private vulnerability reporting for the repository.
  If unavailable, contact the maintainers privately.
- **Do not** open a public issue for an unfixed vulnerability.
- A report should include the affected commit, configuration, reproduction, the
  property that is broken, and any suggested mitigation.

## Roles

| Role | Responsibility |
|---|---|
| Security team | Intake, triage, fix coordination, advisory text |
| Kernel/security maintainer | Review of privileged or trust-boundary changes |
| Release manager | Blocks a release while a critical finding is open |
| Reporter | Available for follow-up and credit (optional) |

While the project is small, one person may hold several roles. A maintainer who
authored the affected code should not be the only reviewer of the fix.

## Timelines

These are targets, not guarantees:

| Severity | Acknowledge | Triage decision | Fix or mitigation target |
|---|---|---|---|
| Critical (boot integrity, isolation bypass, signing bypass) | 3 business days | 10 business days | 30 days |
| High (denial of service, information disclosure, policy bypass) | 5 business days | 15 business days | 60 days |
| Medium | 10 business days | 30 business days | next release |
| Low | Next triage batch | — | planned work |

Severity considers exploitability, required privileges, and whether the property
is claimed by documentation or tests. "Expected for a prototype" is not a reason
to close a report about a *claimed* guarantee.

## Embargo and disclosure

- Default embargo: 90 days from acknowledgement, or until a fix is available,
  whichever is earlier.
- The reporter and maintainers agree on a coordinated disclosure date.
- If a fix is not possible in time, publish a limitation and mitigation rather
  than staying silent.
- Credit the reporter unless they ask otherwise.
- A public advisory states affected versions, impact, mitigation, fixed version,
  and remaining limitations. It does not overstate assurance.

## Fixing

1. Reproduce on a documented commit.
2. Add a regression test that fails before the fix. Security-relevant tests use
   the `security.` prefix in the compatibility suite where applicable.
3. Keep the fix minimal; do not bundle unrelated changes.
4. Run `make ci` and, for release-critical fixes, `make qualify`.
5. Record the change in `CHANGELOG.md` without disclosing the vulnerability
   before the agreed date.

## Signing key compromise

If a release or package signing key is suspected compromised:

1. Stop publishing with that key and revoke trust in the next release.
2. Publish an advisory naming the key, the affected artifacts, and the
   replacement key.
3. Re-sign and re-verify a clean release bundle on an offline host.
4. Rotate the test fixtures if they were affected; test keys provide no
   security and are not treated as secrets.

## Postmortem

Within 14 days of resolution, write a short postmortem covering timeline, root
cause, detection gaps, the test that now prevents recurrence, and any process
change. Postmortems are public unless they disclose an unfixed issue.

## Out of scope

- Physical attacks and DMA (no IOMMU configured).
- Side channels and speculative execution.
- A compromised toolchain or host operating system.
- Issues in the QEMU/OVMF test platform unless Aether's handling amplifies them.
- Missing features listed in the known limitations, unless documentation claims
  they exist.

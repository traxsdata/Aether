# Release and Security Acceptance Policy

## Current status

There are no production releases. Aether 0.1 is a kernel and security research
prototype. Release notes must say so plainly.

## Mandatory gates

Every pull request runs the local gate (`make ci`), which covers:

1. Formatting check.
2. Clippy with warnings denied for host, kernel, and loader targets.
3. Documentation link and licence/dependency metadata checks.
4. Host unit tests.
5. Compatibility database and classification rules.
6. Package and update verification policy tests.
7. Kernel and loader cross-builds.
8. Automated QEMU boot smoke test with required success markers, including
   package, runtime, update verification, and the ring-3 IPC evidence gate.
9. Shell-driven console test.
10. Fault-path test requiring a structured exception report.
11. Rollback-path test requiring `AETHER_RECOVERY_ROLLBACK_OK`.
12. Negative boot test proving the harness rejects a deliberately failing
    kernel.
13. Signed release build, verification, and tamper acceptance test.

Security-critical changes additionally require review from a designated
kernel/security maintainer.

## Aether 0.1 acceptance

The prototype is complete only when it can:

1. Boot reproducibly on the supported virtual platform.
2. Run isolated user-space tasks.
3. Enforce capability-based access to kernel objects.
4. Provide bounded IPC between services.
5. Demonstrate memory and application isolation.
6. Access a basic test filesystem through user-space services.
7. Run a simple native application.
8. Reject unauthorized filesystem operations.
9. Execute its mandatory automated security tests.
10. Publish reproducible build instructions and known limitations.

The mandatory isolation tests (AT-002, AT-003, AT-005, AT-007, AT-008, AT-010)
block release on failure.

The working tree does not yet meet every mandatory test in its strongest form:
AT-002 is demonstrated by ring-0 workers rather than two ring-3 tasks, AT-008
service bodies still run in the kernel, and AT-010 has no dedicated user-fault
test. These gaps block a release claim; they do not block local prototype
development.

## Release requirements

- Reproducible build from a clean checkout with pinned tools.
- Checksums published for every artifact.
- Signed artifacts and a documented key-management process: `make release`
  refuses to produce an unsigned bundle, and `make release-check` verifies it.
- Known limitations and unsupported hardware listed
  ([supported hardware](../reference/supported-hardware.md)).
- Recovery path tested after a deliberately failed update; update replay,
  downgrade, and unknown publishers are rejected by the boot test.
- A completed `make qualify` report with commit, toolchain, emulator, step
  results, boot markers, and artifact digests.
- No unresolved critical or high-severity security findings without a
  documented exception and mitigation.

## Waivers

A mandatory test may be waived only by a documented exception that states the
reason, the compensating control, the owner, and the expiry. Waivers are public
unless they would disclose an unfixed vulnerability.

## Qualification

`make qualify` runs the full gate plus a signed test-key release build and
verification, then writes `build/qualification/report.md`. A release candidate
is not published from a failing report. Qualification is automated evidence; it
does not replace the external security review, which remains outstanding.

## Vulnerability handling

Follow [SECURITY.md](../../SECURITY.md) and the
[security response process](security-response.md). Fixes for embargoed issues
may be developed privately and merged with a coordinated disclosure date.

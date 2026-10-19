# Release and Security Acceptance Policy

## Current status

There are no production releases. Aether 0.1 is a kernel and security research
prototype. Release notes must say so plainly.

## Mandatory gates

Every pull request runs:

1. Formatting check.
2. Clippy with warnings denied for host, kernel, and loader targets.
3. Host unit tests.
4. Kernel and loader cross-builds.
5. Automated QEMU boot smoke test.
6. A negative boot-test self-check, proving CI detects a missing marker.

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

## Release requirements

- Reproducible build from a clean checkout with pinned tools.
- Checksums published for every artifact.
- Signed artifacts and a documented key-management process.
- Known limitations and unsupported hardware listed.
- Recovery path tested after a deliberately failed update.
- No unresolved critical or high-severity security findings without a
  documented exception and mitigation.

## Waivers

A mandatory test may be waived only by a documented exception that states the
reason, the compensating control, the owner, and the expiry. Waivers are public
unless they would disclose an unfixed vulnerability.

## Vulnerability handling

Follow [SECURITY.md](../../SECURITY.md). Fixes for embargoed issues may be
developed privately and merged with a coordinated disclosure date.

# Security policy

Aether is an early research prototype. It has not received an external security
review and must not be used to protect real data.

## Reporting a vulnerability

Please do not open a public issue for an unfixed vulnerability. Use GitHub's
private vulnerability reporting for this repository
(`Security` → `Report a vulnerability`). If that is unavailable, contact the
maintainers privately through the repository owner.

Include:

- the affected commit or branch,
- the build/test configuration,
- a minimal reproduction,
- the security property you believe is broken, and
- any suggested fix or mitigation.

Do not include passwords, private keys, or personal data in a report.

## Scope

The prototype's in-scope trust boundaries are currently:

- the UEFI loader's parsing of the embedded kernel image and firmware memory
  map,
- the loader-to-kernel `BootInfo` hand-off,
- the kernel's validation of that hand-off and memory region ordering,
- kernel-task and user/kernel address-space isolation,
- kernel capability and IPC validation,
- the syscall gate's pointer, length, handle, and generation checks,
- per-process roots and portal grants,
- package verification (framing, manifest policy, publisher pin, signature,
  payload digest, archive structure) and installation, and
- update verification (signature, publisher pin, image digest, rollback
  counter, version, and channel policy).

Missing features are not vulnerabilities by themselves, but a report that a
*claimed* boundary does not hold is in scope: for example an application reading
a sibling root, a package bypassing a verification step, a replayed update being
accepted, or a package installing a file outside its derived root. Memory
unsafety in the implemented boot, memory, scheduler, capability, IPC,
filesystem, or verification paths is in scope.

Windows compatibility, networking, audio, persistent storage, Secure Boot, and
the UEFI installer are not implemented; reports about their absence are tracked
as planned work in the [roadmap](docs/roadmap.md).

## Response

Maintainers will acknowledge a report, reproduce it, and agree on a disclosure
timeline. The detailed process, severity targets, embargo, key-compromise
handling, and advisory requirements are in the
[security response process](docs/security/security-response.md). Fixes for
mandatory isolation, boot-integrity, package-verification, or
update-verification failures block a release. Release notes must state known
limitations; a passing test suite is not proof of security.

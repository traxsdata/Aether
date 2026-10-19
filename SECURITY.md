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
- kernel-task address-space isolation, and
- kernel-internal capability and IPC validation.

There is no user/kernel isolation or user-mode syscall boundary, and capability
enforcement is not yet a user-visible security boundary, so reports about
missing application sandboxing are expected and tracked as planned work rather
than vulnerabilities. Reports about a *claim* of isolation that does not hold,
or about memory unsafety in the implemented boot, memory, scheduler,
capability, or IPC paths, are in scope.

## Response

Maintainers will acknowledge a report, reproduce it, and agree on a disclosure
timeline. Fixes for mandatory isolation or boot-integrity failures block a
release. Release notes must state known limitations; a passing test suite is not
proof of security.

# ADR-007: Windows compatibility

- **Status:** Accepted as a compatibility direction; runtime not implemented
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Aether aims to run useful Windows applications without making Windows
compatibility part of the privileged kernel or granting a compatibility runtime
unrestricted host access. Wine and Proton are existing user-space compatibility
technologies, but they depend on host interfaces, graphics, libraries, and
process semantics that Aether must supply.

## Decision

Use Wine/Proton as user-space compatibility technologies, with optional virtual
machines as a separate execution mode.

- The Windows runtime manager and each compatibility environment are user-space
  components with a documented capability profile.
- Each Windows application receives a distinct compatibility environment by
  default. Sharing a prefix requires an explicit user decision and a warning
  that applications in that prefix may share files and registry data.
- A Wine prefix is not treated as a security sandbox. The Aether security domain,
  filesystem service, network policy, and portal enforce isolation independently.
- File import/export goes through the file portal or scoped grants. The runtime
  does not receive ambient access to the user's home directory.
- Network and device access are denied by default and granted explicitly.
- VM support is optional, evaluated after native memory, scheduling, interrupt,
  and device-resource isolation are stable. A VM is not described as perfectly
  isolated.

Full compatibility is not promised until the feasibility gate in
[`../specifications/windows-compatibility.md`](../specifications/windows-compatibility.md)
has mapped the selected Wine/Proton host requirements to tested Aether
interfaces.

## Consequences

- Compatibility work is gated on a documented host-interface study, not on a
  marketing claim.
- The runtime manager is not a security authority and cannot mint capabilities.
- Compatibility testing must record the application version, runtime version,
  GPU/driver configuration, and test date.
- Some applications may remain unsupported because they require authority that
  cannot be safely mediated.

## Alternatives considered

- **Implement Windows compatibility in the kernel:** rejected; it would enlarge
  the trusted computing base and violate the microkernel boundary.
- **Run Wine on a POSIX compatibility layer with broad host access:** rejected;
  it would bypass Aether's capability model.
- **Only support Windows in a full VM:** kept as an optional fallback, but it is
  heavier and does not replace a native compatibility path.

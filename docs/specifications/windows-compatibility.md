# Windows Compatibility Feasibility Plan

**Document ID:** AETHER-WIN-001  
**Status:** Planning only; no Windows runtime exists. Gate 1 is reviewed and not
met; see the [host-interface report](windows-host-interface-report.md).  
**Related:** ADR-007, technical specification §7

## Purpose

This plan defines how Aether will decide whether Wine/Proton can run in isolated
user-space compatibility environments. It is a feasibility plan, not a
compatibility claim. Aether will not advertise Windows support until the
required host interfaces have been mapped to implemented Aether interfaces and
tested against a documented application set.

## Non-claims

- The kernel does not contain Windows, Wine, Proton, or a hypervisor.
- A Wine prefix is not a security boundary.
- Windows applications do not receive ambient access to Aether user files,
  devices, or the network.
- Full Windows compatibility is not required for Aether 0.1.

## Candidate runtime

The first study targets a pinned Wine or Proton release selected for a concrete
application set. The study must record the exact runtime version and build
configuration. Proton-specific graphics and audio paths may be deferred if they
depend on host facilities Aether does not yet provide.

## Required host facilities

| Facility | What the runtime needs | Aether interface to define | Current status |
|---|---|---|---|
| Process and thread creation | Create/terminate processes and threads, join/wait, priorities | User-space task/process API over the kernel task model | Not implemented |
| Virtual memory | Reserve/commit/decommit, protect, map files and shared memory | Memory-object capabilities and address-space syscalls | Not implemented |
| Synchronization | Mutexes, events, semaphores, wait lists, timeouts | User-space synchronization service or kernel primitives | Not implemented |
| Filesystem semantics | Open/read/write/seek/rename/delete, metadata, locks | `fsd` file handles and `portald` grants | Not implemented |
| Dynamic loading | Map PE/ELF images, resolve imports, TLS, relocations | Loader/runtime library service and memory mapping | Not implemented |
| Exceptions and signals | Trap delivery, unwind metadata, debug events | Fault delivery to a supervisor; process control | Partial (kernel reports faults, no user delivery) |
| Networking | Sockets, name resolution, connection state | `netd` capability-checked socket API | Not implemented |
| Audio | Device enumeration, streams, timing | `audiod` capability profile | Not implemented |
| Windowing and graphics | Window surfaces, input focus, clipboard, drag and drop | `composerd` graphics/input protocol and portal | Not implemented |
| Timers | High-resolution waits, periodic events | Timer service and IPC timeouts | Partial (kernel PIT tick only) |
| Devices | Controllers, HID, removable media, GPU | `devd` resource capabilities | Not implemented |
| Tooling | Compiler, linker, PE loader, debugger, crash dumps | SDK, runtime manager, diagnostic service | Not implemented |

The table is deliberately conservative: a facility is "implemented" only when a
tested Aether interface exists and the runtime uses it, not when the kernel has
an internal primitive that resembles it.

## Isolation model

1. A user-space runtime manager starts each application in its own compatibility
   environment and capability profile.
2. Each environment has a private prefix, registry, and application files.
3. The environment receives only the endpoints and file handles needed for the
   requested operation.
4. File import/export goes through the file portal or a scoped grant. The
   environment cannot enumerate user directories by default.
5. Network and device access are denied unless explicitly granted.
6. Clipboard and drag-and-drop sharing are explicit; they are not automatic
   consequences of running a window.
7. A crash terminates the environment, not the desktop or unrelated
   applications.

## Feasibility gates

### Gate 1 — Host-interface inventory

Produce a reviewed report listing every host facility the selected Wine/Proton
version needs, including process/thread semantics, memory mapping flags,
filesystem behaviour, exception delivery, graphics, audio, networking, and
device access.

**Evidence:** report checked into `docs/`, source references to the pinned
runtime version, and a gap list with no unresolved critical items.
[Current report](windows-host-interface-report.md) — gap list has blocking
items, so the gate is **not met**.

### Gate 2 — Interface mapping

Map every required facility to an Aether syscall, service protocol, or explicit
non-goal. Identify semantics that Aether cannot provide without weakening the
capability model.

**Evidence:** a traceability table from runtime requirement to Aether interface,
with owners and test cases.

### Gate 3 — Isolated prototype

Run a minimal Windows console or windowed application in a per-application
environment on the supported QEMU target. The environment must fail closed when
a capability is missing.

**Evidence:** boot transcript, application version, runtime version, and tests
showing denial of ungranted file, network, and device access.

### Gate 4 — Compatibility classification

Test a documented application set and assign each result one of:

- **Verified:** reproducibly passes the Aether test suite.
- **Compatible:** works with documented limitations.
- **Limited:** some functionality fails or requires a workaround.
- **Unsupported:** known incompatibility or unmet security requirement.

The database must record application version, runtime version, hardware or
virtual platform, and test date. The format and classification rules are
implemented in [`compatibility.md`](compatibility.md).

## Dependencies and sequencing

The compatibility work depends on:

1. User-mode execution, syscall validation, and per-process address spaces.
2. File handles and the file portal (Epic F).
3. Service supervision and IPC timeouts/blocking (Epic D/E).
4. Graphics, input, and audio services (Epic G).
5. Network and device capability policy (Epic F).
6. Update/recovery if compatibility runtimes are distributed as packages
   (Epic I).

Research can begin in parallel, but integration must not bypass these
dependencies.

## Security acceptance criteria

The feasibility work is not complete unless:

- The runtime manager cannot mint capabilities or impersonate a trusted service.
- A compromised compatibility environment cannot read another application's
  private files or the host system image.
- A selected-file grant does not expose parent directories or unrelated files.
- Revocation semantics are defined for open handles and mapped files.
- Network and device denial is enforced by a service or kernel boundary, not by
  the compatibility runtime's configuration alone.
- Crash containment and resource quotas are tested.

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Runtime assumes Linux/POSIX behaviour Aether does not provide | Critical | Complete Gate 1 before committing to a release target |
| Graphics/audio requirements exceed the initial service model | High | Start with console and simple window surfaces; defer accelerated paths |
| Compatibility environment becomes overprivileged | Critical | Per-application capability profile; no ambient file or device access |
| Wine prefix is mistaken for a sandbox | High | Enforce isolation in Aether services; document the distinction |
| Application-specific workarounds undermine isolation | High | Require security review for every new host capability |

## References

- Wine documentation: <https://www.winehq.org/documentation>
- Proton source and release notes: <https://github.com/ValveSoftware/Proton>
- Technical specification §7 (Windows compatibility)

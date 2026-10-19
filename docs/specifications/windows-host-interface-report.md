# Windows Host-Interface Feasibility Report

**Document ID:** AETHER-WIN-002
**Status:** Gate 1 review; no compatibility runtime exists
**Related:** ADR-007, [feasibility plan](windows-compatibility.md), AET-070 and
AET-071

## Purpose and method

The feasibility plan requires an inventory of the host facilities a Wine/Proton
runtime needs before any compatibility work is scheduled. This report maps the
required facilities to Aether interfaces that exist today, cites the test
evidence, and identifies the blocking gaps. It is a planning artifact, not a
compatibility claim.

The review is based on the interfaces in the technical specification §7, the
system ABI, and the current kernel tree. No Windows runtime was built or run,
because the facilities below do not exist yet.

## Result

**Gate 1 is not satisfied.** Aether cannot host a meaningful Wine/Proton runtime
today. The blocker is not the capability model; it is that the user-space system
interface is still a single-process bring-up surface. The compatibility work
should remain a research track until the prerequisites below land.

This is the expected outcome for a prototype at this stage, and it is recorded
rather than hidden so later claims have a baseline.

## Facility inventory

| Facility | Runtime requirement | Aether interface today | Status |
|---|---|---|---|
| Process and thread creation | Create, join, wait, terminate, priorities | One embedded ring-3 program; no `process.create` syscall | **Missing** |
| Virtual memory | Reserve/commit/decommit, protect, map files, shared memory | Per-process PML4, fixed user window, no memory-object syscalls | **Missing API** |
| Synchronization | Mutexes, events, semaphores, waits, timeouts | Polled IPC; no blocking wait or timeout | **Missing** |
| Filesystem semantics | Open/read/write/seek/rename/delete, metadata, locks | Per-process roots, generation-checked handles, `open/read/write/close/list/stat`; no seek, rename, or locks | **Partial** |
| Dynamic loading | Map PE/ELF, resolve imports, TLS, relocations | Loader embeds one static ELF; no user-space loader | **Missing** |
| Exceptions and signals | Trap delivery, unwind metadata, debug events | Fault retires the task and prints diagnostics; no delivery to a supervisor | **Partial** |
| Networking | Sockets, name resolution | None | **Missing** |
| Audio | Device enumeration, streams, timing | None | **Missing** |
| Windowing and graphics | Surfaces, focus, clipboard, drag and drop | Kernel software compositor and six windows; no application surface protocol | **Prototype only** |
| Input | Keyboard, pointer, focus events | PS/2 keyboard queue and focus movement | **Partial, no application API** |
| Timers | High-resolution waits, periodic events | 100 Hz PIT tick | **Partial** |
| Devices | Controllers, HID, removable media, GPU | No device service or IOMMU | **Missing** |
| Tooling | Compiler, linker, PE loader, debugger, crash dumps | Rust toolchain and SDK for native programs; no PE tooling | **Missing** |

## Security findings

The capability model is compatible with the isolation requirements:

- Packages are signature-verified and declare an allowlist of permissions
  ([package format](package-format.md)).
- Runtime profiles derive the storage root from application identity and deny
  network and device access by construction
  ([runtime policy](../../kernel/services/src/runtime.rs)).
- The portal grants one exact file, not a directory or pattern.

However, isolation is only credible once the *runtime manager* exists as its own
trusted service with a narrow profile. A runtime that shares prefixes, maps
memory across applications, or hands out raw device descriptors would defeat the
package policy.

## Minimum first compatibility target

If the prerequisites land, the first target should be a console application
from a pinned Wine release, with:

- one per-application prefix created by the runtime manager;
- no network, audio, or device access;
- file import/export only through explicit portal grants;
- no shared clipboard;
- a crash contained to the compatibility environment.

Windowed applications, Direct3D, and Vulkan are later work and should not be
promised before the host interfaces exist.

## Prerequisites before Gate 2

1. User-space process creation, memory objects, and blocking waits with timeouts.
2. A user-space ELF/PE loader and a runtime manager service.
3. A network service and an audio service with capability policy.
4. An application surface protocol over the compositor.
5. Persistent storage, so a prefix can survive a reboot.
6. Resource accounting to enforce memory, file, and task limits.

## Conclusion

Aether's security architecture does not block Wine/Proton, but its user-space
surface is far smaller than the runtime expects. Compatibility remains a
research track behind the native platform. The compatibility database records
the current result as `unsupported` with this reason, and the package verifier
refuses `runtime=windows` rather than implying an environment that does not
exist.

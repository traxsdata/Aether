# Architecture overview

Aether is layered so that the privileged core stays small and security policy
is enforced at explicit boundaries.

```text
Packages and signed updates               verified and installed by the kernel
        |
Native application / SDK                  implemented (ring 3)
        |
Desktop, file manager, Security Center   implemented (window manager + compositor)
        |
Services                                  implemented as supervised kernel modules
  storaged / fsd / appd / inputd / displayd / desktopd / portald / securityd
        |
Capability-oriented kernel prototype     boot, memory, exceptions, scheduler,
                                          syscalls, capabilities, IPC, timer,
                                          package/update verification
        |
Hardware and UEFI firmware
```

The current desktop and services execute in the kernel prototype. The user
application executes at CPL3; its syscalls, capability table, address space, and
file handles are enforced by the kernel. Moving the service bodies into
separate user processes is follow-up work; the capability profiles and
supervision rules are already declared.

## Trust boundaries

1. **Firmware to loader.** UEFI firmware supplies tables, memory, GOP, and the
   boot image. The loader treats the embedded kernel image as untrusted input
   and validates it before copying.
2. **Loader to kernel.** The loader is part of the trusted boot chain. The
   kernel still validates the versioned `BootInfo` block, memory map, and
   framebuffer descriptor. A mismatch fails closed.
3. **Kernel to user space.** Enforced by CPL3, per-task PML4 user mappings, a
   TSS-backed kernel stack, an `int 0x80` gate, and syscall pointer/handle
   validation. A user fault terminates the task, not the kernel.
4. **Service to service.** Services have declared capability profiles and the
   supervisor applies dependency, restart, and quarantine rules. The kernel
   still hosts the service implementations in this prototype.
5. **Application to user data.** Enforced by per-process roots and file
   capabilities. Outside files are reachable only through an exact-path portal
   grant. Ordinary ring-3 applications cannot mint authority.
6. **Desktop to policy.** The compositor displays state and requests portal
   actions through filesystem, supervisor, and portal adapters. This is a coding
   convention, not an enforced boundary: the current ring-0 compositor is trusted
   kernel code. A compromise there would be kernel compromise. The specification's
   user-space compositor/service boundary remains follow-up work; see
   [ADR-016](../decisions/ADR-016-desktop-review-and-uefi-correctness.md).
7. **Publisher to system.** A package is installed only after manifest policy,
   publisher pin, Ed25519 signature, and payload digest verification. A signed
   update is activated only when the manifest, image digest, rollback counter,
   version, and channel policy agree. A valid signature is not a safety claim.

## Current implementation

- `abi/` defines the loader/kernel contract, framebuffer hand-off, and user
  syscall ABI.
- `boot/common/` parses and validates the kernel ELF and contains host-tested
  memory arithmetic.
- `boot/loader/` is a UEFI application that embeds the kernel and queries GOP.
- `kernel/fs/` is a host-tested virtual block device and filesystem.
- `kernel/memory/` contains the host-tested frame allocator and page-table
  encoding.
- `kernel/ipc/` contains host-tested capability tables and endpoint queues.
- `kernel/input/` contains pure input decoders, modifiers, and bounded FIFOs;
  `kernel/wm/` contains window/layout/damage and browser presentation models.
- `kernel/services/` contains the host-tested supervisor, application
  lifecycle rules, and runtime profiles.
- `crypto/` contains dependency-free SHA-256, SHA-512, and Ed25519 used by
  package and update verification.
- `packaging/` and `updates/` contain the signed package format, runtime
  manifest policy, update manifest, and activation policy.
- `tests/compatibility/` contains the compatibility database and classification
  rules; `tests/fixtures/` contains signed test artifacts.
- `kernel/` contains the entry point, GDT/TSS/IDT, CPU NX/WP setup, serial,
  hand-off validation, physical memory, kernel and user page tables, timer,
  scheduler, syscall gate, filesystem service, package installation, runtime
  profiles, update verification, portal, shell, graphics, window manager,
  compositor, PS/2 keyboard and mouse, and recovery logic.
- `sdk/` and `user/` contain the native application SDK and the ring-3 init
  program.

## Design rules

- Kernel mechanisms, not policy. File access, networking policy, and permission
  UI belong to isolated services.
- Every kernel object is reached through an unforgeable capability with explicit
  rights.
- The desktop may request an action but cannot grant authority by itself.
- A missing capability means denial, not a fallback or an implicit elevation.
- Resource limits apply to tasks, IPC, files, and memory; malformed input is
  rejected with bounded work.
- Security claims require tests and documented limitations.

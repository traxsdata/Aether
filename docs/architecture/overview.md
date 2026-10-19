# Architecture overview

Aether is layered so that the privileged core stays small and security policy
is enforced at explicit boundaries.

```text
Native application / SDK                  implemented (ring 3)
        |
Desktop, file manager, Security Center   implemented (software compositor)
        |
Services                                  implemented as supervised kernel modules
  storaged / fsd / appd / inputd / displayd / desktopd / portald / securityd
        |
Capability-based microkernel              boot, memory, exceptions, scheduler,
                                          syscalls, capabilities, IPC, timer
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
   grant; the desktop cannot mint authority.
6. **Desktop to policy.** The compositor displays state and requests portal
   actions. The filesystem, supervisor, and portal services make the decisions.

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
- `kernel/services/` contains the host-tested supervisor and application
  lifecycle rules.
- `kernel/` contains the entry point, GDT/TSS/IDT, CPU NX/WP setup, serial,
  hand-off validation, physical memory, kernel and user page tables, timer,
  scheduler, syscall gate, filesystem service, portal, shell, graphics,
  compositor, input, and recovery logic.
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

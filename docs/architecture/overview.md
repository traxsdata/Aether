# Architecture overview

Aether is layered so that the privileged core stays small and security policy
is enforced at explicit boundaries.

```text
Applications and runtimes            (not implemented)
        |
Desktop and native SDK               (not implemented)
        |
Essential user-space services        (not implemented)
  initd / devd / storaged / fsd / netd / securityd / portald / sessiond
        |
Capability-based microkernel         (boot, memory, exceptions, scheduler)
  memory / tasks / IPC / capabilities / interrupts / timer
        |
Hardware and UEFI firmware
```

## Trust boundaries

1. **Firmware to loader.** UEFI firmware supplies tables, memory, and the boot
   image. The loader treats the embedded kernel image as untrusted input and
   validates it before copying.
2. **Loader to kernel.** The loader is part of the trusted boot chain. The
   kernel still validates the versioned `BootInfo` block and its memory map.
   A mismatch fails closed.
3. **Kernel to user space.** Not implemented. When it exists, the kernel will
   enforce address-space separation and capability rights in hardware.
4. **Service to service.** Not implemented. Future IPC must bound messages,
   validate transferred capabilities, and prevent impersonation.
5. **Application to user data.** Not implemented. Files will be reached through
   capability-bearing handles and a user-mediated portal, not global paths.

## Current implementation

The tree implements the lowest layer needed to prove the boot and memory model:

- `abi/` defines the loader/kernel contract.
- `boot/common/` parses and validates the kernel ELF and contains host-tested
  memory arithmetic.
- `boot/loader/` is a UEFI application.
- `kernel/memory/` contains the host-tested frame allocator and page-table
  encoding.
- `kernel/` contains the entry point, GDT/IDT, serial output, hand-off
  validation, physical-memory initialization, kernel page tables, timer, and
  preemptive scheduler with per-task address spaces.

There is no general-purpose filesystem, network stack, package manager, GUI
toolkit, or permission dialog in the kernel. That remains a design rule, not
just an accident of the current snapshot.

## Design rules

- Kernel mechanisms, not policy. File access, networking policy, and permission
  UI belong to isolated services.
- Every kernel object is reached through an unforgeable capability with explicit
  rights.
- The desktop may request an action but cannot grant authority by itself.
- A missing capability means denial, not a fallback or an implicit elevation.
- Resource limits apply to tasks, IPC, and memory; malformed input is rejected
  with bounded work.
- Security claims require tests and documented limitations.

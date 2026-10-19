# Kernel

## Scope

The kernel is the only privileged component. The specification limits it to
mechanisms that require hardware privilege:

- CPU and thread scheduling;
- address spaces and memory protection;
- IPC primitives;
- capability tables and object lifetime;
- interrupts and exceptions;
- timers;
- controlled hardware-resource mapping;
- task lifecycle and fault reporting.

It must not grow a general-purpose filesystem, TCP/IP stack, package manager,
GUI toolkit, or permission dialog. Those belong in isolated user-space
services.

## Current state

The kernel is a first-sprint skeleton:

- x86-64 long mode, ring 0, interrupts disabled at entry;
- a dedicated 64 KiB stack in `.bss`;
- COM1 serial output;
- panic reporting;
- `BootInfo` header, kernel-range, and memory-region validation;
- a memory summary (region count and usable bytes);
- an explicit `AETHER_BOOT_OK` marker for the boot test.

What is deliberately absent:

- IDT, exception handlers, and a structured fault report;
- physical page allocator and page tables owned by the kernel;
- GDT/TSS ownership and user-mode entry;
- task/thread objects, context switching, and scheduling;
- timer and interrupt controllers;
- capability objects, tables, rights checks, and IPC;
- SMP startup.

These gaps are tracked in the technical specification backlog (Epics B–D).

## Entry contract

`_start` is the ELF entry point. It:

1. executes `cli`;
2. sets `rsp` to `__stack_top` from the linker script region (a 64 KiB stack
   in `.bss`);
3. clears `rbp`;
4. calls `kernel_main(boot_info)` with the pointer already in `RDI`;
5. executes `ud2` if the entry function ever returns.

`kernel_main` is `unsafe extern "sysv64"` because it trusts only the calling
convention, not the pointer. It validates the `BootInfo` header before reading
any pointed-to memory.

## Next steps

The critical path from the specification is:

1. IDT, exception reporting, and a deliberate panic/abort path.
2. Physical page allocator over the validated memory map.
3. Kernel-owned page tables and a higher-half mapping policy.
4. Task/address-space objects and user/kernel separation.
5. Timer interrupts, preemptive scheduling, and fault containment.
6. Capability objects, tables, rights checks, IPC endpoints, and transfer.
7. User-space loader and supervisor.

Each step needs tests that fail closed and an ADR if it changes a trust
boundary.

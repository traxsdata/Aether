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

The kernel implements the Phase 1 boot and memory slice:

- x86-64 long mode, ring 0, interrupts disabled at entry;
- a dedicated 64 KiB stack in `.bss`;
- a flat kernel GDT and a 32-entry architectural exception IDT;
- COM1 serial output and panic reporting;
- `BootInfo` header, kernel-range, and memory-region validation;
- a 16 GiB bitmap frame allocator over validated RAM;
- kernel-owned four-level page tables with a 4 KiB W^X kernel mapping and 2 MiB
  identity mappings for the rest of RAM;
- structured exception reports with vector, error code, RIP, RFLAGS, and CR2;
- explicit `AETHER_MM_OK` and `AETHER_BOOT_OK` markers for the boot test;
- a `fault-selftest` feature that executes `int3` after paging for the
  exception-path test.

What is deliberately absent:

- a higher-half mapping and per-address-space page tables;
- IST/TSS setup for double-fault and stack-overflow recovery;
- user-mode entry and user/kernel page permissions;
- task/thread objects, context switching, and scheduling;
- timer and interrupt controllers;
- capability objects, tables, rights checks, and IPC;
- SMP startup.

These gaps are tracked in the technical specification backlog (Epics C–D).

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

1. Higher-half kernel mapping and per-address-space page tables.
2. IST/TSS for double-fault and stack-overflow diagnostics.
3. Interrupt controller and timer initialization.
4. Task/address-space objects and user/kernel separation.
5. Preemptive scheduling and fault containment.
6. Capability objects, tables, rights checks, IPC endpoints, and transfer.
7. User-space loader and supervisor.

Each step needs tests that fail closed and an ADR if it changes a trust
boundary.

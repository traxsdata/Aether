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

The kernel implements the Phase 1 boot/memory slice and the first Phase 2
scheduling slice:

- x86-64 long mode, ring 0, interrupts enabled after scheduler setup;
- a dedicated 64 KiB boot stack and 16 KiB per-task stacks;
- a flat kernel GDT and a 32-entry architectural exception IDT;
- COM1 serial output and panic reporting;
- `BootInfo` header, kernel-range, and memory-region validation;
- a 16 GiB bitmap frame allocator over validated RAM;
- kernel-owned four-level page tables with a 4 KiB W^X kernel mapping and 2 MiB
  identity mappings for the rest of RAM;
- structured exception reports with vector, error code, RIP, RFLAGS, and CR2;
- a 100 Hz PIT/PIC timer and preemptive round-robin scheduler;
- per-task PML4 address spaces and CR3 switching;
- per-task capability tables, bounded IPC endpoints, and capability transfer;
- a boot-time isolation and capability/IPC test across three kernel tasks;
- explicit `AETHER_MM_OK`, `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`,
  `AETHER_CAP_OK`, `AETHER_IPC_OK`, and `AETHER_BOOT_OK` markers.

What is deliberately absent:

- user mode, TSS/RSP0, syscall entry, and user/supervisor page permissions;
- a kernel heap and dynamic task creation;
- priorities, sleeping/wakeup, blocking IPC, SMP, and per-CPU scheduler state;
- capability revocation and broader object types;
- higher-half kernel mapping and guard pages;
- local APIC/high-resolution timers.

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

1. User-mode entry: TSS/RSP0, user page permissions, syscall gate.
2. Per-task kernel stacks and guard pages.
3. Higher-half kernel mapping and per-address-space user regions.
4. Capability objects, tables, rights checks, IPC endpoints, and transfer.
5. User-space loader, `initd` supervision, and the first native task.
6. Adversarial isolation, capability, and IPC tests.

Each step needs tests that fail closed and an ADR if it changes a trust
boundary.

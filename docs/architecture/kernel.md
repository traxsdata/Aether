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

The kernel implements the Phase 1 through Phase 4 prototype slices:

- x86-64 long mode, ring 0/3, interrupts enabled after scheduler setup;
- explicit `IA32_EFER.NXE` and `CR0.WP` setup, with boot failure if NX is
  unsupported;
- a dedicated 64 KiB boot stack, 16 KiB worker stacks, and 32 KiB per-task
  kernel stacks used through TSS `RSP0`;
- a flat kernel GDT with ring-3 code/data descriptors, a 64-bit TSS, and a
  32-entry architectural exception IDT plus timer, keyboard, and `int 0x80`
  gates;
- COM1 serial output, keyboard IRQ1 input, and panic reporting;
- `BootInfo` header, kernel-range, memory-region, and framebuffer validation;
- a 16 GiB bitmap frame allocator over validated RAM;
- kernel-owned four-level page tables with a 4 KiB W^X kernel mapping,
  2 MiB/4 KiB identity mappings for kernel-accessible RAM, and an explicit
  write-combining framebuffer mapping (cache-disabled without PAT);
- user PML4s with only the user window marked user-accessible, and a TSS-backed
  privilege transition;
- structured exception reports; ring-3 faults retire the task and reschedule;
- a 100 Hz PIT/PIC timer and preemptive round-robin scheduler;
- per-task capability tables, endpoint identity checks, bounded IPC endpoints,
  atomic receives, capability transfer, and file capabilities;
- validated syscalls for console output, process identity, yield, file
  open/read/write/close/list/stat, portal open, and diagnostics;
- a boot-time isolation, capability/IPC, ring-3 sandbox, filesystem,
  supervisor, recovery, graphics, desktop, input, portal, and shell test;
- a volatile RAM block device and bounded filesystem service.

What is deliberately absent:

- a kernel heap and dynamic task creation;
- priorities, sleeping/wakeup, blocking IPC, SMP, and per-CPU scheduler state;
- general capability revocation and interactive grant management;
- higher-half kernel mapping and guard pages;
- local APIC/high-resolution timers;
- persistent block storage, networking, audio, and GPU acceleration.

These gaps are tracked in the technical specification backlog.

## Entry contract

`_start` is the ELF entry point. It:

1. executes `cli`;
2. sets `rsp` to `__stack_top` from the linker script region (a 64 KiB stack
   in `.bss`);
3. clears `rbp`;
4. calls `kernel_main(boot_info)` with the pointer already in `RDI`;
5. halts in a `cli; hlt` loop if the entry function ever returns.

`kernel_main` is `unsafe extern "sysv64"` because it trusts only the calling
convention, not the pointer. It validates the `BootInfo` header before reading
any pointed-to memory.

## Next steps

The critical path from the specification is:

1. Move service bodies out of the kernel into supervised ring-3 processes.
2. Higher-half kernel mapping, guard pages, and a kernel heap.
3. Capability revocation, object destruction, and stale-reference behavior for
   every object type, including live file grants.
4. Persistent block storage and crash-consistent filesystem updates.
5. Interactive portal consent and persistent, auditable grant storage.
6. Blocking IPC, timeouts, cancellation, and broader adversarial tests.
7. SMP, per-CPU scheduler state, and local APIC timers.

Each step needs tests that fail closed and an ADR if it changes a trust
boundary.

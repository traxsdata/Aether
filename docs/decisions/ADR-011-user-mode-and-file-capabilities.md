# ADR-011: User mode, syscalls, and file capabilities

- Status: accepted
- Date: 2026-10-19

## Context

Phase 2 already had per-task page tables, capability tables, and bounded IPC,
but every task ran at CPL0. Phase 3 requires a native application that cannot
reach ungranted resources, so the privilege boundary had to become real rather
than simulated by cooperative kernel tasks.

## Decision

1. Run the first native process at CPL3 in a private PML4. Only the user window
   is mapped with the user bit; the kernel identity map is copied
   supervisor-only.
2. Add ring-3 code/data GDT descriptors and a 64-bit TSS. The scheduler updates
   `RSP0` to a per-task kernel stack on every switch.
3. Enter the kernel through interrupt vector `0x80` with a DPL-3 gate. Validate
   every user pointer, length, handle, generation, object type, and right
   before copying or acting.
4. Represent open files as `File` capabilities in the existing per-task table.
   The userspace handle encodes only slot index and generation.
5. Give each process a kernel-owned root. Reject `..` in path resolution and
   map absolute-looking paths relative to that root.
6. Retire a faulting ring-3 task and reschedule; kernel faults remain fatal.

## Consequences

- The isolation test is now a CPU-enforced boundary and a sandbox-escape test
  is meaningful.
- The kernel owns a TSS and per-task kernel stacks; both must be kept in sync
  with scheduler state.
- User pointer validation has a per-syscall cost, but no copy occurs before the
  whole range is checked.
- Service bodies still run in the kernel. Moving them to ring 3 is a follow-up
  that reuses this ABI rather than changing it.
- The ABI is versioned but not yet frozen for third-party binaries.

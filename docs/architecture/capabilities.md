# Capability Model

## Definition

A capability is an unforgeable reference to a kernel object or controlled
resource, carrying a set of rights. An integer identifier, pathname, PID, or
user-supplied name is never sufficient authority.

## Rights

| Right | Meaning |
|---|---|
| `READ` | Observe object contents or state |
| `WRITE` | Modify object contents or state |
| `EXECUTE` | Use an object as executable code |
| `MAP` | Map a memory object into an address space |
| `TRANSFER` | Pass the capability through authorized IPC |
| `DERIVE` | Create a capability with a subset of rights |
| `ADMINISTER` | Perform control operations defined per object type |

Rights are object-type-specific. An operation is permitted only if the
capability is valid, alive, of the expected type, and carries every required
right.

## Rules

1. No capability means no authority.
2. Ordinary applications cannot forge capabilities.
3. Transfer occurs only through an authorized IPC mechanism.
4. A derived capability cannot exceed its source's rights.
5. Revocation has defined semantics.
6. Every table operation validates bounds, rights, lifetime, and concurrency.
7. Destroyed objects must not leave usable references to recycled objects.

## Revocation semantics

Revocation is a policy decision with concrete mechanical consequences. For
each resource class the system must define what happens to:

- Open handles.
- Existing mappings.
- Child capabilities derived from the revoked one.
- Data already copied or displayed.
- In-flight IPC that carries the capability.

The initial policy should favor fail-closed behavior: future operations fail,
mappings are removed or made inaccessible, and descendants are invalidated.
Revocation cannot undo information an application has already legitimately
read.

## Design notes for the first implementation

- Capability slots are indices into a per-task table, but the index alone is
  not authority: the slot must contain the expected live object and rights.
- Generation counters or type tags prevent use-after-free through recycled
  slots.
- Capability transfer is a kernel-mediated message operation, not a raw write
  into another task's table.
- Revocation can be implemented with per-object back-references or a
  derivation tree; the choice must be documented in an ADR before coding.
- Adversarial tests must cover forged indices, wrong object types, insufficient
  rights, stale slots, and transfer of a non-transferable capability.

## Status

Implemented in `kernel/ipc` (`aether-ipc`) and exercised by the boot-time
scheduler test:

- Per-task table with 16 slots.
- Generation-checked handles: a forged index or stale generation fails.
- Endpoint object IDs carry their own generation.
- Rights: `SEND`, `RECEIVE`, `TRANSFER`.
- Kernel-mediated transfer that removes `TRANSFER` from the new capability.
- Tests for insufficient rights, forged/stale handles, table exhaustion,
  queue capacity, and capability transfer.

This is still a kernel-internal interface. A user-mode syscall boundary will
expose these operations to applications in a later Phase 2 slice; until then,
the tests demonstrate mechanism correctness, not a user-visible security
boundary.

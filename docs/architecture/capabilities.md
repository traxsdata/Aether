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
8. Rights are validated per object type at mint time, not at use time only:
   `Rights::for_object` masks the legal set for each object kind, and granting
   an out-of-scope right (for example `SEND` on a file) is rejected with
   `InvalidRights` instead of being silently attenuated.

## Revocation semantics

Revocation is a policy decision with concrete mechanical consequences. For
each resource class the system must define what happens to:

- Open handles.
- Existing mappings.
- Child capabilities derived from the revoked one.
- Data already copied or displayed.
- In-flight IPC that carries the capability.

The initial policy is fail-closed where revocation exists: a revoked portal
grant is already checked on every future `portal_open` call, while handles
opened before the revoke keep working because "open handles stay valid" is the
documented, weaker rule for them — the desktop shows the audit when this
happens. General capability revocation removes future minting authority
first; already-open file capabilities are re-checked right-by-right on every
syscall, so a downgraded grant never yields new access, but mapping removal
and child-capability invalidation are not implemented yet (DERIVE and MAP
rights do not exist in the minted subset). Revocation cannot undo information
an application has already legitimately read.

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
- Endpoint object IDs carry their own generation, and an operation rejects a
  capability whose object identity does not match the endpoint.
- Rights: `SEND`, `RECEIVE`, `TRANSFER`, `READ`, and `WRITE`.
- Object types: `Endpoint` and `File`.
- Kernel-mediated transfer that removes `TRANSFER` from the new capability and
  commits the receiver slot before consuming the queued message.
- Tests for insufficient rights, forged/stale handles, wrong endpoints, table
  exhaustion, queue capacity, capability transfer, file rights/type checks, and
  removal/generation reuse.
- File capabilities are exposed to ring-3 code through `open`, `read`, `write`,
  and `close` syscalls. A userspace handle is only a slot index and generation;
  the kernel checks the object type and right before every operation.
- Endpoint capabilities are minted by the caller's own `endpoint_create`
  syscall (system call 15) with the full `SEND | RECEIVE | TRANSFER` set, from
  the caller's own table. Message syscalls 16–17 then validate every handle
  through the same table the kernel knows.
- A wrong object type is reported as `WrongObjectType` (mapped to `EBADF`)
  even when the requested rights would also miss; the rights check in each
  typed accessor runs after the type check, so error precedence is stable.
- The kernel-side file service reads a task's File-kind capability count with
  `file_count()` when it enforces a verified package's `limits.open_files`.
- Tests cover mint-time rights masking, file right snapshots and counts,
  insufficient rights, forged/stale handles, wrong endpoints, table
  exhaustion, queue capacity, capability transfer (including the mandatory
  `TRANSFER` strip), and removal/generation reuse.

Portal grants can be revoked, which blocks future `portal_open` calls;
already-open file capabilities are re-checked against the open handle's
minted rights on every operation, so they cannot gain authority from a
revocation. General capability revocation, endpoint revocation, complete
object destruction, and derivation-tree-based invalidation are not
implemented yet; `DERIVE` and `MAP` rights do not exist in the minted subset.
The generation counters prepare the table for them.

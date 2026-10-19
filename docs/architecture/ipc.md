# IPC Design

IPC is the foundation of the user-space architecture. Services communicate
through bounded, capability-checked messages rather than ambient authority or
global names.

## Required primitives

- Synchronous request/reply.
- Asynchronous notification/events.
- Capability transfer.
- Bounded message sizes.
- Timeouts and cancellation.
- Sender identity where the protocol requires it.
- Backpressure and resource-exhaustion protection.

## Message model

A message contains a small, fixed header and a bounded payload. The header
identifies the operation and payload length; the kernel validates the length
before copying. Capabilities travel in a separate, explicitly sized list and
are installed in the receiver's table only when the receive commits.

The initial implementation should copy small messages and use shared-memory
capabilities for bulk data. Shared memory is created explicitly; mapping the
same frame into two address spaces does not grant authority over anything else.

## Endpoints

An endpoint is a kernel object with a receive queue and a capability-controlled
send right. Clients hold send capabilities; servers hold receive capabilities.
No global endpoint namespace is required for the MVP.

## Failure behavior

- Malformed, oversized, or truncated messages are rejected without panicking.
- A dead or revoked endpoint returns a defined error.
- A blocked sender can time out or be cancelled.
- Per-task and per-endpoint quotas bound queue memory.
- A faulting client cannot corrupt the server's address space through IPC.

## Security checks

For every message the kernel verifies:

- Endpoint capability validity and send right.
- Payload bounds and alignment.
- Every transferred capability's validity, type, and `TRANSFER` right.
- Receiver table capacity before committing the receive.
- Sender identity is supplied by the kernel from the current task, not copied
  from a message field. A user-mode syscall wrapper must preserve this rule.

## Status

Implemented in `kernel/ipc` (`aether-ipc`) plus a kernel object table:

- Two endpoint objects in the boot test, each with a four-message queue.
- 32-byte payload limit; messages carry sender identity and an optional
  capability transfer.
- Send/receive require the corresponding endpoint capability right, and the
  capability's object identity must match the endpoint being operated on.
- Capability transfer requires `TRANSFER`; the receiver gets a new table slot
  and the transferred capability drops `TRANSFER`.
- The receiver's capability slot is installed before the queue head advances.
  If the table is full, the message stays queued and no capability is lost.
- Queue operations run with interrupts masked, so preemption cannot observe a
  half-updated queue.
- Polled semantics: send returns `Full` and receive returns `Empty`; test tasks
  wait with `HLT` until the timer preempts them. Blocking/wakeup queues are next.
- Host tests cover bounded messages, rights, forged handles, endpoint
  confusion, queue capacity, table exhaustion, and transfer.

The endpoint API is not yet exposed through a user-mode syscall boundary.
File operations have a separate validated syscall path and per-task file
capabilities; endpoint syscalls, blocking receives, and timeouts remain
follow-up work.

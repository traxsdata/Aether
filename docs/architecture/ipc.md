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
are installed in the receiver's table only if the send operation succeeds.

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
- Sender identity matches what the protocol is allowed to observe.

## Status

Not implemented. The first IPC implementation should land with the capability
table and adversarial tests, not before.

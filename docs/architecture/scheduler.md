# Tasks, Scheduler, and Address Spaces

The scheduler started as a Phase 2 kernel-task slice and now also runs one
ring-3 native application. Kernel workers prove address-space switching; the
user task proves the user/kernel boundary, syscall validation, and per-process
file root.

## Current slice

- Five static task slots: one idle task, three ring-0 workers, and one ring-3
  user task.
- Worker stacks are 16 KiB `.bss` arrays. Every task also has a 32 KiB static
  kernel stack used for ring-3 interrupts and syscalls.
- A 100 Hz timer interrupt from the 8254 PIT through the remapped 8259 PIC.
  IRQ0 (timer) and IRQ1 (keyboard) are unmasked; all other legacy IRQs stay
  masked.
- Round-robin preemptive scheduling on every timer tick.
- A per-task PML4, a private physical frame for workers, and a user-only
  address-space window for the native application.
- A CR3 switch on every task switch.
- NX and supervisor write-protect enabled before the first address space is
  activated.
- Per-task capability tables, endpoint identity checks, and two endpoints.
- An isolation self-test, a capability/IPC self-test, and a ring-3 application
  self-test in the normal boot path.

There is no kernel heap. Task control blocks and stacks are static, and the
frame allocator supplies address-space page tables, user pages, and worker
frames during scheduler initialization.

## Task model

Each task records:

- state (`Empty`, `Ready`, `Running`, `Done`, `Faulted`);
- saved interrupt-frame pointer;
- PML4 physical address;
- kernel stack top loaded into TSS `RSP0` when the task is selected;
- a kernel-owned filesystem root for the user task;
- a capability table and endpoint handles.

The idle task is the boot context (`kernel_main`). It keeps the kernel PML4 and
runs the console shell after the boot tests finish. Worker tasks are created
with prepared interrupt-return frames so the first `IRETQ` enters
`worker_entry(task_id)` with interrupts enabled. The user task is created from
the embedded native ELF in a private PML4 and enters at CPL3.

The scheduler is deliberately small and deterministic: no dynamic task
creation, no priorities, and no SMP. The user-task slice is static; process
creation and resource accounting are follow-up work.

## Timer and interrupt path

`arch::x86_64::timer` remaps the PIC to vectors 32–47 and programs PIT channel 0
for 100 Hz.

The IRQ path in `arch::x86_64::idt`:

1. pushes a synthetic error code and vector;
2. saves all 15 general-purpose registers in a fixed `InterruptFrame` layout;
3. aligns the stack and calls `scheduler::on_tick(frame)`;
4. saves the interrupted frame, selects the next ready task, and updates TSS
   `RSP0`;
5. switches CR3 if the selected task has a different address space;
6. restores the selected frame and executes `IRETQ`.

The same assembly tail is used for exceptions and the `int 0x80` syscall gate.
A user fault is routed to `scheduler::on_user_fault`, which retires only that
task. A fault from ring 0 remains fatal.

There is no separate emergency stack or IST for double faults and stack
overflow. Exception reporting depends on the interrupted stack still being
usable; guard pages and IST entries are follow-up hardening.

## Scheduling policy

`on_tick` saves the interrupted task's frame, marks a running task ready, scans
round-robin for the next ready task, and falls back to the idle task when
nothing else is runnable. Finished, exited, and faulted tasks are skipped.

The `yield` and `sleep` syscalls save the caller's frame and enter the same
selection path, so they actually schedule another ready task instead of
returning immediately. There is still no sleep queue: `sleep` yields for at
most the next timer tick.

This is intentionally minimal: no priorities, no blocking IPC wakeups, no SMP,
no load balancing, and no preemption points outside the timer interrupt and
explicit yields.

## Address-space isolation

`memory::paging::create_address_space` allocates a new PML4, copies the kernel
identity mapping entry, and maps a private physical frame at
`PRIVATE_BASE = 0x0000_0080_0000_0000` (PML4 slot 1). Each worker has a
different physical frame at the same virtual address.

`create_user_address_space` copies the supervisor-only kernel entry and adds
user mappings only in the validated user window at `USER_BASE`. Every `PT_LOAD`
page gets its own frame and final permissions from the ELF flags; the stack is
mapped with NX. Kernel text, stacks, page tables, and the identity map are never
mapped with the user bit.

The worker test:

1. writes `0xA5A5_0000_0000_0000 | task_id` to `PRIVATE_BASE`;
2. waits for timer-driven preemption;
3. re-reads the address and verifies it still contains its own value.

If CR3 were not switched, another task would overwrite the shared virtual
address and the test would fail with
`AETHER_BOOT_FAILED reason=isolation ...`.

This demonstrates hardware address-space isolation between kernel tasks. The
ring-3 application separately demonstrates that user code cannot reach the
kernel identity map and that syscall pointers are validated against its user
window.

## Capability and IPC test

After the isolation rounds the workers exercise the capability and IPC path:

- Worker 1 receives on endpoint A.
- Worker 2 sends data, then transfers its endpoint B send capability through a
  message.
- Worker 1 installs the transferred capability and uses it to send on endpoint
  B; worker 2 receives that reply.
- Worker 3's send with a receive-only capability is denied, and a forged handle
  is rejected. It then sends one valid message.
- The idle task checks that exactly two operations were denied and three
  messages were received before printing `AETHER_CAP_OK` and `AETHER_IPC_OK`.

Capabilities are per task, handles are generation-checked, endpoint identity is
checked against the target object, and transfer requires `TRANSFER`. A failed
transfer install does not consume the queued message. Endpoint capabilities are
now also minted from ring 3 through the `endpoint_create` syscall and used by
the user task's own IPC demonstration (see
[ipc.md](ipc.md) and the system ABI specification).

## Verified behavior

`make test` requires the full marker set in
[`tools/test-boot.py`](../../tools/test-boot.py), including:

```text
AETHER_MM_OK
AETHER_SCHED_OK workers=3 ticks=N
AETHER_ISOLATION_OK tasks=3
AETHER_CAP_OK denials=2
AETHER_IPC_OK messages=3
AETHER_APP_OK
AETHER_USER_OK tasks=4 exits=1 faults=0
AETHER_SANDBOX_OK escapes=4 access_denials=0 quota_denials=1
AETHER_IPC_USER_OK sends=7 receives=7 denials=7
AETHER_BOOT_OK stage=kernel version=0.1.0
```

## Task exit, faults, and the boot gate

A user task's exit line records `user: exit pid=N status=S closed=C`, where
`closed` is the number of file capability handles the kernel closed on the
task's behalf (`fs::close_task_files`). The kernel closes them on normal exit
and on every user fault, so a retired task can never keep open-file slots that
shrink the pool shared by later tasks. The retired task never runs again, so
its remaining capability table is unreachable; endpoint objects owned by an
exited task are not reclaimed yet because the pool has no destroy path.

Every user failure is accounted: `user_exits()`/`user_faults()` end up in
`AETHER_USER_OK`, denial counters in `AETHER_SANDBOX_OK`/`AETHER_CAP_OK`, and
the application table. The desktop counters read the same sources.

The boot gate in the normal path makes the evidence mandatory rather than
decorative:

- A user task that exits nonzero, or which faults, fails the boot with
  `AETHER_BOOT_FAILED reason=user-test ...`.
- A task that claims a zero exit without exercising the contract fails the
  gate: `AETHER_BOOT_FAILED reason=user-ipc sends=... receives=... denials=...`
  unless the IPC counters reached at least 7 sends, 7 receives, 7
  denials, and the filesystem quota counter reported at least one denied
  fifth open. Only then is `AETHER_IPC_USER_OK` printed — by the kernel, not
  by the user program.

`make fault-test` independently verifies the exception path before the
scheduler starts, and `make rollback-test` exercises the recovery rollback
branch with its own `AETHER_RECOVERY_ROLLBACK_OK` marker.

## Known limitations

- Static task set; no process creation, wait, or per-process resource
  accounting beyond the counters above.
- Endpoint objects created by a task are not destroyed when the task exits;
  the fixed eight-object pool is not reclaimed.
- Single CPU; no SMP startup or per-CPU scheduler state.
- No priorities, sleeping/wakeup queues, or blocking IPC.
- Per-task kernel stacks are static, not capability-managed.
- Address spaces share all kernel mappings; there is no higher-half split.
- No guard pages or IST for stack overflow.
- The timer uses legacy PIC/PIT rather than the local APIC.
- One ring-3 program is loaded at boot; there is no general user-space loader
  or service supervision in ring 3.

## Next steps

1. Dynamic user-process creation and wait (retired tasks already release their
   open file slots through `close_task_files`).
2. Guard pages and IST entries for stack overflow and double faults.
3. Blocking send/receive with wakeup queues and timeouts.
4. A user-space loader and `initd` supervision for real services.
5. Adversarial scheduler, capability, IPC, and isolation tests, including a
   deliberately shared address space that must fail the isolation check.

# Tasks, Scheduler, and Address Spaces

Phase 2 begins with preemptive kernel tasks and hardware-enforced address-space
isolation. User mode, capabilities, and IPC build on this slice.

## Current slice

- Four static task slots: one idle task and three workers.
- A per-task 16 KiB kernel stack in `.bss`.
- A 100 Hz timer interrupt from the 8254 PIT through the remapped 8259 PIC.
- Round-robin preemptive scheduling on every timer tick.
- A per-task PML4 and private physical frame mapped at the same virtual address.
- A CR3 switch on every task switch.
- An isolation self-test that runs in the normal boot path.

There is no kernel heap. Task control blocks and stacks are static, and the
frame allocator supplies address-space page tables and private frames during
scheduler initialization.

## Task model

Each task records:

- state (`Empty`, `Ready`, `Running`, `Done`);
- saved interrupt-frame pointer;
- PML4 physical address.

The idle task is the boot context (`kernel_main`). Its stack is the kernel boot
stack and it keeps the kernel PML4. Worker tasks are created with prepared
interrupt-return frames so the first `IRETQ` enters `worker_entry(task_id)` with
interrupts enabled.

Static limits are deliberate: the first scheduler is small, deterministic, and
auditable. Dynamic task creation, per-task kernel stacks, and a heap arrive with
the user-space task model.

## Timer and interrupt path

`arch::x86_64::timer` remaps the PIC to vectors 32–47 and programs PIT channel 0
for 100 Hz. IRQ0 is the only unmasked legacy IRQ.

The IRQ path in `arch::x86_64::idt`:

1. pushes a synthetic error code and vector 32;
2. saves all 15 general-purpose registers in a fixed `InterruptFrame` layout;
3. aligns the stack and calls `scheduler::on_tick(frame)`;
4. switches CR3 if the selected task has a different address space;
5. restores the selected frame and executes `IRETQ`.

Exceptions use a separate emergency stack so a fault on a corrupted task stack
can still produce a diagnostic instead of a triple fault.

## Scheduling policy

`on_tick` saves the interrupted task's frame, marks a running task ready, scans
round-robin for the next ready worker, and falls back to the idle task when no
worker is runnable. Finished workers are marked `Done` and skipped.

This is intentionally minimal: no priorities, no sleeping/wakeup queues, no
SMP, no load balancing, and no preemption points outside the timer interrupt.

## Address-space isolation

`memory::paging::create_address_space` allocates a new PML4, copies the kernel
identity mapping entry, and maps a private physical frame at
`PRIVATE_BASE = 0x0000_0080_0000_0000` (PML4 slot 1). Each task has a different
physical frame at the same virtual address.

The worker test:

1. writes `0xA5A5_0000_0000_0000 | task_id` to `PRIVATE_BASE`;
2. waits for timer-driven preemption;
3. re-reads the address and verifies it still contains its own value.

If CR3 were not switched, another task would overwrite the shared virtual
address and the test would fail with
`AETHER_BOOT_FAILED reason=isolation ...`.

This demonstrates hardware address-space isolation between kernel tasks. It is
not yet user/kernel isolation: all tasks run at CPL0 and share the kernel
mapping. User-accessible page permissions, a TSS, and user-mode entry are the
next step.

## Verified behavior

`make test` requires:

```text
AETHER_MM_OK
AETHER_SCHED_OK workers=3 ticks=N
AETHER_ISOLATION_OK tasks=3
AETHER_BOOT_OK
```

`make fault-test` independently verifies the exception path before the
scheduler starts.

## Known limitations

- Kernel tasks only; no ring-3 tasks or syscall boundary.
- Single CPU; no SMP startup or per-CPU scheduler state.
- No priorities, sleeping, wakeup, or task creation syscall.
- Per-task kernel stacks are static, not capability-managed.
- Address spaces share all kernel mappings; there is no higher-half split.
- No guard pages or IST for stack overflow.
- The timer uses legacy PIC/PIT rather than the local APIC.

## Next steps

1. User-mode entry: TSS/RSP0, user page permissions, syscall gate.
2. Per-task kernel stacks and guard pages.
3. Capability tables attached to tasks.
4. Bounded IPC endpoints and capability transfer.
5. User-space loader and `initd` supervision.
6. Adversarial scheduler and isolation tests, including a deliberately shared
   address space that must fail the isolation check.

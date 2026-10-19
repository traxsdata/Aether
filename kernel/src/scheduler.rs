// SPDX-License-Identifier: Apache-2.0
//! Phase 2 preemptive kernel scheduler and address-space isolation test.
//!
//! The first Phase 2 slice runs several ring-0 tasks, each with its own PML4
//! and a private physical frame mapped at the same virtual address. The timer
//! preempts them round-robin, changing CR3 on every switch. A task writes a
//! unique value to the shared virtual address, waits for a timer tick, and
//! verifies that another task did not overwrite it. This demonstrates
//! hardware-enforced address-space isolation before user mode exists.
//!
//! Task control blocks and stacks are static: there is no kernel heap yet.
//! Scheduling runs only in interrupt context on a single CPU, so the scheduler
//! state is not concurrently accessible.

use crate::arch::x86_64::idt::InterruptFrame;
use crate::arch::x86_64::{halt, timer};
use crate::memory::paging::{self, PagingError};
use crate::serial_line;
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Number of worker tasks in the isolation test.
pub const WORKER_COUNT: usize = 3;

const MAX_TASKS: usize = WORKER_COUNT + 1;
const STACK_SIZE: usize = 16 * 1024;
const ROUNDS: usize = 4;
const MAGIC_PREFIX: u64 = 0xa5a5_0000_0000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Empty,
    Ready,
    Running,
    Done,
}

#[derive(Clone, Copy)]
struct Task {
    state: TaskState,
    saved_rsp: u64,
    pml4: u64,
}

impl Task {
    const EMPTY: Self = Self {
        state: TaskState::Empty,
        saved_rsp: 0,
        pml4: 0,
    };
}

/// Minimal `Sync` wrapper for boot-time globals that are only mutated while
/// interrupts are disabled or from the timer handler itself.
struct Global<T>(UnsafeCell<T>);

// SAFETY: Access is serialized by the single-CPU boot model and interrupt
// masking; see the module documentation.
unsafe impl<T> Sync for Global<T> {}

static TASKS: Global<[Task; MAX_TASKS]> = Global(UnsafeCell::new([Task::EMPTY; MAX_TASKS]));
static STACKS: Global<[[u8; STACK_SIZE]; WORKER_COUNT]> =
    Global(UnsafeCell::new([[0; STACK_SIZE]; WORKER_COUNT]));

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Scheduler initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    /// The active kernel PML4 could not be read.
    MissingKernelPml4,
    /// No frame was available for a task's private page or page tables.
    OutOfMemory,
    /// Address-space construction failed.
    Paging(PagingError),
}

impl From<PagingError> for SchedulerError {
    fn from(error: PagingError) -> Self {
        Self::Paging(error)
    }
}

/// Initializes the scheduler and runs the isolation test forever.
///
/// This function never returns. It prints the Phase 2 success markers and
/// halts after every worker has completed its isolation checks.
pub fn run(allocator: &mut FrameAllocator<'_>) -> ! {
    if let Err(error) = init(allocator) {
        serial_line!("AETHER_BOOT_FAILED reason=scheduler: {error:?}");
        halt()
    }

    timer::init();
    timer::enable_interrupts();
    serial_line!("sched: timer started at {} Hz", timer::TIMER_HZ);

    loop {
        if DONE.load(Ordering::Acquire) == WORKER_COUNT {
            timer::disable_interrupts();
            serial_line!(
                "AETHER_SCHED_OK workers={} ticks={}",
                WORKER_COUNT,
                TICKS.load(Ordering::Relaxed)
            );
            serial_line!("AETHER_ISOLATION_OK tasks={WORKER_COUNT}");
            serial_line!(
                "AETHER_BOOT_OK stage=kernel version={}",
                env!("CARGO_PKG_VERSION")
            );
            halt()
        }
        timer::halt_until_interrupt();
    }
}

fn init(allocator: &mut FrameAllocator<'_>) -> Result<(), SchedulerError> {
    let kernel_pml4 = paging::current_pml4().ok_or(SchedulerError::MissingKernelPml4)?;
    let tasks = unsafe { &mut *TASKS.0.get() };
    let stacks = unsafe { &mut *STACKS.0.get() };

    // Task 0 is the idle task: it is the boot context and keeps the kernel
    // address space.
    tasks[0] = Task {
        state: TaskState::Running,
        saved_rsp: 0,
        pml4: kernel_pml4.start_address(),
    };

    for (index, stack) in stacks.iter_mut().enumerate() {
        let task_id = index + 1;
        let private = allocator.allocate().ok_or(SchedulerError::OutOfMemory)?;
        // SAFETY: The frame is exclusively owned by this task.
        unsafe { ptr::write_bytes(private.start_address() as *mut u8, 0, PAGE_SIZE as usize) };

        let pml4 = paging::create_address_space(allocator, private)?;
        let stack_top = stack.as_mut_ptr() as u64 + STACK_SIZE as u64;
        let saved_rsp = prepare_stack(stack_top, worker_entry, task_id as u64);

        tasks[task_id] = Task {
            state: TaskState::Ready,
            saved_rsp,
            pml4: pml4.start_address(),
        };
        serial_line!(
            "sched: task {task_id} ready pml4={:#018x} private={:#018x}",
            pml4.start_address(),
            private.start_address()
        );
    }

    Ok(())
}

/// Builds an interrupt-return frame so IRETQ enters `entry(arg)` as a fresh
/// task with interrupts enabled.
fn prepare_stack(stack_top: u64, entry: extern "C" fn(u64) -> !, arg: u64) -> u64 {
    let frame_size = core::mem::size_of::<InterruptFrame>() as u64;
    // Keep the initial return frame 16 bytes below the stack top. The two
    // words above it are reserved so no frame write can cross into the next
    // static allocation, and so an RSP/SS slot is available if the CPU takes
    // an outer-privilege return path.
    let frame_end = (stack_top - 16) & !7;
    let frame_start = frame_end - frame_size;

    // SAFETY: Both addresses are inside the task's own stack allocation.
    unsafe {
        ptr::write(frame_end as *mut u64, frame_end);
        ptr::write((frame_end + 8) as *mut u64, 0x10);
        let frame = InterruptFrame {
            rdi: arg,
            rip: entry as usize as u64,
            cs: 0x08,
            // IF=1 so the timer can preempt the task, reserved bit 1 set.
            rflags: 0x202,
            ..InterruptFrame::default()
        };
        ptr::write(frame_start as *mut InterruptFrame, frame);
    }
    frame_start
}

/// Worker task: verify that its private mapping survives preemption.
extern "C" fn worker_entry(task_id: u64) -> ! {
    let magic = MAGIC_PREFIX | task_id;
    let private = paging::PRIVATE_BASE as *mut u64;

    for round in 0..ROUNDS {
        // SAFETY: The current address space maps PRIVATE_BASE to this task's
        // private frame.
        unsafe { ptr::write_volatile(private, magic) };

        // Wait for at least one timer interrupt. The scheduler may switch to
        // another address space here; when this task resumes, its mapping must
        // still contain its own value.
        timer::halt_until_interrupt();

        // SAFETY: Same mapping as above.
        let observed = unsafe { ptr::read_volatile(private) };
        if observed != magic {
            serial_line!(
                "AETHER_BOOT_FAILED reason=isolation task={task_id} round={round} \
                 expected={magic:#018x} observed={observed:#018x}"
            );
            halt()
        }
    }

    finish_current();
    loop {
        timer::halt_until_interrupt();
    }
}

/// Marks the current task finished so the scheduler stops selecting it.
fn finish_current() {
    timer::disable_interrupts();
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].state = TaskState::Done;
    DONE.fetch_add(1, Ordering::Release);
    timer::enable_interrupts();
}

/// Called from the timer interrupt before returning to a task.
pub fn on_tick(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    TICKS.fetch_add(1, Ordering::Relaxed);
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);

    tasks[current].saved_rsp = frame as u64;
    if tasks[current].state == TaskState::Running {
        tasks[current].state = TaskState::Ready;
    }

    // Round-robin over worker tasks; use the idle task only when no worker is
    // runnable. This keeps the isolation test progressing.
    let mut next = current;
    for step in 1..=MAX_TASKS {
        let index = (current + step) % MAX_TASKS;
        if tasks[index].state == TaskState::Ready {
            next = index;
            break;
        }
    }
    if next == current && tasks[current].state != TaskState::Ready {
        // No worker is ready. The idle task is always runnable.
        next = 0;
        tasks[next].state = TaskState::Ready;
    }
    tasks[next].state = TaskState::Running;
    CURRENT.store(next, Ordering::Relaxed);

    if next != current
        && let Some(pml4) = PhysFrame::from_address(tasks[next].pml4)
    {
        paging::activate(pml4);
    }

    tasks[next].saved_rsp as *mut InterruptFrame
}

// SPDX-License-Identifier: Apache-2.0
//! Preemptive scheduler, address-space isolation, and the first user process.
//!
//! Phase 2 runs several ring-0 workers in separate address spaces and exercises
//! capability and IPC checks. Phase 3 adds a ring-3 process created from an
//! embedded native ELF image. The scheduler switches CR3 and the TSS kernel
//! stack on every context switch, so a timer interrupt or system call that
//! arrives from user mode lands on that process's private kernel stack.
//!
//! Task control blocks and stacks are static: there is no kernel heap yet.
//! Scheduling runs only in interrupt or system-call context on a single CPU,
//! so the scheduler state is not concurrently accessible.

use crate::arch::x86_64::gdt::{self, USER_CODE_SELECTOR, USER_DATA_SELECTOR};
use crate::arch::x86_64::idt::InterruptFrame;
use crate::arch::x86_64::{halt, timer};
use crate::memory::paging::{self, PagingError};
use crate::serial_line;
use aether_abi::user::{USER_STACK_TOP, UserTestResult};
use aether_boot_common::elf;
use aether_fs::FsError;
use aether_ipc::{CapError, CapHandle, CapTable, IpcError, ObjectType, Rights};
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_services::ServiceError;
use aether_services::runtime::RuntimeError;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Number of ring-0 worker tasks in the isolation test.
pub const WORKER_COUNT: usize = 3;
/// Task index of the ring-3 process.
pub const USER_TASK: usize = WORKER_COUNT + 1;

const MAX_TASKS: usize = WORKER_COUNT + 2;
const STACK_SIZE: usize = 16 * 1024;
const KERNEL_STACK_SIZE: usize = 32 * 1024;
const ROUNDS: usize = 4;
const MAGIC_PREFIX: u64 = 0xa5a5_0000_0000_0000;

/// Native user program embedded by the kernel build.
static USER_ELF: &[u8] = include_bytes!(env!("AETHER_USER_ELF"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Empty,
    Ready,
    Running,
    Done,
    Faulted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskKind {
    Empty,
    Idle,
    Worker,
    User,
}

#[derive(Clone, Copy, Default)]
struct TaskHandles {
    ep_a_send: Option<CapHandle>,
    ep_a_recv: Option<CapHandle>,
    ep_b_send: Option<CapHandle>,
    ep_b_recv: Option<CapHandle>,
}

struct Task {
    state: TaskState,
    kind: TaskKind,
    saved_rsp: u64,
    pml4: u64,
    kernel_stack_top: u64,
    root: &'static str,
    caps: CapTable,
    handles: TaskHandles,
}

impl Task {
    const fn new() -> Self {
        Self {
            state: TaskState::Empty,
            kind: TaskKind::Empty,
            saved_rsp: 0,
            pml4: 0,
            kernel_stack_top: 0,
            root: "/",
            caps: CapTable::new(),
            handles: TaskHandles {
                ep_a_send: None,
                ep_a_recv: None,
                ep_b_send: None,
                ep_b_recv: None,
            },
        }
    }
}

/// Minimal `Sync` wrapper for boot-time globals that are only mutated while
/// interrupts are disabled or from the timer handler itself.
struct Global<T>(UnsafeCell<T>);

// SAFETY: Access is serialized by the single-CPU boot model and interrupt
// masking; see the module documentation.
unsafe impl<T> Sync for Global<T> {}

static TASKS: Global<[Task; MAX_TASKS]> =
    Global(UnsafeCell::new([const { Task::new() }; MAX_TASKS]));
static STACKS: Global<[[u8; STACK_SIZE]; WORKER_COUNT]> =
    Global(UnsafeCell::new([[0; STACK_SIZE]; WORKER_COUNT]));
static KERNEL_STACKS: Global<[[u8; KERNEL_STACK_SIZE]; MAX_TASKS]> =
    Global(UnsafeCell::new([[0; KERNEL_STACK_SIZE]; MAX_TASKS]));

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static DENIED: AtomicUsize = AtomicUsize::new(0);
static MESSAGES: AtomicUsize = AtomicUsize::new(0);
static USER_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);
static USER_EXITS: AtomicUsize = AtomicUsize::new(0);
static MEM_FREE_MIB: AtomicUsize = AtomicUsize::new(0);
static MEM_TOTAL_MIB: AtomicUsize = AtomicUsize::new(0);

/// Scheduler initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerError {
    /// The active kernel PML4 could not be read.
    MissingKernelPml4,
    /// No frame was available for a task's private page or page tables.
    OutOfMemory,
    /// Address-space construction failed.
    Paging(PagingError),
    /// No endpoint object was available.
    NoEndpoint,
    /// Capability table initialization failed.
    Capability(CapError),
    /// The embedded user program is not a valid executable.
    UserImage(elf::ParseError),
    /// The virtual filesystem could not be formatted or seeded.
    Filesystem(FsError),
    /// A service could not be registered or started.
    Service(ServiceError),
    /// The embedded package failed verification or installation.
    Package(&'static str),
    /// Runtime isolation policy failed.
    Runtime(RuntimeError),
    /// Update verification policy failed.
    Update(&'static str),
    /// Portal grant/revocation self-test failed.
    Portal,
}

impl From<PagingError> for SchedulerError {
    fn from(error: PagingError) -> Self {
        Self::Paging(error)
    }
}

impl From<CapError> for SchedulerError {
    fn from(error: CapError) -> Self {
        Self::Capability(error)
    }
}

/// Initializes the scheduler, starts the timer, and waits for the boot tests.
///
/// This function never returns. It prints the Phase 2 and Phase 3 success
/// markers and halts after every worker and the user process have completed.
pub fn run(allocator: &mut FrameAllocator<'_>) -> ! {
    if let Err(error) = init(allocator) {
        serial_line!("AETHER_BOOT_FAILED reason=scheduler: {error:?}");
        halt()
    }

    timer::init();
    timer::enable_interrupts();
    serial_line!("sched: timer started at {} Hz", timer::TIMER_HZ);

    loop {
        if DONE.load(Ordering::Acquire) == WORKER_COUNT
            && USER_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
        {
            timer::disable_interrupts();
            let denied = DENIED.load(Ordering::Acquire);
            let messages = MESSAGES.load(Ordering::Acquire);
            if denied != 2 || messages != 3 {
                serial_line!("AETHER_BOOT_FAILED reason=ipc denials={denied} messages={messages}");
                halt()
            }
            let user_result = USER_RESULT.load(Ordering::Acquire);
            if user_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=user-test result={} faults={}",
                    user_result,
                    USER_FAULTS.load(Ordering::Acquire)
                );
                halt()
            }

            serial_line!(
                "AETHER_SCHED_OK workers={} ticks={}",
                WORKER_COUNT,
                TICKS.load(Ordering::Relaxed)
            );
            serial_line!("AETHER_ISOLATION_OK tasks={WORKER_COUNT}");
            serial_line!("AETHER_CAP_OK denials={denied}");
            serial_line!("AETHER_IPC_OK messages={messages}");
            let escapes = crate::fs::escape_denials();
            if escapes < 2 {
                serial_line!("AETHER_BOOT_FAILED reason=sandbox escapes={escapes}");
                halt()
            }
            // The ring-3 IPC demo must have exercised the bound positive and
            // negative paths: sends, receives, and at least one denial of
            // every kind. A user task that exited by claiming success without
            // producing the evidence fails the boot.
            if crate::syscall::ipc_sends() < 7
                || crate::syscall::ipc_receives() < 7
                || crate::syscall::ipc_denials() < 7
                || crate::fs::quota_denials() < 1
            {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=user-ipc sends={} receives={} denials={} \
                     quota={}",
                    crate::syscall::ipc_sends(),
                    crate::syscall::ipc_receives(),
                    crate::syscall::ipc_denials(),
                    crate::fs::quota_denials()
                );
                halt()
            }
            serial_line!(
                "AETHER_USER_OK tasks={} exits={} faults={}",
                MAX_TASKS - 1,
                USER_EXITS.load(Ordering::Acquire),
                USER_FAULTS.load(Ordering::Acquire)
            );
            serial_line!(
                "AETHER_FS_OK opens={} denials={}",
                crate::fs::open_count(),
                crate::fs::access_denials()
            );
            serial_line!(
                "AETHER_SANDBOX_OK escapes={} access_denials={} quota_denials={}",
                escapes,
                crate::fs::access_denials(),
                crate::fs::quota_denials()
            );
            serial_line!(
                "AETHER_IPC_USER_OK sends={} receives={} denials={}",
                crate::syscall::ipc_sends(),
                crate::syscall::ipc_receives(),
                crate::syscall::ipc_denials()
            );
            serial_line!(
                "AETHER_BOOT_OK stage=kernel version={}",
                env!("CARGO_PKG_VERSION")
            );
            // The boot self-tests are complete. Keep the console alive so the
            // shell test and a human can inspect the running system.
            timer::enable_interrupts();
            loop {
                crate::shell::poll();
                timer::halt_until_interrupt();
            }
        }
        crate::shell::poll();
        timer::halt_until_interrupt();
    }
}

fn init(allocator: &mut FrameAllocator<'_>) -> Result<(), SchedulerError> {
    let kernel_pml4 = paging::current_pml4().ok_or(SchedulerError::MissingKernelPml4)?;
    let tasks = unsafe { &mut *TASKS.0.get() };
    let stacks = unsafe { &mut *STACKS.0.get() };
    let kernel_stacks = unsafe { &mut *KERNEL_STACKS.0.get() };

    // Task 0 is the idle task: it is the boot context and keeps the kernel
    // address space. Its kernel stack is the stack used during boot.
    let mut idle = Task::new();
    idle.state = TaskState::Running;
    idle.kind = TaskKind::Idle;
    idle.pml4 = kernel_pml4.start_address();
    idle.kernel_stack_top = stack_top(&kernel_stacks[0]);
    tasks[0] = idle;

    for (index, stack) in stacks.iter_mut().enumerate() {
        let task_id = index + 1;
        let private = allocator.allocate().ok_or(SchedulerError::OutOfMemory)?;
        // SAFETY: The frame is exclusively owned by this task.
        unsafe { ptr::write_bytes(private.start_address() as *mut u8, 0, PAGE_SIZE as usize) };

        let pml4 = paging::create_address_space(allocator, private)?;
        let task_stack_top = stack.as_mut_ptr() as u64 + STACK_SIZE as u64;
        let saved_rsp = prepare_stack(task_stack_top, worker_entry, task_id as u64);

        let mut task = Task::new();
        task.state = TaskState::Ready;
        task.kind = TaskKind::Worker;
        task.saved_rsp = saved_rsp;
        task.pml4 = pml4.start_address();
        task.kernel_stack_top = stack_top(&kernel_stacks[task_id]);
        tasks[task_id] = task;
        serial_line!(
            "sched: task {task_id} ready pml4={:#018x} private={:#018x}",
            pml4.start_address(),
            private.start_address()
        );
    }

    // Create two endpoints and grant table-local capabilities. Worker 3 gets a
    // receive-only capability on endpoint A to exercise insufficient rights.
    let endpoint_a = crate::ipc::create_endpoint().ok_or(SchedulerError::NoEndpoint)?;
    let endpoint_b = crate::ipc::create_endpoint().ok_or(SchedulerError::NoEndpoint)?;
    let rights = ObjectType::Endpoint;

    tasks[1].handles.ep_a_recv = Some(tasks[1].caps.grant(endpoint_a, rights, Rights::RECEIVE)?);
    tasks[1].handles.ep_b_recv = Some(tasks[1].caps.grant(endpoint_b, rights, Rights::RECEIVE)?);

    tasks[2].handles.ep_a_send = Some(tasks[2].caps.grant(
        endpoint_a,
        rights,
        Rights::SEND | Rights::TRANSFER,
    )?);
    tasks[2].handles.ep_b_send = Some(tasks[2].caps.grant(
        endpoint_b,
        rights,
        Rights::SEND | Rights::TRANSFER,
    )?);
    tasks[2].handles.ep_b_recv = Some(tasks[2].caps.grant(endpoint_b, rights, Rights::RECEIVE)?);

    tasks[3].handles.ep_a_recv = Some(tasks[3].caps.grant(endpoint_a, rights, Rights::RECEIVE)?);
    tasks[3].handles.ep_a_send = Some(tasks[3].caps.grant(
        endpoint_a,
        rights,
        Rights::SEND | Rights::TRANSFER,
    )?);

    crate::fs::init().map_err(SchedulerError::Filesystem)?;
    crate::packaging::init().map_err(SchedulerError::Package)?;
    crate::fs::seed_application_roots().map_err(SchedulerError::Filesystem)?;
    crate::runtime::init().map_err(SchedulerError::Runtime)?;
    crate::updates::init().map_err(SchedulerError::Update)?;
    crate::supervisor::init().map_err(SchedulerError::Service)?;
    crate::recovery::init().map_err(SchedulerError::Filesystem)?;
    #[cfg(feature = "recovery-rollback-selftest")]
    crate::recovery::rollback_self_test().map_err(SchedulerError::Filesystem)?;
    crate::portal::init();
    let revocation_checks = crate::portal::self_test().map_err(|_| SchedulerError::Portal)?;
    serial_line!("AETHER_REVOCATION_OK checks={revocation_checks}");
    crate::apps::init();
    crate::desktop::init();
    crate::input::init();
    crate::input::self_test();
    crate::shell::init();

    // Load the native user program into its own PML4. The kernel identity map
    // is copied supervisor-only; only USER_BASE..USER_LIMIT is user-accessible.
    let image = elf::parse(USER_ELF).map_err(SchedulerError::UserImage)?;
    let user_pml4 = paging::create_user_address_space(allocator, &image, USER_ELF)?;
    let user_kernel_top = stack_top(&kernel_stacks[USER_TASK]);
    let user_saved_rsp = prepare_user_stack(user_kernel_top, image.entry());

    let mut user = Task::new();
    user.state = TaskState::Ready;
    user.kind = TaskKind::User;
    user.saved_rsp = user_saved_rsp;
    user.pml4 = user_pml4.start_address();
    user.kernel_stack_top = user_kernel_top;
    user.root = crate::fs::DEMO_ROOT;
    tasks[USER_TASK] = user;
    crate::apps::started(USER_TASK);

    // SAFETY: Interrupts are disabled during boot and the idle task is current.
    unsafe { gdt::set_kernel_stack(tasks[0].kernel_stack_top) };
    serial_line!(
        "sched: user task ready pid={} pml4={:#018x} entry={:#018x} stack={:#018x}",
        USER_TASK,
        user_pml4.start_address(),
        image.entry(),
        USER_STACK_TOP
    );

    // Record the allocator's pressure after every boot allocation for the
    // `mem` diagnostic. The allocator is not reachable after bootstrap and no
    // frames are allocated later, so this is the free/total view the shell
    // reports for the running system.
    let stats = allocator.stats();
    MEM_FREE_MIB.store(
        (stats.free_bytes() / (1024 * 1024)) as usize,
        Ordering::Release,
    );
    MEM_TOTAL_MIB.store(
        (stats.total_bytes() / (1024 * 1024)) as usize,
        Ordering::Release,
    );

    Ok(())
}

fn stack_top(stack: &[u8; KERNEL_STACK_SIZE]) -> u64 {
    stack.as_ptr() as u64 + KERNEL_STACK_SIZE as u64
}

/// Builds an interrupt-return frame so IRETQ enters `entry(arg)` as a fresh
/// ring-0 task with interrupts enabled.
fn prepare_stack(stack_top: u64, entry: extern "C" fn(u64) -> !, arg: u64) -> u64 {
    let frame_size = core::mem::size_of::<InterruptFrame>() as u64;
    // Keep the initial return frame 16 bytes below the stack top. The two
    // words above it are reserved so no frame write can cross into the next
    // static allocation.
    let frame_end = (stack_top - 16) & !7;
    let frame_start = frame_end - frame_size;

    // SAFETY: Both addresses are inside the task's own stack allocation.
    unsafe {
        ptr::write(frame_end as *mut u64, frame_end);
        ptr::write((frame_end + 8) as *mut u64, 0x10);
        let frame = InterruptFrame {
            rdi: arg,
            rip: entry as usize as u64,
            cs: u64::from(gdt::KERNEL_CODE_SELECTOR),
            // IF=1 so the timer can preempt the task, reserved bit 1 set.
            rflags: 0x202,
            // A ring-0 IRETQ ignores these slots, but keeping them valid means
            // a frame cannot be mistaken for an outer-privilege return.
            rsp: frame_end,
            ss: u64::from(gdt::KERNEL_DATA_SELECTOR),
            ..InterruptFrame::default()
        };
        ptr::write(frame_start as *mut InterruptFrame, frame);
    }
    frame_start
}

/// Builds an interrupt-return frame that enters ring 3 at `entry`.
fn prepare_user_stack(kernel_stack_top: u64, entry: u64) -> u64 {
    let frame_size = core::mem::size_of::<InterruptFrame>() as u64;
    let frame_end = (kernel_stack_top - 16) & !7;
    let frame_start = frame_end - frame_size;

    // SAFETY: The frame is inside this task's private kernel stack. The CPU
    // will read the user RSP/SS slots when IRETQ changes privilege.
    unsafe {
        let frame = InterruptFrame {
            rip: entry,
            cs: u64::from(USER_CODE_SELECTOR),
            rflags: 0x202,
            rsp: (USER_STACK_TOP - 16) & !0xf,
            ss: u64::from(USER_DATA_SELECTOR),
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

    match task_id {
        1 => ipc_receiver(),
        2 => ipc_sender(),
        3 => ipc_denied(),
        _ => {}
    }

    finish_current();
    loop {
        timer::halt_until_interrupt();
    }
}

/// Runs `f` with the current task identity, capability table, and handles.
///
/// Interrupts are masked so a preemption cannot observe a half-updated table.
/// The task index passed to `f` is the kernel's authoritative sender identity;
/// callers must not accept a sender identity from a message payload.
fn with_current<R>(f: impl FnOnce(usize, &mut CapTable, &TaskHandles) -> R) -> R {
    let _guard = timer::InterruptGuard::enter();
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    f(current, &mut task.caps, &task.handles)
}

fn ipc_fail(operation: &str, error: IpcError) -> ! {
    serial_line!("AETHER_BOOT_FAILED reason=ipc operation={operation} error={error:?}");
    halt()
}

/// Worker 1 receives two data messages and a transferred capability, then
/// uses that capability to send a message on endpoint B.
fn ipc_receiver() {
    let mut got_normal = false;
    let mut got_from3 = false;
    let mut transferred = None;

    while !(got_normal && got_from3 && transferred.is_some()) {
        let result = with_current(|_current, caps, handles| {
            crate::ipc::receive(caps, handles.ep_a_recv.expect("receiver capability"))
        });
        match result {
            Ok(message) => {
                MESSAGES.fetch_add(1, Ordering::Release);
                let payload = message.payload();
                if message.sender == 2 && payload == b"normal" {
                    got_normal = true;
                } else if message.sender == 3 && payload == b"from3" {
                    got_from3 = true;
                } else if message.sender == 2 && payload == b"transfer" {
                    transferred = message.transferred;
                } else {
                    serial_line!(
                        "AETHER_BOOT_FAILED reason=ipc unexpected message sender={} len={}",
                        message.sender,
                        message.len
                    );
                    halt()
                }
            }
            Err(IpcError::Empty) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("receive-a", error),
        }
    }

    let transferred = transferred.expect("transfer capability");
    loop {
        let result = with_current(|current, caps, _handles| {
            crate::ipc::send(caps, transferred, current as u16, b"transferred", None)
        });
        match result {
            Ok(()) => break,
            Err(IpcError::Full) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("send-b", error),
        }
    }
}

/// Worker 2 sends data, transfers its endpoint B send capability, and then
/// receives the message that worker 1 sends using the transferred capability.
fn ipc_sender() {
    loop {
        let result = with_current(|current, caps, handles| {
            crate::ipc::send(
                caps,
                handles.ep_a_send.expect("send capability"),
                current as u16,
                b"normal",
                None,
            )
        });
        match result {
            Ok(()) => break,
            Err(IpcError::Full) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("send-normal", error),
        }
    }

    loop {
        let result = with_current(|current, caps, handles| {
            crate::ipc::send(
                caps,
                handles.ep_a_send.expect("send capability"),
                current as u16,
                b"transfer",
                handles.ep_b_send,
            )
        });
        match result {
            Ok(()) => break,
            Err(IpcError::Full) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("send-transfer", error),
        }
    }

    loop {
        let result = with_current(|_current, caps, handles| {
            crate::ipc::receive(caps, handles.ep_b_recv.expect("receive capability"))
        });
        match result {
            Ok(message) if message.sender == 1 && message.payload() == b"transferred" => break,
            Ok(message) => {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=ipc unexpected transfer reply sender={} len={}",
                    message.sender,
                    message.len
                );
                halt()
            }
            Err(IpcError::Empty) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("receive-b", error),
        }
    }
}

/// Worker 3 verifies that insufficient rights and forged handles are rejected,
/// then sends one valid message.
fn ipc_denied() {
    let result = with_current(|current, caps, handles| {
        crate::ipc::send(
            caps,
            handles.ep_a_recv.expect("receive-only capability"),
            current as u16,
            b"bad",
            None,
        )
    });
    match result {
        Err(IpcError::Capability(CapError::InsufficientRights)) => {
            DENIED.fetch_add(1, Ordering::Release);
        }
        other => {
            serial_line!("AETHER_BOOT_FAILED reason=ipc rights check: {other:?}");
            halt()
        }
    }

    let forged = CapHandle {
        index: 15,
        generation: 1,
    };
    let result = with_current(|current, caps, _handles| {
        crate::ipc::send(caps, forged, current as u16, b"forged", None)
    });
    match result {
        Err(IpcError::Capability(CapError::InvalidHandle)) => {
            DENIED.fetch_add(1, Ordering::Release);
        }
        other => {
            serial_line!("AETHER_BOOT_FAILED reason=ipc forged check: {other:?}");
            halt()
        }
    }

    loop {
        let result = with_current(|current, caps, handles| {
            crate::ipc::send(
                caps,
                handles.ep_a_send.expect("send capability"),
                current as u16,
                b"from3",
                None,
            )
        });
        match result {
            Ok(()) => break,
            Err(IpcError::Full) => timer::halt_until_interrupt(),
            Err(error) => ipc_fail("send-from3", error),
        }
    }
}

/// Marks the current worker finished so the scheduler stops selecting it.
fn finish_current() {
    timer::disable_interrupts();
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].state = TaskState::Done;
    DONE.fetch_add(1, Ordering::Release);
    timer::enable_interrupts();
}

/// Returns the current task index.
#[must_use]
pub fn current_task() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// Returns the current task's PML4, or `None` if CR3 is not interpretable.
#[must_use]
pub fn current_pml4() -> Option<PhysFrame> {
    paging::current_pml4()
}

/// Returns the timer tick count.
#[must_use]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Returns the number of tasks that have been initialized.
#[must_use]
pub fn task_count() -> usize {
    MAX_TASKS - 1
}

/// Returns the number of user tasks that exited normally.
#[must_use]
pub fn user_exits() -> usize {
    USER_EXITS.load(Ordering::Acquire)
}

/// Returns the number of user tasks retired for a fault.
#[must_use]
pub fn user_faults() -> usize {
    USER_FAULTS.load(Ordering::Acquire)
}

/// Returns the recorded frame-allocator pressure in MiB as `(free, total)`.
#[must_use]
pub fn mem_status() -> (usize, usize) {
    (
        MEM_FREE_MIB.load(Ordering::Acquire),
        MEM_TOTAL_MIB.load(Ordering::Acquire),
    )
}

/// Runs `f` with the current task's root scope and capability table.
///
/// Interrupts are masked so a timer tick cannot observe a half-updated table.
/// The scope is kernel-owned and must not be taken from a user payload.
pub fn with_current_caps<R>(f: impl FnOnce(&'static str, &mut CapTable) -> R) -> R {
    let _guard = timer::InterruptGuard::enter();
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    f(task.root, &mut task.caps)
}

/// Runs `f` with the current task identity and capability table.
///
/// System calls that touch IPC objects use this so the kernel, not a message
/// field, supplies the sender identity. Interrupts are masked for the
/// duration; the closure must not return to user mode while the table is
/// borrowed.
pub fn with_current_ipc<R>(f: impl FnOnce(usize, &mut CapTable) -> R) -> R {
    let _guard = timer::InterruptGuard::enter();
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    f(current, &mut task.caps)
}

/// Terminates the current user process and returns the frame to resume.
///
/// Called from the syscall dispatcher with interrupts masked by the gate.
pub fn user_exit(status: u64) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if current != USER_TASK || tasks[current].kind != TaskKind::User {
        serial_line!("AETHER_BOOT_FAILED reason=exit from non-user task");
        halt()
    }

    tasks[current].state = TaskState::Done;
    USER_EXITS.fetch_add(1, Ordering::Release);
    // Release the task's open-file slots so a completed task cannot shrink
    // the filesystem's bounded open-file pool for everyone else.
    let closed = crate::fs::close_task_files(&mut tasks[current].caps);
    let result = if status == 0 {
        UserTestResult::Passed
    } else {
        UserTestResult::Failed
    };
    USER_RESULT.store(result as usize, Ordering::Release);
    crate::apps::exited(current, status as i64);
    serial_line!("user: exit pid={} status={status} closed={closed}", current);

    schedule_next(current)
}

/// Handles a fault raised by a ring-3 task. The task is retired and the kernel
/// continues with the next runnable task.
pub fn on_user_fault(frame: *mut InterruptFrame, vector: u64) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if tasks[current].kind != TaskKind::User {
        serial_line!("AETHER_BOOT_FAILED reason=user-fault on non-user task");
        halt()
    }

    tasks[current].state = TaskState::Faulted;
    tasks[current].saved_rsp = frame as u64;
    // Release the task's open-file slots on the fault path for the same
    // reason as on exit. Only faulting-task state is touched here; unrelated
    // tasks and the kernel continue.
    let _closed = crate::fs::close_task_files(&mut tasks[current].caps);
    USER_FAULTS.fetch_add(1, Ordering::Release);
    USER_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
    crate::apps::faulted(current, vector);
    serial_line!("sched: user task {current} faulted vector={vector}");

    schedule_next(current)
}

fn schedule_next(current: usize) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let mut next = 0usize;
    for step in 1..=MAX_TASKS {
        let index = (current + step) % MAX_TASKS;
        if tasks[index].state == TaskState::Ready {
            next = index;
            break;
        }
    }
    tasks[next].state = TaskState::Running;
    CURRENT.store(next, Ordering::Relaxed);

    // The TSS RSP0 must always describe the task that is about to run: if it
    // takes an interrupt from ring 3, the CPU switches to this stack.
    // SAFETY: Interrupts are masked in the exception/syscall path, and the
    // stack belongs to the selected task.
    unsafe { gdt::set_kernel_stack(tasks[next].kernel_stack_top) };

    if let Some(pml4) = PhysFrame::from_address(tasks[next].pml4) {
        paging::activate(pml4);
    }
    tasks[next].saved_rsp as *mut InterruptFrame
}

/// Yields the current task and returns the frame that should be resumed.
///
/// The syscall gate calls this with interrupts masked. If another task is
/// ready, it runs immediately; otherwise the idle task runs until the next
/// timer tick selects this task again. `Sleep` uses the same scheduling point:
/// the prototype has no sleep queue, but it must not return without yielding.
pub fn yield_current(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].saved_rsp = frame as u64;
    if tasks[current].state == TaskState::Running {
        tasks[current].state = TaskState::Ready;
    }
    schedule_next(current)
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

    // Round-robin over ready tasks; use the idle task only when nothing else
    // is runnable. This keeps the boot tests progressing.
    let mut next = current;
    for step in 1..=MAX_TASKS {
        let index = (current + step) % MAX_TASKS;
        if tasks[index].state == TaskState::Ready {
            next = index;
            break;
        }
    }
    if next == current && tasks[current].state != TaskState::Ready {
        // No worker or user process is ready. The idle task is always
        // runnable.
        next = 0;
        tasks[next].state = TaskState::Ready;
    }
    tasks[next].state = TaskState::Running;
    CURRENT.store(next, Ordering::Relaxed);

    // SAFETY: Interrupts are masked in the timer path and the stack belongs to
    // the task selected above.
    unsafe { gdt::set_kernel_stack(tasks[next].kernel_stack_top) };

    if next != current
        && let Some(pml4) = PhysFrame::from_address(tasks[next].pml4)
    {
        paging::activate(pml4);
    }

    tasks[next].saved_rsp as *mut InterruptFrame
}

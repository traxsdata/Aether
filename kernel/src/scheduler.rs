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
use aether_abi::user::{USER_BASE, USER_LIMIT, USER_STACK_PAGES, USER_STACK_TOP, UserTestResult};
use aether_boot_common::elf;
use aether_fs::FsError;
use aether_ipc::{CapError, CapHandle, CapTable, IpcError, ObjectType, Rights};
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_memory::vma::{VmaError, VmaTable};
use aether_services::ServiceError;
use aether_services::runtime::RuntimeError;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Number of ring-0 worker tasks in the isolation test.
pub const WORKER_COUNT: usize = 3;
/// Task index of the ring-3 process.
pub const USER_TASK: usize = WORKER_COUNT + 1;
/// Task index of the Linux ABI boot test.
pub const LINUX_TASK: usize = USER_TASK + 1;
/// Task index of the dynamic Linux loader test.
pub const LINUX_DYN_TASK: usize = LINUX_TASK + 1;

const MAX_TASKS: usize = WORKER_COUNT + 12;
const STACK_SIZE: usize = 16 * 1024;
const KERNEL_STACK_SIZE: usize = 32 * 1024;
const ROUNDS: usize = 4;
const MAGIC_PREFIX: u64 = 0xa5a5_0000_0000_0000;

/// Native user program embedded by the kernel build.
static USER_ELF: &[u8] = include_bytes!(env!("AETHER_USER_ELF"));
/// Static Linux program used to exercise the Linux syscall entry.
static LINUX_ELF: &[u8] = include_bytes!(env!("AETHER_LINUX_ELF"));
/// Path of the dynamic test image inside the imported runtime rootfs.
const LINUX_DYN_PATH: &str = match option_env!("AETHER_LINUX_DYN_PATH") {
    Some(path) => path,
    None => "/usr/bin/dynamic",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Empty,
    Ready,
    Running,
    Blocked,
    Done,
    Faulted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskKind {
    Empty,
    Idle,
    Worker,
    User,
    Linux,
    Thread,
    Process,
}

/// Syscall ABI selected for a ring-3 task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskAbi {
    /// Aether's native `int 0x80` ABI.
    Aether,
    /// Linux x86-64 `syscall` ABI.
    Linux,
}

#[derive(Clone, Copy)]
enum LinuxFileSource {
    Root(usize),
    Overlay(usize),
    Directory { path: [u8; 64], len: usize },
    Pipe { index: usize, write: bool },
    EventFd { index: usize },
    Epoll { index: usize },
    Inotify,
    Pseudo(PseudoFile),
    PseudoDir(PseudoDir),
}

const PIPE_BYTES: usize = 512;
const MAX_PIPES: usize = 2;
const MAX_EVENTFDS: usize = 2;
const MAX_EPOLLS: usize = 1;
const MAX_EPOLL_WATCH: usize = 8;

/// One ready epoll entry returned to user space.
#[derive(Clone, Copy)]
pub struct LinuxEpollEvent {
    /// Requested/returned event mask.
    pub events: u32,
    /// Caller data associated with the watched descriptor.
    pub data: u64,
}

#[derive(Clone, Copy)]
struct LinuxEpoll {
    watches: [Option<(u64, u32, u64)>; MAX_EPOLL_WATCH],
}

impl LinuxEpoll {
    const EMPTY: Self = Self {
        watches: [None; MAX_EPOLL_WATCH],
    };
}

#[derive(Clone, Copy)]
struct LinuxPipe {
    data: [u8; PIPE_BYTES],
    read: usize,
    write: usize,
    len: usize,
}

impl LinuxPipe {
    const EMPTY: Self = Self {
        data: [0; PIPE_BYTES],
        read: 0,
        write: 0,
        len: 0,
    };
}

#[derive(Clone, Copy)]
struct LinuxFile {
    source: LinuxFileSource,
    offset: usize,
    /// Set for overlay descriptors opened through a write-capable `openat`.
    writable: bool,
}

/// Pseudo-file content synthesized by the kernel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PseudoFile {
    ProcExe,
    ProcMaps,
    ProcStatus,
    SyscallAudit,
    DevNull,
    DevZero,
    DevRandom,
}

/// Pseudo-directory enumerated by `getdents64`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PseudoDir {
    Proc,
    ProcSelf,
    ProcSelfFd,
    Dev,
}

const OVERLAY_FILES: usize = 4;
const OVERLAY_BYTES: usize = 512;
const OVERLAY_PATH: usize = 64;
/// Total writable-overlay bytes allowed per Linux domain.
const OVERLAY_QUOTA_BYTES: usize = 1024;

const MAX_INOTIFY_WATCHES: usize = 4;
const MAX_INOTIFY_EVENTS: usize = 8;
const INOTIFY_NAME_BYTES: usize = 64;
const IN_WATCH_PATH_BYTES: usize = 64;

#[derive(Clone, Copy)]
struct LinuxOverlay {
    path: [u8; OVERLAY_PATH],
    path_len: usize,
    data: [u8; OVERLAY_BYTES],
    len: usize,
}

#[derive(Clone, Copy)]
struct InotifyWatch {
    wd: i32,
    path: [u8; IN_WATCH_PATH_BYTES],
    path_len: usize,
    mask: u32,
}

#[derive(Clone, Copy)]
struct InotifyEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: [u8; INOTIFY_NAME_BYTES],
    name_len: usize,
}

/// One bounded inotify instance.
///
/// Instances are keyed by process (tgid), so `CLONE_VM` threads share the
/// queue and watches. `fork` snapshots the parent's instance into the child's
/// own tgid slot; neither direction observes later changes. One instance and
/// four watches per process are supported.
#[derive(Clone, Copy)]
struct LinuxInotify {
    watches: [Option<InotifyWatch>; MAX_INOTIFY_WATCHES],
    events: [Option<InotifyEvent>; MAX_INOTIFY_EVENTS],
    event_head: usize,
    event_len: usize,
    overflow: bool,
    next_wd: i32,
    blocking: bool,
}

impl LinuxInotify {
    const EMPTY: Self = Self {
        watches: [None; MAX_INOTIFY_WATCHES],
        events: [None; MAX_INOTIFY_EVENTS],
        event_head: 0,
        event_len: 0,
        overflow: false,
        next_wd: 1,
        blocking: true,
    };

    fn has_data(&self) -> bool {
        self.event_len != 0 || self.overflow
    }

    fn find_path(&self, path: &str) -> Option<usize> {
        self.watches.iter().position(|watch| {
            watch
                .as_ref()
                .is_some_and(|watch| watch_path(watch) == path)
        })
    }

    fn enqueue(&mut self, event: InotifyEvent) {
        if self.event_len == MAX_INOTIFY_EVENTS {
            self.overflow = true;
            return;
        }
        let index = (self.event_head + self.event_len) % MAX_INOTIFY_EVENTS;
        self.events[index] = Some(event);
        self.event_len += 1;
    }

    fn dequeue(&mut self) -> Option<InotifyEvent> {
        if self.event_len == 0 {
            return None;
        }
        let event = self.events[self.event_head].take();
        self.event_head = (self.event_head + 1) % MAX_INOTIFY_EVENTS;
        self.event_len -= 1;
        event
    }
}

fn watch_path(watch: &InotifyWatch) -> &str {
    core::str::from_utf8(&watch.path[..watch.path_len]).unwrap_or("")
}

impl LinuxOverlay {
    const EMPTY: Self = Self {
        path: [0; OVERLAY_PATH],
        path_len: 0,
        data: [0; OVERLAY_BYTES],
        len: 0,
    };

    fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("")
    }
}

#[derive(Clone, Copy, Default)]
struct TaskHandles {
    ep_a_send: Option<CapHandle>,
    ep_a_recv: Option<CapHandle>,
    ep_b_send: Option<CapHandle>,
    ep_b_recv: Option<CapHandle>,
}

/// Installed signal action.
#[derive(Clone, Copy)]
pub struct SigAction {
    /// Handler address, `0` for default, `1` for ignore.
    pub handler: u64,
    /// `SA_*` flags.
    pub flags: u64,
    /// Restorer trampoline.
    pub restorer: u64,
    /// Additional blocked mask.
    pub mask: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SignalFrame {
    saved: InterruptFrame,
    blocked: u64,
}

const MAX_SIGNALS: usize = 64;

struct Task {
    state: TaskState,
    kind: TaskKind,
    abi: TaskAbi,
    saved_rsp: u64,
    pml4: u64,
    kernel_stack_top: u64,
    root: &'static str,
    caps: CapTable,
    handles: TaskHandles,
    vmas: VmaTable,
    anonymous_vmas: VmaTable,
    shared_vmas: VmaTable,
    brk: u64,
    fs_base: u64,
    tgid: usize,
    pgid: usize,
    sid: usize,
    futex_address: u64,
    futex_deadline: u64,
    clear_child_tid: u64,
    linux_fds: [Option<LinuxFile>; crate::linux::MAX_FDS],
    linux_overlay: [Option<LinuxOverlay>; OVERLAY_FILES],
    inotify_wait_pointer: u64,
    inotify_wait_length: usize,
    linux_pipes: [Option<LinuxPipe>; MAX_PIPES],
    // Descriptor-local allocation map; counters live in EVENTFD_COUNTERS and
    // are addressed by (tgid, slot), so cloned threads share them.
    linux_eventfds: [Option<()>; MAX_EVENTFDS],
    linux_epolls: [Option<LinuxEpoll>; MAX_EPOLLS],
    sigactions: [Option<SigAction>; MAX_SIGNALS],
    sigblocked: u64,
    sigpending: u64,
    sigaltstack: Option<(u64, u64)>,
    parent: usize,
    exit_status: i64,
    wait_pid: i64,
    wait_status_pointer: u64,
}

impl Task {
    const fn new() -> Self {
        Self {
            state: TaskState::Empty,
            kind: TaskKind::Empty,
            abi: TaskAbi::Aether,
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
            vmas: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
            anonymous_vmas: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
            shared_vmas: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
            brk: 0,
            fs_base: 0,
            tgid: 0,
            pgid: 0,
            sid: 0,
            futex_address: 0,
            futex_deadline: 0,
            clear_child_tid: 0,
            linux_fds: [None; crate::linux::MAX_FDS],
            linux_overlay: [None; OVERLAY_FILES],
            inotify_wait_pointer: 0,
            inotify_wait_length: 0,
            linux_pipes: [None; MAX_PIPES],
            linux_eventfds: [None; MAX_EVENTFDS],
            linux_epolls: [None; MAX_EPOLLS],
            sigactions: [None; MAX_SIGNALS],
            sigblocked: 0,
            sigpending: 0,
            sigaltstack: None,
            parent: 0,
            exit_status: 0,
            wait_pid: 0,
            wait_status_pointer: 0,
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
static EVENTFD_COUNTERS: Global<[[Option<u64>; MAX_EVENTFDS]; MAX_TASKS]> =
    Global(UnsafeCell::new([[None; MAX_EVENTFDS]; MAX_TASKS]));
/// One inotify instance per process id (tgid). Threads share it; fork copies it.
static INOTIFY: Global<[Option<LinuxInotify>; MAX_TASKS]> =
    Global(UnsafeCell::new([None; MAX_TASKS]));
#[repr(align(16))]
struct WorkerStacks([[u8; STACK_SIZE]; WORKER_COUNT]);

#[repr(align(16))]
struct KernelStacks([[u8; KERNEL_STACK_SIZE]; MAX_TASKS]);

static STACKS: Global<WorkerStacks> = Global(UnsafeCell::new(WorkerStacks(
    [[0; STACK_SIZE]; WORKER_COUNT],
)));
static KERNEL_STACKS: Global<KernelStacks> = Global(UnsafeCell::new(KernelStacks(
    [[0; KERNEL_STACK_SIZE]; MAX_TASKS],
)));

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static DENIED: AtomicUsize = AtomicUsize::new(0);
static MESSAGES: AtomicUsize = AtomicUsize::new(0);
static USER_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static USER_FAULTS: AtomicUsize = AtomicUsize::new(0);
static USER_EXITS: AtomicUsize = AtomicUsize::new(0);
static LINUX_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static LINUX_EXITS: AtomicUsize = AtomicUsize::new(0);
static LINUX_FAULTS: AtomicUsize = AtomicUsize::new(0);
static LINUX_DYN_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static LINUX_DYN_EXITS: AtomicUsize = AtomicUsize::new(0);
static LINUX_DYN_FAULTS: AtomicUsize = AtomicUsize::new(0);
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
    /// The embedded Linux test program is not a valid executable.
    LinuxImage,
    /// The embedded signed Linux runtime rootfs could not be imported.
    LinuxRoot,
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
pub fn run() -> ! {
    let Some(result) = crate::memory::with_frame_allocator(|allocator| init(allocator)) else {
        serial_line!("AETHER_BOOT_FAILED reason=scheduler: frame allocator unavailable");
        halt()
    };
    if let Err(error) = result {
        serial_line!("AETHER_BOOT_FAILED reason=scheduler: {error:?}");
        halt()
    }

    timer::init();
    timer::enable_interrupts();
    serial_line!("sched: timer started at {} Hz", timer::TIMER_HZ);

    loop {
        if DONE.load(Ordering::Acquire) == WORKER_COUNT
            && USER_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
            && LINUX_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
            && LINUX_DYN_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
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
            let linux_result = LINUX_RESULT.load(Ordering::Acquire);
            if linux_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linux-test result={} faults={}",
                    linux_result,
                    LINUX_FAULTS.load(Ordering::Acquire)
                );
                halt()
            }
            let dyn_result = LINUX_DYN_RESULT.load(Ordering::Acquire);
            if dyn_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linux-dynamic-test result={} faults={}",
                    dyn_result,
                    LINUX_DYN_FAULTS.load(Ordering::Acquire)
                );
                halt()
            }
            serial_line!(
                "AETHER_LINUX_OK abi=linux exits={} faults={}",
                linux_exits(),
                LINUX_FAULTS.load(Ordering::Acquire)
            );
            serial_line!(
                "AETHER_LINUX_DYN_OK abi=linux exits={} faults={}",
                LINUX_DYN_EXITS.load(Ordering::Acquire),
                LINUX_DYN_FAULTS.load(Ordering::Acquire)
            );

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
                crate::input::poll();
                crate::desktop::tick();
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
    idle.kernel_stack_top = stack_top(&kernel_stacks.0[0]);
    tasks[0] = idle;

    for (index, stack) in stacks.0.iter_mut().enumerate() {
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
        task.kernel_stack_top = stack_top(&kernel_stacks.0[task_id]);
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
    crate::input::init(crate::graphics::width(), crate::graphics::height());
    crate::input::self_test();
    crate::desktop::self_test();
    crate::shell::init();

    // Load the native user program into its own PML4. The kernel identity map
    // is copied supervisor-only; only USER_BASE..USER_LIMIT is user-accessible.
    let image = elf::parse(USER_ELF).map_err(SchedulerError::UserImage)?;
    let user_pml4 = paging::create_user_address_space(allocator, &image, USER_ELF)?;
    let user_kernel_top = stack_top(&kernel_stacks.0[USER_TASK]);
    let user_saved_rsp =
        prepare_user_stack(user_kernel_top, image.entry(), (USER_STACK_TOP - 16) & !0xf);

    let mut user = Task::new();
    user.state = TaskState::Ready;
    user.kind = TaskKind::User;
    user.saved_rsp = user_saved_rsp;
    user.pml4 = user_pml4.start_address();
    user.kernel_stack_top = user_kernel_top;
    user.root = crate::fs::DEMO_ROOT;
    user.tgid = USER_TASK;
    tasks[USER_TASK] = user;
    crate::apps::started(USER_TASK);

    // Verify and import the embedded signed runtime rootfs before the Linux
    // task can resolve any file.
    let (root_files, root_bytes) = crate::linux::init().map_err(|_| SchedulerError::LinuxRoot)?;
    serial_line!("AETHER_LINUX_IMPORT_OK files={root_files} bytes={root_bytes}");

    // Load the static Linux test program. It is a boot gate for the Linux
    // syscall entry, not yet an application domain with its own root.
    let linux_image = elf::parse(LINUX_ELF).map_err(|_| SchedulerError::LinuxImage)?;
    let linux_pml4 = paging::create_user_address_space(allocator, &linux_image, LINUX_ELF)?;
    let linux_kernel_top = stack_top(&kernel_stacks.0[LINUX_TASK]);
    let linux_saved_rsp = prepare_user_stack(
        linux_kernel_top,
        linux_image.entry(),
        (USER_STACK_TOP - 16) & !0xf,
    );

    let mut linux = Task::new();
    linux.state = TaskState::Ready;
    linux.kind = TaskKind::Linux;
    linux.abi = TaskAbi::Linux;
    linux.saved_rsp = linux_saved_rsp;
    linux.pml4 = linux_pml4.start_address();
    linux.kernel_stack_top = linux_kernel_top;
    linux.root = crate::fs::DEMO_ROOT;
    linux.brk = paging::LINUX_ARENA_BASE;
    linux.tgid = LINUX_TASK;
    linux.pgid = LINUX_TASK;
    linux.sid = LINUX_TASK;
    tasks[LINUX_TASK] = linux;

    // Load a position-independent image through its interpreter. This proves
    // PT_INTERP resolution, auxv construction, and interpreter-to-main
    // control transfer. The interpreter is embedded for now; resolving its
    // path through the verified rootfs is follow-up work.
    let dynamic_main = crate::linux::file_data(LINUX_DYN_PATH).ok_or(SchedulerError::LinuxImage)?;
    let dynamic_image =
        aether_linux_elf::parse(dynamic_main).map_err(|_| SchedulerError::LinuxImage)?;
    let interpreter_path = dynamic_image
        .interpreter()
        .ok_or(SchedulerError::LinuxImage)?;
    let dynamic_interp =
        crate::linux::file_data(interpreter_path).ok_or(SchedulerError::LinuxImage)?;
    let dynamic = crate::linux_loader::load(
        allocator,
        dynamic_main,
        dynamic_interp,
        LINUX_DYN_PATH,
        &[LINUX_DYN_PATH],
        &["LD_LIBRARY_PATH=/lib/x86_64-linux-gnu:/lib64"],
    )
    .map_err(|_| SchedulerError::LinuxImage)?;
    let dynamic_kernel_top = stack_top(&kernel_stacks.0[LINUX_DYN_TASK]);
    let dynamic_saved_rsp =
        prepare_user_stack(dynamic_kernel_top, dynamic.entry, dynamic.stack_pointer);
    let mut dynamic_task = Task::new();
    dynamic_task.state = TaskState::Ready;
    dynamic_task.kind = TaskKind::Linux;
    dynamic_task.abi = TaskAbi::Linux;
    dynamic_task.saved_rsp = dynamic_saved_rsp;
    dynamic_task.pml4 = dynamic.pml4.start_address();
    dynamic_task.kernel_stack_top = dynamic_kernel_top;
    dynamic_task.root = crate::fs::DEMO_ROOT;
    dynamic_task.brk = paging::LINUX_ARENA_BASE;
    dynamic_task.tgid = LINUX_DYN_TASK;
    dynamic_task.pgid = LINUX_DYN_TASK;
    dynamic_task.sid = LINUX_DYN_TASK;
    let dynamic_interpreter =
        aether_linux_elf::parse(dynamic_interp).map_err(|_| SchedulerError::LinuxImage)?;
    seed_linux_vmas(
        &mut dynamic_task,
        &dynamic_image,
        crate::linux_loader::MAIN_BASE,
    );
    seed_linux_vmas(
        &mut dynamic_task,
        &dynamic_interpreter,
        crate::linux_loader::INTERP_BASE,
    );
    let _ = dynamic_task.vmas.insert_at(
        USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE,
        USER_STACK_PAGES * PAGE_SIZE,
        aether_memory::vma::PROT_READ | aether_memory::vma::PROT_WRITE,
    );
    tasks[LINUX_DYN_TASK] = dynamic_task;
    serial_line!(
        "sched: linux dynamic task ready pid={} pml4={:#018x} entry={:#018x}",
        LINUX_DYN_TASK,
        dynamic.pml4.start_address(),
        dynamic.entry
    );

    // SAFETY: Interrupts are disabled during boot and the idle task is current.
    unsafe { gdt::set_kernel_stack(tasks[0].kernel_stack_top) };
    serial_line!(
        "sched: user task ready pid={} pml4={:#018x} entry={:#018x} stack={:#018x}",
        USER_TASK,
        user_pml4.start_address(),
        image.entry(),
        USER_STACK_TOP
    );
    serial_line!(
        "sched: linux task ready pid={} pml4={:#018x} entry={:#018x}",
        LINUX_TASK,
        linux_pml4.start_address(),
        linux_image.entry()
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

fn seed_linux_vmas(task: &mut Task, image: &aether_linux_elf::Image<'_>, bias: u64) {
    for segment in image.segments() {
        let Some(start) = bias.checked_add(segment.vaddr) else {
            continue;
        };
        let Some(end) = start.checked_add(segment.memory_size) else {
            continue;
        };
        let page_start = start & !(PAGE_SIZE - 1);
        let Some(page_end) = end
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
        else {
            continue;
        };
        if page_end <= page_start {
            continue;
        }
        let mut prot = 0u32;
        if segment.flags & aether_linux_elf::PF_R != 0 {
            prot |= aether_memory::vma::PROT_READ;
        }
        if segment.flags & aether_linux_elf::PF_W != 0 {
            prot |= aether_memory::vma::PROT_WRITE;
        }
        if segment.flags & aether_linux_elf::PF_X != 0 {
            prot |= aether_memory::vma::PROT_EXEC;
        }
        // Adjacent ELF segments may share a page; the existing area already
        // covers it, so an overlap is not fatal.
        let _ = task.vmas.insert_at(page_start, page_end - page_start, prot);
    }
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
fn prepare_user_stack(kernel_stack_top: u64, entry: u64, user_rsp: u64) -> u64 {
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
            rsp: user_rsp,
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

/// Returns the number of Linux ABI tasks that exited normally.
#[must_use]
pub fn linux_exits() -> usize {
    LINUX_EXITS.load(Ordering::Acquire)
}

/// Returns the syscall ABI of the running task.
#[must_use]
pub fn current_abi() -> TaskAbi {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].abi
}

/// Creates a Linux thread sharing the caller's address space.
///
/// The child resumes at the instruction after `clone` with `RAX=0`, the
/// caller-supplied stack, and an optional TLS base. File and signal tables are
/// copied for now; true sharing is a later refinement.
#[allow(clippy::too_many_arguments)]
pub fn linux_clone(
    frame: *mut InterruptFrame,
    flags: u64,
    child_stack: u64,
    child_tid: u64,
    tls: u64,
) -> Result<usize, i64> {
    const CLONE_VM: u64 = 0x0000_0100;
    const CLONE_THREAD: u64 = 0x0001_0000;
    const CLONE_SETTLS: u64 = 0x0008_0000;
    if flags & (CLONE_VM | CLONE_THREAD) != (CLONE_VM | CLONE_THREAD) {
        return Err(LINUX_ENOSYS);
    }
    if child_stack == 0
        || !child_stack.is_multiple_of(16)
        || !(USER_BASE..USER_LIMIT).contains(&child_stack)
    {
        return Err(LINUX_EINVAL);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let (
        parent_abi,
        parent_pml4,
        parent_root,
        parent_vmas,
        parent_anonymous_vmas,
        parent_shared_vmas,
        parent_brk,
        parent_fs_base,
        parent_tgid,
        parent_fds,
        parent_overlay,
        parent_pipes,
        parent_eventfds,
        parent_epolls,
        parent_handles,
    ) = {
        let parent = &tasks[current];
        (
            parent.abi,
            parent.pml4,
            parent.root,
            parent.vmas,
            parent.anonymous_vmas,
            parent.shared_vmas,
            parent.brk,
            parent.fs_base,
            parent.tgid,
            parent.linux_fds,
            parent.linux_overlay,
            parent.linux_pipes,
            parent.linux_eventfds,
            parent.linux_epolls,
            parent.handles,
        )
    };
    let Some(slot) = tasks.iter().position(|task| task.state == TaskState::Empty) else {
        return Err(LINUX_EAGAIN);
    };
    let kernel_stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
    let kernel_top = stack_top(&kernel_stacks.0[slot]);

    let mut thread = Task::new();
    thread.state = TaskState::Ready;
    thread.kind = TaskKind::Thread;
    thread.abi = parent_abi;
    thread.saved_rsp = 0;
    thread.pml4 = parent_pml4;
    thread.kernel_stack_top = kernel_top;
    thread.root = parent_root;
    thread.vmas = parent_vmas;
    thread.anonymous_vmas = parent_anonymous_vmas;
    thread.shared_vmas = parent_shared_vmas;
    thread.brk = parent_brk;
    thread.fs_base = if flags & CLONE_SETTLS != 0 {
        tls
    } else {
        parent_fs_base
    };
    thread.tgid = parent_tgid;
    thread.pgid = tasks[current].pgid;
    thread.sid = tasks[current].sid;
    thread.futex_address = 0;
    thread.futex_deadline = 0;
    thread.clear_child_tid = child_tid;
    thread.linux_fds = parent_fds;
    thread.linux_overlay = parent_overlay;
    thread.linux_pipes = parent_pipes;
    thread.linux_eventfds = parent_eventfds;
    thread.linux_epolls = parent_epolls;
    thread.handles = parent_handles;

    let frame_size = core::mem::size_of::<InterruptFrame>() as u64;
    let frame_end = (kernel_top - 16) & !15;
    let frame_start = frame_end - frame_size;
    // SAFETY: The destination is inside the new task's private kernel stack,
    // and the source frame is live on the caller's kernel stack.
    unsafe {
        ptr::copy_nonoverlapping(
            frame as *const u8,
            frame_start as *mut u8,
            frame_size as usize,
        );
        let child_frame = &mut *(frame_start as *mut InterruptFrame);
        child_frame.rax = 0;
        child_frame.rsp = child_stack;
    }
    thread.saved_rsp = frame_start;
    tasks[slot] = thread;
    Ok(slot)
}

/// Forks the current Linux process into a fresh address space.
#[allow(clippy::too_many_arguments)]
pub fn linux_fork(frame: *mut InterruptFrame) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if tasks.iter().any(|task| {
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.tgid == tasks[current].tgid
            && !task.shared_vmas.is_empty()
    }) {
        return Err(LINUX_ENOSYS);
    }
    let parent_pml4 = tasks[current].pml4;
    let Some(slot) = tasks.iter().position(|task| task.state == TaskState::Empty) else {
        return Err(LINUX_EAGAIN);
    };
    let Some(parent_pml4) = PhysFrame::from_address(parent_pml4) else {
        return Err(LINUX_EINVAL);
    };
    let Some(child_pml4) = crate::memory::with_frame_allocator(|allocator| {
        paging::copy_user_address_space(allocator, parent_pml4)
    }) else {
        return Err(LINUX_ENOMEM);
    };
    let child_pml4 = child_pml4.map_err(|_| LINUX_ENOMEM)?;

    let (
        abi,
        root,
        vmas,
        anonymous_vmas,
        shared_vmas,
        brk,
        fs_base,
        fds,
        overlay,
        pipes,
        eventfds,
        parent_tgid,
        epolls,
        sigactions,
        sigblocked,
        sigaltstack,
    ) = {
        let parent = &tasks[current];
        (
            parent.abi,
            parent.root,
            parent.vmas,
            parent.anonymous_vmas,
            parent.shared_vmas,
            parent.brk,
            parent.fs_base,
            parent.linux_fds,
            parent.linux_overlay,
            parent.linux_pipes,
            parent.linux_eventfds,
            parent.tgid,
            parent.linux_epolls,
            parent.sigactions,
            parent.sigblocked,
            parent.sigaltstack,
        )
    };
    let kernel_stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
    let kernel_top = stack_top(&kernel_stacks.0[slot]);

    let mut child = Task::new();
    child.state = TaskState::Ready;
    child.kind = TaskKind::Process;
    child.abi = abi;
    child.pml4 = child_pml4.start_address();
    child.kernel_stack_top = kernel_top;
    child.root = root;
    child.vmas = vmas;
    child.anonymous_vmas = anonymous_vmas;
    child.shared_vmas = shared_vmas;
    child.brk = brk;
    child.fs_base = fs_base;
    child.tgid = slot;
    child.pgid = tasks[current].pgid;
    child.sid = tasks[current].sid;
    child.parent = current;
    child.linux_fds = fds;
    child.linux_overlay = overlay;
    child.linux_pipes = pipes;
    child.linux_eventfds = eventfds;
    // fork gets a process-private snapshot; threads continue to address the
    // parent's counters via their shared tgid.
    if parent_tgid < MAX_TASKS {
        let counters = unsafe { &mut *EVENTFD_COUNTERS.0.get() };
        counters[slot] = counters[parent_tgid];
        // Inotify is shared by threads but snapshotted by fork.
        let inotify = unsafe { &mut *INOTIFY.0.get() };
        inotify[slot] = inotify[parent_tgid];
    }
    child.linux_epolls = epolls;
    child.sigactions = sigactions;
    child.sigblocked = sigblocked;
    child.sigaltstack = sigaltstack;

    let frame_size = core::mem::size_of::<InterruptFrame>() as u64;
    let frame_end = (kernel_top - 16) & !15;
    let frame_start = frame_end - frame_size;
    // SAFETY: The destination is inside the new task's private kernel stack,
    // and the source frame is live on the caller's kernel stack.
    unsafe {
        ptr::copy_nonoverlapping(
            frame as *const u8,
            frame_start as *mut u8,
            frame_size as usize,
        );
        (*(frame_start as *mut InterruptFrame)).rax = 0;
    }
    child.saved_rsp = frame_start;
    tasks[slot] = child;
    Ok(slot)
}

/// Replaces the current task's address space after `execve`.
pub fn linux_replace_image(pml4: PhysFrame) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let old_pml4 = PhysFrame::from_address(task.pml4);
    if let Some(old_pml4) = old_pml4.filter(|old| *old != pml4) {
        // The replacement is already active when this is called. Release all
        // user leaf frames from the obsolete image, including ELF/stack pages
        // that are not represented by the mmap VMA table.
        let _ = crate::memory::with_frame_allocator(|allocator| {
            let _ =
                paging::unmap_user_range(allocator, old_pml4, USER_BASE, USER_LIMIT - USER_BASE);
        });
    }
    task.pml4 = pml4.start_address();
    task.vmas.reset();
    task.anonymous_vmas.reset();
    task.brk = paging::LINUX_ARENA_BASE;
    task.fs_base = 0;
}

/// Reaps an exited child of the current process.
///
/// Returns `None` when the child has not exited yet and [`LINUX_ECHILD`] when
/// there is no matching child. `pid == -1` matches any child; positive PIDs
/// match exactly. Process-group selectors are unsupported.
pub fn linux_wait4(pid: i64) -> Result<Option<(usize, i64)>, i64> {
    if pid < -1 || pid == 0 {
        return Err(LINUX_EINVAL);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let mut has_child = false;
    for (index, task) in tasks.iter_mut().enumerate() {
        if task.parent != current || index == current {
            continue;
        }
        let matches = pid == -1 || pid as usize == index;
        has_child |= matches;
        if task.state == TaskState::Done && matches {
            let status = task.exit_status;
            *task = Task::new();
            return Ok(Some((index, status)));
        }
    }
    if has_child {
        Ok(None)
    } else {
        Err(LINUX_ECHILD)
    }
}

/// Blocks the current process until a matching child exits.
pub fn linux_wait4_block(
    frame: *mut InterruptFrame,
    pid: i64,
    status_pointer: u64,
) -> *mut InterruptFrame {
    match linux_wait4(pid) {
        Err(error) => {
            unsafe { (*frame).rax = error as u64 };
            return frame;
        }
        Ok(Some((child, status))) => {
            // A child may have exited between the caller's first check and
            // entering this helper. Complete the wait instead of sleeping.
            if status_pointer != 0 {
                let status_bytes = (status as u32).to_ne_bytes();
                if !current_pml4().is_some_and(|pml4| {
                    paging::write_user_bytes(pml4, status_pointer, &status_bytes)
                }) {
                    unsafe { (*frame).rax = (-14i64) as u64 };
                    return frame;
                }
            }
            unsafe { (*frame).rax = child as u64 };
            return frame;
        }
        Ok(None) => {}
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].wait_pid = pid;
    tasks[current].wait_status_pointer = status_pointer;
    tasks[current].state = TaskState::Blocked;
    yield_current(frame)
}

fn wake_waiting_parent(child: usize) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let parent = tasks[child].parent;
    if parent >= tasks.len() || parent == child {
        return;
    }
    let wait_pid = tasks[parent].wait_pid;
    if tasks[parent].state == TaskState::Blocked && (wait_pid == -1 || wait_pid as usize == child) {
        if let Some(frame) = PhysFrame::from_address(tasks[parent].pml4)
            .map(|_| tasks[parent].saved_rsp as *mut InterruptFrame)
        {
            let status_pointer = tasks[parent].wait_status_pointer;
            let status = (tasks[child].exit_status as u32).to_ne_bytes();
            let status_ok = status_pointer == 0
                || PhysFrame::from_address(tasks[parent].pml4)
                    .is_some_and(|pml4| paging::write_user_bytes(pml4, status_pointer, &status));
            // SAFETY: saved_rsp is the syscall frame saved when the task
            // blocked in wait4; it remains on the parent's private kernel stack.
            unsafe {
                (*frame).rax = if status_ok {
                    child as u64
                } else {
                    (-14i64) as u64
                };
            }
        }
        tasks[parent].wait_pid = 0;
        tasks[parent].wait_status_pointer = 0;
        tasks[parent].state = TaskState::Ready;
        // The blocking wait has now consumed the child's exit status.
        tasks[child] = Task::new();
    }
}

/// Blocks the current Linux task on a futex word.
pub fn linux_futex_wait(
    frame: *mut InterruptFrame,
    address: u64,
    timeout_ticks: Option<u64>,
) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].futex_address = address;
    tasks[current].futex_deadline = timeout_ticks
        .and_then(|ticks| TICKS.load(Ordering::Relaxed).checked_add(ticks))
        .unwrap_or(0);
    tasks[current].state = TaskState::Blocked;
    yield_current(frame)
}

/// Wakes up to `count` tasks blocked on `address`.
pub fn linux_futex_wake(address: u64, count: u64) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let mut woken = 0u64;
    for task in tasks.iter_mut() {
        if woken >= count {
            break;
        }
        if task.state == TaskState::Blocked && task.futex_address == address {
            task.state = TaskState::Ready;
            task.futex_address = 0;
            task.futex_deadline = 0;
            woken += 1;
        }
    }
    woken as i64
}

/// Installs a signal action for the current task.
pub fn linux_set_sigaction(signum: usize, action: SigAction) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if signum > 0 && signum <= MAX_SIGNALS {
        task.sigactions[signum - 1] = Some(action);
    }
}

/// Returns the current signal action, if installed.
#[must_use]
pub fn linux_get_sigaction(signum: usize) -> Option<SigAction> {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)]
        .sigactions
        .get(signum.saturating_sub(1))
        .copied()
        .flatten()
}

/// Replaces the current signal mask and returns the previous mask.
pub fn linux_set_sigmask(mask: u64) -> u64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let old = task.sigblocked;
    task.sigblocked = mask;
    old
}

/// Returns the current signal mask.
#[must_use]
pub fn linux_sigmask() -> u64 {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].sigblocked
}

/// Sets the alternate signal stack for the current task.
pub fn linux_set_altstack(address: u64, size: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].sigaltstack = Some((address, size));
}

/// Returns the alternate signal stack for the current task.
#[must_use]
pub fn linux_altstack() -> Option<(u64, u64)> {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].sigaltstack
}

/// Queues a signal on the current thread group.
pub fn linux_send_signal(signum: u64) -> i64 {
    if signum == 0 || signum > MAX_SIGNALS as u64 {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let tgid = tasks[current].tgid;
    for task in tasks.iter_mut() {
        if task.tgid == tgid && task.abi == TaskAbi::Linux {
            task.sigpending |= 1u64 << (signum - 1);
        }
    }
    0
}

/// Sends a signal to one process or a specific thread in that process.
/// Signal zero performs only the existence check.
pub fn linux_send_signal_to(tgid: usize, tid: Option<usize>, signum: u64) -> i64 {
    if signum > MAX_SIGNALS as u64 {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let session = tasks[current].sid;
    let targets: [bool; MAX_TASKS] = core::array::from_fn(|index| {
        let task = &tasks[index];
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.sid == session
            && task.tgid == tgid
            && tid.is_none_or(|tid| tid == index)
    });
    if !targets.iter().any(|target| *target) {
        return LINUX_ESRCH;
    }
    if signum == 0 {
        return 0;
    }
    for (index, target) in targets.into_iter().enumerate() {
        if !target {
            continue;
        }
        let task = &mut tasks[index];
        task.sigpending |= 1u64 << (signum - 1);
        if task.state == TaskState::Blocked {
            if task.wait_pid != 0 {
                task.wait_pid = 0;
                task.wait_status_pointer = 0;
            }
            task.inotify_wait_pointer = 0;
            task.inotify_wait_length = 0;
            task.inotify_wait_pointer = 0;
            task.inotify_wait_length = 0;
            // A queued signal interrupts blocking syscall waits. The handler
            // is delivered from the normal timer path.
            unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-4i64) as u64 };
            task.futex_address = 0;
            task.futex_deadline = 0;
            task.state = TaskState::Ready;
        }
    }
    0
}

/// Delivers the lowest pending unblocked signal, if a handler is installed.
pub fn linux_deliver_signal(frame: *mut InterruptFrame) -> bool {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    if task.abi != TaskAbi::Linux || frame.is_null() {
        return false;
    }
    let pending = task.sigpending & !task.sigblocked;
    if pending == 0 {
        return false;
    }
    let signum = pending.trailing_zeros() as usize + 1;
    let Some(action) = task.sigactions[signum - 1] else {
        return false;
    };
    if action.handler == 0 || action.restorer == 0 {
        return false;
    }
    task.sigpending &= !(1u64 << (signum - 1));
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return false;
    };
    let saved = unsafe { *frame };
    if saved.cs & 3 != 3 {
        return false;
    }
    let size = core::mem::size_of::<SignalFrame>() as u64;
    let Some(frame_pointer) = saved.rsp.checked_sub(size).map(|value| value & !15) else {
        return false;
    };
    let Some(restorer_pointer) = frame_pointer.checked_sub(8) else {
        return false;
    };
    if frame_pointer < USER_BASE
        || !paging::user_range_valid(pml4, restorer_pointer, size + 8, true)
    {
        return false;
    }
    let signal_frame = SignalFrame {
        saved,
        blocked: task.sigblocked,
    };
    // SAFETY: The user range was validated as writable and the syscall or
    // interrupt path keeps the task from running concurrently.
    unsafe {
        ptr::write(frame_pointer as *mut SignalFrame, signal_frame);
        ptr::write(restorer_pointer as *mut u64, action.restorer);
        (*frame).rax = 0;
        (*frame).rdi = signum as u64;
        (*frame).rsi = 0;
        (*frame).rdx = 0;
        (*frame).rip = action.handler;
        (*frame).rsp = restorer_pointer;
    }
    task.sigblocked |= action.mask;
    true
}

/// Consumes a signal frame and restores the interrupted context.
pub fn linux_sigreturn(frame: *mut InterruptFrame) -> bool {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return false;
    };
    // The handler popped the restorer address; RSP now points at the saved
    // signal frame.
    let frame_pointer = unsafe { (*frame).rsp };
    let size = core::mem::size_of::<SignalFrame>() as u64;
    if !paging::user_range_valid(pml4, frame_pointer, size, false) {
        return false;
    }
    // SAFETY: The saved frame was validated as readable.
    let saved = unsafe { ptr::read(frame_pointer as *const SignalFrame) };
    // SAFETY: The caller supplied the live interrupt frame.
    unsafe { ptr::write(frame, saved.saved) };
    task.sigblocked = saved.blocked;
    true
}

/// Queues and delivers a fault signal to the current task.
pub fn linux_deliver_fault(frame: *mut InterruptFrame, signum: u64) -> bool {
    let _ = linux_send_signal(signum);
    linux_deliver_signal(frame)
}

/// Returns the thread-group id for `getpid`.
#[must_use]
pub fn linux_getpid() -> usize {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].tgid
}

/// Current process group/session IDs. IDs are process leaders' task slots.
pub fn linux_getpgid(pid: usize) -> Result<usize, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let target = if pid == 0 { tasks[current].tgid } else { pid };
    let Some(task) = tasks.iter().find(|task| {
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.tgid == target
            && task.kind != TaskKind::Thread
    }) else {
        return Err(LINUX_ESRCH);
    };
    if task.sid != tasks[current].sid {
        return Err(LINUX_EPERM);
    }
    Ok(task.pgid)
}

/// Restricted Linux setpgid: current process or its direct child, same session,
/// and only the caller's existing group or a new group led by the target.
pub fn linux_setpgid(pid: usize, pgid: usize) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let self_tgid = tasks[current].tgid;
    let target = if pid == 0 { self_tgid } else { pid };
    let Some(index) = tasks.iter().position(|task| {
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.tgid == target
            && task.kind != TaskKind::Thread
    }) else {
        return LINUX_ESRCH;
    };
    if target != self_tgid && tasks[index].parent != current {
        return LINUX_EPERM;
    }
    if tasks[index].sid != tasks[current].sid || tasks[index].sid == target {
        return LINUX_EPERM;
    }
    let group = if pgid == 0 { target } else { pgid };
    if group != target
        && !tasks.iter().any(|task| {
            task.state != TaskState::Empty
                && task.state != TaskState::Done
                && task.abi == TaskAbi::Linux
                && task.pgid == group
                && task.sid == tasks[current].sid
        })
    {
        return LINUX_EPERM;
    }
    for task in tasks
        .iter_mut()
        .filter(|task| task.state != TaskState::Empty && task.tgid == target)
    {
        task.pgid = group;
    }
    0
}

/// Starts a new session for the caller, unless it is already a process-group leader.
pub fn linux_setsid() -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let tgid = tasks[current].tgid;
    if tasks[current].pgid == tgid {
        return LINUX_EPERM;
    }
    for task in tasks
        .iter_mut()
        .filter(|task| task.state != TaskState::Empty && task.tgid == tgid)
    {
        task.sid = tgid;
        task.pgid = tgid;
    }
    tgid as i64
}

/// Signal members of a process group, preserving per-process thread-group delivery.
pub fn linux_send_signal_group(pgid: usize, signal: u64) -> i64 {
    if signal > MAX_SIGNALS as u64 {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let session = tasks[current].sid;
    let targets: [bool; MAX_TASKS] = core::array::from_fn(|index| {
        let task = &tasks[index];
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.sid == session
            && task.pgid == pgid
            && task.kind != TaskKind::Thread
    });
    if !targets.iter().any(|target| *target) {
        return LINUX_ESRCH;
    }
    let tgids: [usize; MAX_TASKS] =
        core::array::from_fn(|index| if targets[index] { tasks[index].tgid } else { 0 });
    for task in tasks.iter_mut() {
        if task.state == TaskState::Empty
            || task.state == TaskState::Done
            || !tgids.contains(&task.tgid)
            || task.tgid == 0
        {
            continue;
        }
        if signal == 0 {
            continue;
        }
        task.sigpending |= 1u64 << (signal - 1);
        if task.state == TaskState::Blocked {
            if task.wait_pid != 0 {
                task.wait_pid = 0;
                task.wait_status_pointer = 0;
            }
            task.inotify_wait_pointer = 0;
            task.inotify_wait_length = 0;
            unsafe {
                (*(task.saved_rsp as *mut InterruptFrame)).rax = (-4i64) as u64;
            }
            task.futex_address = 0;
            task.futex_deadline = 0;
            task.state = TaskState::Ready;
        }
    }
    0
}

/// Clears a thread's `CLONE_CHILD_CLEARTID` word and wakes a joiner.
fn clear_child_tid(index: usize) {
    let (pointer, pml4) = {
        let tasks = unsafe { &*TASKS.0.get() };
        (tasks[index].clear_child_tid, tasks[index].pml4)
    };
    if pointer == 0 || pointer >= USER_LIMIT || !pointer.is_multiple_of(4) {
        return;
    }
    let Some(pml4) = PhysFrame::from_address(pml4) else {
        return;
    };
    if !paging::user_range_valid(pml4, pointer, 4, true) {
        return;
    }
    // SAFETY: The four-byte user range was validated as writable above.
    unsafe { ptr::write(pointer as *mut u32, 0) };
    let _ = linux_futex_wake(pointer, 1);
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

const LINUX_EBADF: i64 = -9;
const LINUX_EACCES: i64 = -13;
const LINUX_ECHILD: i64 = -10;
const LINUX_EAGAIN: i64 = -11;
const LINUX_ENOMEM: i64 = -12;
const LINUX_ENOSYS: i64 = -38;
const LINUX_ENOTDIR: i64 = -20;
const LINUX_EISDIR: i64 = -21;
const LINUX_EINVAL: i64 = -22;
const LINUX_EMFILE: i64 = -24;
const LINUX_ENOENT: i64 = -2;
const LINUX_ESRCH: i64 = -3;
const LINUX_EPERM: i64 = -1;
const LINUX_EROFS: i64 = -30;
const LINUX_ENAMETOOLONG: i64 = -36;
const LINUX_ESPIPE: i64 = -29;
const LINUX_ENOSPC: i64 = -28;

/// Opens a file from the read-only Linux root and returns a file descriptor.
pub fn linux_open(path: &str) -> Result<u64, i64> {
    let Some(file) = crate::linux::lookup(path) else {
        return Err(LINUX_ENOENT);
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Root(file),
                offset: 0,
                writable: false,
            });
            linux_inotify_emit(path, IN_OPEN, false);
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Maps a synthetic runtime path to a pseudo-file kind.
fn pseudo_file(path: &str) -> Option<PseudoFile> {
    match path {
        "/proc/self/exe" | "/proc/thread-self/exe" => Some(PseudoFile::ProcExe),
        "/proc/self/maps" => Some(PseudoFile::ProcMaps),
        "/proc/self/status" => Some(PseudoFile::ProcStatus),
        "/proc/aether/syscalls" => Some(PseudoFile::SyscallAudit),
        "/dev/null" => Some(PseudoFile::DevNull),
        "/dev/zero" => Some(PseudoFile::DevZero),
        "/dev/random" | "/dev/urandom" => Some(PseudoFile::DevRandom),
        _ => None,
    }
}

/// Maps a synthetic runtime path to a pseudo-directory kind.
fn pseudo_dir(path: &str) -> Option<PseudoDir> {
    match path {
        "/proc" => Some(PseudoDir::Proc),
        "/proc/self" | "/proc/thread-self" => Some(PseudoDir::ProcSelf),
        "/proc/self/fd" | "/proc/thread-self/fd" => Some(PseudoDir::ProcSelfFd),
        "/dev" => Some(PseudoDir::Dev),
        _ => None,
    }
}

fn pseudo_stat(kind: PseudoFile) -> (u64, u64, u32) {
    match kind {
        PseudoFile::ProcExe => (LINUX_ELF.len() as u64, 3, 0o100555),
        PseudoFile::DevNull | PseudoFile::DevZero => (0, 4, 0o020666),
        PseudoFile::DevRandom => (0, 5, 0o020666),
        PseudoFile::ProcMaps | PseudoFile::ProcStatus | PseudoFile::SyscallAudit => {
            (0, 6, 0o100444)
        }
    }
}

/// Opens a read-only pseudo-file or pseudo-directory.
pub fn linux_open_pseudo(path: &str, writable: bool) -> Result<u64, i64> {
    let source = if let Some(kind) = pseudo_file(path) {
        if writable && kind != PseudoFile::DevNull {
            return Err(LINUX_EACCES);
        }
        LinuxFileSource::Pseudo(kind)
    } else if let Some(kind) = pseudo_dir(path) {
        if writable {
            return Err(LINUX_EACCES);
        }
        LinuxFileSource::PseudoDir(kind)
    } else {
        return Err(LINUX_ENOENT);
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source,
                offset: 0,
                writable: false,
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Opens a read-only file, preferring an existing overlay entry.
pub fn linux_open_read(path: &str) -> Result<u64, i64> {
    if pseudo_file(path).is_some() || pseudo_dir(path).is_some() {
        return linux_open_pseudo(path, false);
    }
    if path.starts_with("/tmp/")
        && let Ok(fd) = linux_overlay_open(path, false, false, false)
    {
        return Ok(fd);
    }
    if crate::linux::is_dir(path) {
        return linux_open_directory(path);
    }
    linux_open(path)
}

/// Opens a directory descriptor for `getdents64`.
pub fn linux_open_directory(path: &str) -> Result<u64, i64> {
    if path.len() > 64 || !(crate::linux::is_dir(path) || pseudo_dir(path).is_some()) {
        return Err(LINUX_ENOENT);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    let mut directory = [0u8; 64];
    directory[..path.len()].copy_from_slice(path.as_bytes());
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Directory {
                    path: directory,
                    len: path.len(),
                },
                offset: 0,
                writable: false,
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Fills `out` with Linux `dirent64` records from a directory descriptor.
///
/// The returned count is bytes written; the descriptor cursor advances by the
/// number of entries consumed. Entry names are bounded by the imported tree.
fn pseudo_dir_entry(
    directory: PseudoDir,
    index: usize,
    fd_mask: u16,
) -> Option<([u8; 16], usize, u64, u8)> {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    const DT_LNK: u8 = 10;
    let mut name = [0u8; 16];
    let (text, kind): (&str, u8) = match (directory, index) {
        (_, 0) => (".", DT_DIR),
        (_, 1) => ("..", DT_DIR),
        (PseudoDir::Proc, 2) => ("self", DT_DIR),
        (PseudoDir::Proc, 3) => ("aether", DT_DIR),
        (PseudoDir::ProcSelf, 2) => ("exe", DT_LNK),
        (PseudoDir::ProcSelf, 3) => ("maps", DT_REG),
        (PseudoDir::ProcSelf, 4) => ("status", DT_REG),
        (PseudoDir::ProcSelf, 5) => ("fd", DT_DIR),
        (PseudoDir::ProcSelf, 6) => ("cwd", DT_LNK),
        (PseudoDir::Dev, 2) => ("null", DT_REG),
        (PseudoDir::Dev, 3) => ("zero", DT_REG),
        (PseudoDir::Dev, 4) => ("random", DT_REG),
        (PseudoDir::Dev, 5) => ("urandom", DT_REG),
        (PseudoDir::ProcSelfFd, _) => {
            let descriptor = index - 2;
            if descriptor >= 3
                && (descriptor >= crate::linux::MAX_FDS || fd_mask & (1 << descriptor) == 0)
            {
                return None;
            }
            let mut value = descriptor;
            let mut digits = [0u8; 8];
            let mut length = 0usize;
            loop {
                digits[length] = b'0' + (value % 10) as u8;
                length += 1;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            for position in 0..length {
                name[position] = digits[length - position - 1];
            }
            return Some((name, length, 10 + descriptor as u64, DT_LNK));
        }
        _ => return None,
    };
    let length = text.len().min(name.len());
    name[..length].copy_from_slice(&text.as_bytes()[..length]);
    Some((name, length, 10 + index as u64, kind))
}

pub fn linux_getdents(fd: u64, out: &mut [u8]) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let fd_mask = task
        .linux_fds
        .iter()
        .enumerate()
        .fold(0u16, |mask, (index, slot)| {
            if slot.is_some() && index < 16 {
                mask | (1 << index)
            } else {
                mask
            }
        });
    let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    let mut written = 0usize;
    if let LinuxFileSource::Directory { path, len } = slot.source {
        let path = core::str::from_utf8(&path[..len]).map_err(|_| LINUX_EBADF)?;
        while let Some((name, inode, kind)) = crate::linux::dir_entry(path, slot.offset) {
            let record_length = (19 + name.len() + 1 + 7) & !7;
            if written + record_length > out.len() {
                break;
            }
            let record = &mut out[written..written + record_length];
            record.fill(0);
            record[..8].copy_from_slice(&inode.to_le_bytes());
            record[8..16].copy_from_slice(&((slot.offset + 1) as i64).to_le_bytes());
            record[16..18].copy_from_slice(&(record_length as u16).to_le_bytes());
            record[18] = kind;
            record[19..19 + name.len()].copy_from_slice(name.as_bytes());
            written += record_length;
            slot.offset += 1;
        }
        return Ok(written);
    }
    let LinuxFileSource::PseudoDir(directory) = slot.source else {
        return Err(LINUX_ENOTDIR);
    };
    while let Some((name, name_length, inode, kind)) =
        pseudo_dir_entry(directory, slot.offset, fd_mask)
    {
        let record_length = (19 + name_length + 1 + 7) & !7;
        if written + record_length > out.len() {
            break;
        }
        let record = &mut out[written..written + record_length];
        record.fill(0);
        record[..8].copy_from_slice(&inode.to_le_bytes());
        record[8..16].copy_from_slice(&((slot.offset + 1) as i64).to_le_bytes());
        record[16..18].copy_from_slice(&(record_length as u16).to_le_bytes());
        record[18] = kind;
        record[19..19 + name_length].copy_from_slice(&name[..name_length]);
        written += record_length;
        slot.offset += 1;
    }
    Ok(written)
}

/// Opens or creates a file in the task's private writable overlay.
///
/// The overlay is deliberately separate from the read-only root. Only `/tmp`
/// paths are accepted, entries are bounded, and the overlay disappears with the
/// task. This is the prototype of the per-application writable layer.
pub fn linux_overlay_open(
    path: &str,
    create: bool,
    truncate: bool,
    writable: bool,
) -> Result<u64, i64> {
    if !path.starts_with("/tmp/") || !crate::linux::valid_path(path) {
        return Err(LINUX_EROFS);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    let mut created = false;
    let mut truncated = false;
    let mut overlay_index = None;
    for (index, entry) in task.linux_overlay.iter().enumerate() {
        if entry.as_ref().is_some_and(|entry| entry.path() == path) {
            overlay_index = Some(index);
            break;
        }
    }
    if overlay_index.is_none() {
        if !create {
            return Err(LINUX_ENOENT);
        }
        if path.len() > OVERLAY_PATH {
            return Err(LINUX_ENAMETOOLONG);
        }
        let index = task
            .linux_overlay
            .iter()
            .position(Option::is_none)
            .ok_or(LINUX_ENOSPC)?;
        let mut entry = LinuxOverlay::EMPTY;
        entry.path[..path.len()].copy_from_slice(path.as_bytes());
        entry.path_len = path.len();
        task.linux_overlay[index] = Some(entry);
        overlay_index = Some(index);
        created = true;
    }
    let index = overlay_index.unwrap();
    if truncate && let Some(entry) = task.linux_overlay[index].as_mut() {
        truncated = entry.len != 0;
        entry.len = 0;
    }
    let mut assigned = None;
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Overlay(index),
                offset: 0,
                writable,
            });
            assigned = Some(descriptor as u64);
            break;
        }
    }
    let Some(fd) = assigned else {
        return Err(LINUX_EMFILE);
    };
    if created {
        linux_inotify_emit(path, IN_CREATE, false);
    }
    linux_inotify_emit(path, IN_OPEN, false);
    if truncated {
        linux_inotify_emit(path, IN_ATTRIB, false);
        linux_inotify_emit(path, IN_MODIFY, false);
    }
    Ok(fd)
}

/// Creates a bounded pipe and returns its read and write descriptors.
pub fn linux_pipe() -> Result<(u64, u64), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(index) = task.linux_pipes.iter().position(|pipe| pipe.is_none()) else {
        return Err(LINUX_EMFILE);
    };
    task.linux_pipes[index] = Some(LinuxPipe::EMPTY);
    let mut read_fd = None;
    let mut write_fd = None;
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_some() {
            continue;
        }
        if read_fd.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Pipe {
                    index,
                    write: false,
                },
                offset: 0,
                writable: false,
            });
            read_fd = Some(descriptor as u64);
        } else if write_fd.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Pipe { index, write: true },
                offset: 0,
                writable: true,
            });
            write_fd = Some(descriptor as u64);
            break;
        }
    }
    match (read_fd, write_fd) {
        (Some(read), Some(write)) => Ok((read, write)),
        _ => {
            for slot in task.linux_fds.iter_mut().skip(3) {
                if let Some(file) = *slot
                    && matches!(file.source, LinuxFileSource::Pipe { index: i, .. } if i == index)
                {
                    *slot = None;
                }
            }
            task.linux_pipes[index] = None;
            Err(LINUX_EMFILE)
        }
    }
}

/// Creates an eventfd with the given initial counter.
pub fn linux_eventfd(initial: u64) -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(index) = task
        .linux_eventfds
        .iter()
        .position(|eventfd| eventfd.is_none())
    else {
        return Err(LINUX_EMFILE);
    };
    task.linux_eventfds[index] = Some(());
    let tgid = task.tgid;
    if tgid >= MAX_TASKS {
        task.linux_eventfds[index] = None;
        return Err(LINUX_EMFILE);
    }
    unsafe { (*EVENTFD_COUNTERS.0.get())[tgid][index] = Some(initial) };
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::EventFd { index },
                offset: 0,
                writable: true,
            });
            return Ok(descriptor as u64);
        }
    }
    task.linux_eventfds[index] = None;
    unsafe { (*EVENTFD_COUNTERS.0.get())[tgid][index] = None };
    Err(LINUX_EMFILE)
}

/// Creates an epoll instance.
pub fn linux_epoll_create() -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(index) = task.linux_epolls.iter().position(|epoll| epoll.is_none()) else {
        return Err(LINUX_EMFILE);
    };
    task.linux_epolls[index] = Some(LinuxEpoll::EMPTY);
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Epoll { index },
                offset: 0,
                writable: false,
            });
            return Ok(descriptor as u64);
        }
    }
    task.linux_epolls[index] = None;
    Err(LINUX_EMFILE)
}

/// Adds, modifies, or removes an epoll watch.
pub fn linux_epoll_ctl(
    epoll_fd: u64,
    operation: u64,
    target_fd: u64,
    events: u32,
    data: u64,
) -> Result<(), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task
        .linux_fds
        .get(epoll_fd as usize)
        .and_then(Option::as_ref)
    else {
        return Err(LINUX_EBADF);
    };
    let LinuxFileSource::Epoll { index } = slot.source else {
        return Err(LINUX_EINVAL);
    };
    if task
        .linux_fds
        .get(target_fd as usize)
        .and_then(Option::as_ref)
        .is_none()
    {
        return Err(LINUX_EBADF);
    }
    let Some(epoll) = task.linux_epolls.get_mut(index).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    match operation {
        1 | 3 => {
            if let Some(entry) = epoll
                .watches
                .iter_mut()
                .find(|entry| entry.is_some_and(|(fd, _, _)| fd == target_fd))
            {
                *entry = Some((target_fd, events, data));
            } else {
                let Some(entry) = epoll.watches.iter_mut().find(|entry| entry.is_none()) else {
                    return Err(LINUX_ENOSPC);
                };
                *entry = Some((target_fd, events, data));
            }
        }
        2 => {
            for entry in epoll.watches.iter_mut() {
                if entry.is_some_and(|(fd, _, _)| fd == target_fd) {
                    *entry = None;
                }
            }
        }
        _ => return Err(LINUX_EINVAL),
    }
    Ok(())
}

/// Collects ready epoll entries without blocking.
pub fn linux_epoll_wait(epoll_fd: u64, out: &mut [LinuxEpollEvent]) -> Result<usize, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task
        .linux_fds
        .get(epoll_fd as usize)
        .and_then(Option::as_ref)
    else {
        return Err(LINUX_EBADF);
    };
    let LinuxFileSource::Epoll { index } = slot.source else {
        return Err(LINUX_EINVAL);
    };
    let Some(epoll) = task.linux_epolls.get(index).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    let mut count = 0usize;
    for watch in epoll.watches.iter().flatten() {
        if count == out.len() {
            break;
        }
        let (fd, events, data) = *watch;
        let ready = linux_poll(fd, events as i16);
        if ready != 0 {
            out[count] = LinuxEpollEvent {
                events: ready as u32,
                data,
            };
            count += 1;
        }
    }
    Ok(count)
}

/// Returns poll readiness bits for a Linux descriptor.
pub fn linux_poll(fd: u64, events: i16) -> i16 {
    const POLLIN: i16 = 1;
    const POLLOUT: i16 = 4;
    const POLLNVAL: i16 = 0x20;
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return POLLNVAL;
    };
    let mut ready = 0i16;
    match slot.source {
        LinuxFileSource::Pipe { index, .. } => {
            let Some(pipe) = task.linux_pipes.get(index).and_then(Option::as_ref) else {
                return POLLNVAL;
            };
            if events & POLLIN != 0 && pipe.len > 0 {
                ready |= POLLIN;
            }
            if events & POLLOUT != 0 && pipe.len < PIPE_BYTES {
                ready |= POLLOUT;
            }
        }
        LinuxFileSource::Root(_)
        | LinuxFileSource::Overlay(_)
        | LinuxFileSource::Directory { .. } => {
            ready = events & (POLLIN | POLLOUT);
        }
        LinuxFileSource::EventFd { index } => {
            let counter = if task.tgid < MAX_TASKS {
                unsafe { (*EVENTFD_COUNTERS.0.get())[task.tgid][index].unwrap_or(0) }
            } else {
                0
            };
            if events & POLLIN != 0 && counter > 0 {
                ready |= POLLIN;
            }
            if events & POLLOUT != 0 && counter < u64::MAX {
                ready |= POLLOUT;
            }
        }
        LinuxFileSource::Epoll { .. } => {
            ready = events & POLLIN;
        }
        LinuxFileSource::Inotify => {
            if events & POLLIN != 0
                && inotify_state(task.tgid).is_some_and(|state| state.has_data())
            {
                ready |= POLLIN;
            }
        }
        LinuxFileSource::Pseudo(_) | LinuxFileSource::PseudoDir(_) => {
            ready = events & POLLIN;
        }
    }
    ready
}

struct PseudoText {
    buffer: [u8; 4096],
    length: usize,
}

impl core::fmt::Write for PseudoText {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let end = self
            .length
            .checked_add(value.len())
            .ok_or(core::fmt::Error)?;
        if end > self.buffer.len() {
            return Err(core::fmt::Error);
        }
        self.buffer[self.length..end].copy_from_slice(value.as_bytes());
        self.length = end;
        Ok(())
    }
}

fn proc_maps_text(vmas: &aether_memory::vma::VmaTable) -> PseudoText {
    use core::fmt::Write as _;
    let mut text = PseudoText {
        buffer: [0; 4096],
        length: 0,
    };
    for vma in vmas.iter() {
        let read = if vma.prot & aether_memory::vma::PROT_READ != 0 {
            'r'
        } else {
            '-'
        };
        let write = if vma.prot & aether_memory::vma::PROT_WRITE != 0 {
            'w'
        } else {
            '-'
        };
        let exec = if vma.prot & aether_memory::vma::PROT_EXEC != 0 {
            'x'
        } else {
            '-'
        };
        let _ = writeln!(
            text,
            "{:016x}-{:016x} {read}{write}{exec}p 00000000 00:00 0",
            vma.start, vma.end
        );
    }
    text
}

fn proc_status_text(vmas: &aether_memory::vma::VmaTable, tgid: usize, parent: usize) -> PseudoText {
    use core::fmt::Write as _;
    let mut text = PseudoText {
        buffer: [0; 4096],
        length: 0,
    };
    let memory: u64 = vmas.iter().map(|vma| vma.size()).sum();
    let _ = writeln!(text, "Name:\taether-linux");
    let _ = writeln!(text, "State:\tR (running)");
    let _ = writeln!(text, "Tgid:\t{tgid}");
    let _ = writeln!(text, "Pid:\t{tgid}");
    let _ = writeln!(text, "PPid:\t{parent}");
    let _ = writeln!(text, "Uid:\t1000\t1000\t1000\t1000");
    let _ = writeln!(text, "Gid:\t1000\t1000\t1000\t1000");
    let _ = writeln!(text, "VmSize:\t{} kB", memory / 1024);
    let _ = writeln!(text, "VmRSS:\t{} kB", memory / 1024);
    text
}

fn pseudo_read(
    kind: PseudoFile,
    offset: usize,
    out: &mut [u8],
    vmas: &aether_memory::vma::VmaTable,
    tgid: usize,
    parent: usize,
) -> Result<usize, i64> {
    if kind == PseudoFile::DevNull {
        return Ok(0);
    }
    if kind == PseudoFile::DevZero {
        out.fill(0);
        return Ok(out.len());
    }
    if kind == PseudoFile::DevRandom {
        let mut state = TICKS
            .load(Ordering::Relaxed)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(tgid as u64)
            | 1;
        for byte in out.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = state as u8;
        }
        return Ok(out.len());
    }
    if kind == PseudoFile::SyscallAudit {
        return Ok(crate::linux_policy::audit_text(offset, out));
    }
    let text = match kind {
        PseudoFile::ProcExe => None,
        PseudoFile::ProcMaps => Some(proc_maps_text(vmas)),
        PseudoFile::ProcStatus => Some(proc_status_text(vmas, tgid, parent)),
        _ => None,
    };
    if let Some(text) = text.as_ref() {
        if offset >= text.length {
            return Ok(0);
        }
        let length = (text.length - offset).min(out.len());
        out[..length].copy_from_slice(&text.buffer[offset..offset + length]);
        return Ok(length);
    }
    if offset >= LINUX_ELF.len() {
        return Ok(0);
    }
    let length = (LINUX_ELF.len() - offset).min(out.len());
    out[..length].copy_from_slice(&LINUX_ELF[offset..offset + length]);
    Ok(length)
}

/// Reads from a Linux file descriptor into a validated user buffer.
pub fn linux_read(fd: u64, out: &mut [u8]) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    match slot.source {
        LinuxFileSource::Root(file) => {
            let Some(file) = crate::linux::file(file) else {
                return Err(LINUX_EBADF);
            };
            let remaining = file.data.len().saturating_sub(slot.offset);
            let length = remaining.min(out.len());
            out[..length].copy_from_slice(&file.data[slot.offset..slot.offset + length]);
            slot.offset += length;
            Ok(length)
        }
        LinuxFileSource::Overlay(index) => {
            let Some(entry) = task.linux_overlay.get(index).and_then(Option::as_ref) else {
                return Err(LINUX_EBADF);
            };
            let remaining = entry.len.saturating_sub(slot.offset);
            let length = remaining.min(out.len());
            out[..length].copy_from_slice(&entry.data[slot.offset..slot.offset + length]);
            slot.offset += length;
            Ok(length)
        }
        LinuxFileSource::Directory { .. } => Err(LINUX_EISDIR),
        LinuxFileSource::Pipe {
            index,
            write: false,
        } => {
            let Some(pipe) = task.linux_pipes.get_mut(index).and_then(Option::as_mut) else {
                return Err(LINUX_EBADF);
            };
            if pipe.len == 0 {
                return Err(LINUX_EAGAIN);
            }
            let length = pipe.len.min(out.len());
            for (offset, byte) in out.iter_mut().take(length).enumerate() {
                *byte = pipe.data[(pipe.read + offset) % PIPE_BYTES];
            }
            pipe.read = (pipe.read + length) % PIPE_BYTES;
            pipe.len -= length;
            Ok(length)
        }
        LinuxFileSource::Pipe { write: true, .. } => Err(LINUX_EBADF),
        LinuxFileSource::EventFd { index } => {
            if !task.linux_eventfds.get(index).is_some_and(Option::is_some)
                || task.tgid >= MAX_TASKS
            {
                return Err(LINUX_EBADF);
            }
            let Some(counter) =
                (unsafe { &mut (*EVENTFD_COUNTERS.0.get())[task.tgid][index] }).as_mut()
            else {
                return Err(LINUX_EBADF);
            };
            if *counter == 0 {
                return Err(LINUX_EAGAIN);
            }
            if out.len() < 8 {
                return Err(LINUX_EINVAL);
            }
            let value = *counter;
            *counter = 0;
            out[..8].copy_from_slice(&value.to_le_bytes());
            Ok(8)
        }
        LinuxFileSource::Epoll { .. } => Err(LINUX_EINVAL),
        LinuxFileSource::Pseudo(kind) => {
            let count = pseudo_read(kind, slot.offset, out, &task.vmas, task.tgid, task.parent)?;
            slot.offset = slot.offset.saturating_add(count);
            Ok(count)
        }
        LinuxFileSource::PseudoDir(_) => Err(LINUX_EISDIR),
        LinuxFileSource::Inotify => {
            let tgid = task.tgid;
            let state = inotify_state(tgid).ok_or(LINUX_EBADF)?;
            if !state.has_data() {
                return Err(LINUX_EAGAIN);
            }
            inotify_pack(state, out)
        }
    }
}

/// Writes to a writable overlay or pipe descriptor.
pub fn linux_write_file(fd: u64, data: &[u8]) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    match slot.source {
        LinuxFileSource::Overlay(index) => {
            // Enforce the per-domain total as well as the per-file bound.
            let used: usize = task
                .linux_overlay
                .iter()
                .flatten()
                .map(|entry| entry.len)
                .sum();
            let Some(entry) = task.linux_overlay.get_mut(index).and_then(Option::as_mut) else {
                return Err(LINUX_EBADF);
            };
            let end = slot.offset.checked_add(data.len()).ok_or(LINUX_ENOSPC)?;
            if end > OVERLAY_BYTES {
                return Err(LINUX_ENOSPC);
            }
            let base = used.saturating_sub(entry.len);
            if base.saturating_add(end) > OVERLAY_QUOTA_BYTES {
                return Err(LINUX_ENOSPC);
            }
            entry.data[slot.offset..end].copy_from_slice(data);
            entry.len = entry.len.max(end);
            slot.offset = end;
            let mut path = [0u8; OVERLAY_PATH];
            path.copy_from_slice(&entry.path[..OVERLAY_PATH]);
            let path_length = entry.path_len;
            let Ok(path) = core::str::from_utf8(&path[..path_length]) else {
                return Ok(data.len());
            };
            linux_inotify_emit(path, IN_MODIFY, false);
            Ok(data.len())
        }
        LinuxFileSource::Pipe { index, write: true } => {
            let Some(pipe) = task.linux_pipes.get_mut(index).and_then(Option::as_mut) else {
                return Err(LINUX_EBADF);
            };
            if pipe.len == PIPE_BYTES {
                return Err(LINUX_EAGAIN);
            }
            let length = data.len().min(PIPE_BYTES - pipe.len);
            for (offset, byte) in data.iter().take(length).enumerate() {
                pipe.data[(pipe.write + offset) % PIPE_BYTES] = *byte;
            }
            pipe.write = (pipe.write + length) % PIPE_BYTES;
            pipe.len += length;
            Ok(length)
        }
        LinuxFileSource::EventFd { index } => {
            if data.len() < 8 {
                return Err(LINUX_EINVAL);
            }
            let value = u64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]));
            if !task.linux_eventfds.get(index).is_some_and(Option::is_some)
                || task.tgid >= MAX_TASKS
            {
                return Err(LINUX_EBADF);
            }
            let Some(counter) =
                (unsafe { &mut (*EVENTFD_COUNTERS.0.get())[task.tgid][index] }).as_mut()
            else {
                return Err(LINUX_EBADF);
            };
            *counter = counter.saturating_add(value);
            Ok(8)
        }
        _ => Err(LINUX_EROFS),
    }
}

const IN_MODIFY: u32 = 0x2;
const IN_ATTRIB: u32 = 0x4;
const IN_CLOSE_WRITE: u32 = 0x8;
const IN_CLOSE_NOWRITE: u32 = 0x10;
const IN_OPEN: u32 = 0x20;
const IN_CREATE: u32 = 0x100;
const IN_IGNORED: u32 = 0x8000;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_ISDIR: u32 = 0x4000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_NONBLOCK: u64 = 0x800;

fn inotify_state(tgid: usize) -> Option<&'static mut LinuxInotify> {
    if tgid >= MAX_TASKS {
        return None;
    }
    // SAFETY: Inotify state is only touched from the syscall path with
    // interrupts masked; the same pattern guards the task and eventfd tables.
    unsafe { (*INOTIFY.0.get())[tgid].as_mut() }
}

fn inotify_fd(task: &Task, fd: u64) -> bool {
    matches!(
        task.linux_fds.get(fd as usize).and_then(Option::as_ref),
        Some(LinuxFile {
            source: LinuxFileSource::Inotify,
            ..
        })
    )
}

/// Wakes one blocked reader in the process and completes its `read`.
fn wake_inotify_reader(tgid: usize) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    for task in tasks.iter_mut() {
        if task.tgid != tgid || task.state != TaskState::Blocked || task.inotify_wait_pointer == 0 {
            continue;
        }
        let pointer = task.inotify_wait_pointer;
        let capacity = task.inotify_wait_length.min(512);
        let pml4 = task.pml4;
        let mut buffer = [0u8; 512];
        let Some(state) = inotify_state(tgid) else {
            return;
        };
        let result = match inotify_pack(state, &mut buffer[..capacity]) {
            Ok(count) => {
                if PhysFrame::from_address(pml4)
                    .is_some_and(|frame| paging::write_user_bytes(frame, pointer, &buffer[..count]))
                {
                    count as i64
                } else {
                    -14
                }
            }
            Err(error) => error,
        };
        task.inotify_wait_pointer = 0;
        task.inotify_wait_length = 0;
        task.state = TaskState::Ready;
        // SAFETY: saved_rsp points at the syscall frame retained on this
        // task's private kernel stack when it blocked in the inotify read.
        unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = result as u64 };
        return;
    }
}

fn inotify_pack(state: &mut LinuxInotify, out: &mut [u8]) -> Result<usize, i64> {
    let mut written = 0usize;
    while state.event_len != 0 {
        let Some(event) = state.events[state.event_head] else {
            break;
        };
        let record = (16 + event.name_len + 3) & !3;
        if written + record > out.len() {
            break;
        }
        let target = &mut out[written..written + record];
        target.fill(0);
        target[..4].copy_from_slice(&event.wd.to_ne_bytes());
        target[4..8].copy_from_slice(&event.mask.to_ne_bytes());
        target[8..12].copy_from_slice(&event.cookie.to_ne_bytes());
        target[12..16].copy_from_slice(&(event.name_len as u32).to_ne_bytes());
        target[16..16 + event.name_len].copy_from_slice(&event.name[..event.name_len]);
        written += record;
        state.dequeue();
    }
    if state.overflow {
        if written + 16 > out.len() {
            if written == 0 {
                return Err(LINUX_EINVAL);
            }
        } else {
            let target = &mut out[written..written + 16];
            target.fill(0);
            target[..4].copy_from_slice(&(-1i32).to_ne_bytes());
            target[4..8].copy_from_slice(&IN_Q_OVERFLOW.to_ne_bytes());
            written += 16;
            state.overflow = false;
        }
    }
    if written == 0 {
        return Err(LINUX_EINVAL);
    }
    Ok(written)
}

fn parent_and_name(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Emits one event to an exact-path watch and to the watch on the path's
/// parent directory. Directory events carry the child's base name.
pub fn linux_inotify_emit(path: &str, mask: u32, is_dir: bool) {
    let tgid = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].tgid
    };
    let Some(state) = inotify_state(tgid) else {
        return;
    };
    let (parent, name) = parent_and_name(path);
    for index in 0..MAX_INOTIFY_WATCHES {
        let Some(watch) = state.watches[index] else {
            continue;
        };
        let watch_name = watch_path(&watch);
        let event_mask = if watch_name == path {
            mask & watch.mask
        } else if watch_name == parent {
            (mask | if is_dir { IN_ISDIR } else { 0 }) & watch.mask
        } else {
            0
        };
        if event_mask == 0 {
            continue;
        }
        let mut event = InotifyEvent {
            wd: watch.wd,
            mask: event_mask,
            cookie: 0,
            name: [0; INOTIFY_NAME_BYTES],
            name_len: 0,
        };
        if watch_name != path {
            let length = name.len().min(INOTIFY_NAME_BYTES);
            event.name[..length].copy_from_slice(&name.as_bytes()[..length]);
            event.name_len = length;
        }
        state.enqueue(event);
    }
    if state.has_data() {
        wake_inotify_reader(tgid);
    }
}

/// Creates one inotify instance per process; `flags` 0 blocks on read.
pub fn linux_inotify_init(flags: u64) -> Result<u64, i64> {
    if flags != 0 && flags != IN_NONBLOCK {
        return Err(LINUX_EINVAL);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let tgid = task.tgid;
    if tgid >= MAX_TASKS {
        return Err(LINUX_EMFILE);
    }
    let Some((fd, _)) = task
        .linux_fds
        .iter()
        .enumerate()
        .skip(3)
        .find(|(_, slot)| slot.is_none())
    else {
        return Err(LINUX_EMFILE);
    };
    if inotify_state(tgid).is_some() {
        return Err(LINUX_EMFILE);
    }
    let mut instance = LinuxInotify::EMPTY;
    instance.blocking = flags != IN_NONBLOCK;
    // SAFETY: The same single-CPU, interrupts-masked syscall serialization that
    // guards the task table covers the inotify table.
    unsafe { (*INOTIFY.0.get())[tgid] = Some(instance) };
    task.linux_fds[fd] = Some(LinuxFile {
        source: LinuxFileSource::Inotify,
        offset: 0,
        writable: false,
    });
    Ok(fd as u64)
}

/// Installs or updates a watch. Four watches per process are supported.
pub fn linux_inotify_add(fd: u64, path: &str, mask: u32) -> Result<i32, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if !inotify_fd(task, fd) {
        return Err(LINUX_EBADF);
    }
    if mask & 0x1fff == 0 {
        return Err(LINUX_EINVAL);
    }
    if path.len() > IN_WATCH_PATH_BYTES
        || !crate::linux::valid_path(path)
        || path.starts_with("/proc")
        || path.starts_with("/dev")
    {
        return Err(LINUX_EINVAL);
    }
    let exists = path == "/tmp"
        || crate::linux::lookup(path).is_some()
        || crate::linux::is_dir(path)
        || task
            .linux_overlay
            .iter()
            .flatten()
            .any(|entry| entry.path() == path);
    if !exists {
        return Err(LINUX_ENOENT);
    }
    let is_dir = crate::linux::is_dir(path) || path == "/tmp";
    if mask & IN_ONLYDIR != 0 && !is_dir {
        return Err(LINUX_ENOTDIR);
    }
    let requested = mask & 0x1fff;
    let tgid = task.tgid;
    let Some(state) = inotify_state(tgid) else {
        return Err(LINUX_EBADF);
    };
    if let Some(index) = state.find_path(path) {
        let Some(watch) = state.watches[index].as_mut() else {
            return Err(LINUX_EBADF);
        };
        if mask & IN_MASK_ADD != 0 {
            watch.mask |= requested;
        } else {
            watch.mask = requested;
        }
        return Ok(watch.wd);
    }
    let Some(slot) = state.watches.iter().position(Option::is_none) else {
        return Err(LINUX_ENOSPC);
    };
    let mut storage = [0u8; IN_WATCH_PATH_BYTES];
    storage[..path.len()].copy_from_slice(path.as_bytes());
    let wd = state.next_wd.max(1);
    state.next_wd = wd.saturating_add(1);
    state.watches[slot] = Some(InotifyWatch {
        wd,
        path: storage,
        path_len: path.len(),
        mask: requested,
    });
    Ok(wd)
}

pub fn linux_inotify_remove(fd: u64, wd: u64) -> Result<(), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if !inotify_fd(task, fd) {
        return Err(LINUX_EBADF);
    }
    let tgid = task.tgid;
    let Some(state) = inotify_state(tgid) else {
        return Err(LINUX_EBADF);
    };
    let Some(index) = state
        .watches
        .iter()
        .position(|watch| watch.is_some_and(|watch| watch.wd as u64 == wd))
    else {
        return Err(LINUX_EINVAL);
    };
    state.watches[index] = None;
    state.enqueue(InotifyEvent {
        wd: wd as i32,
        mask: IN_IGNORED,
        cookie: 0,
        name: [0; INOTIFY_NAME_BYTES],
        name_len: 0,
    });
    if state.has_data() {
        wake_inotify_reader(tgid);
    }
    Ok(())
}

/// Returns whether an empty read should block on this descriptor.
#[must_use]
pub fn linux_inotify_blocks(fd: u64) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    inotify_fd(task, fd) && inotify_state(task.tgid).is_some_and(|state| state.blocking)
}

/// Blocks the current task in an inotify read until an event arrives.
///
/// The waker writes the packed records into the waiting task's user buffer and
/// completes the saved syscall frame with the byte count.
pub fn linux_inotify_block(
    frame: *mut InterruptFrame,
    pointer: u64,
    length: usize,
) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].inotify_wait_pointer = pointer;
    tasks[current].inotify_wait_length = length;
    tasks[current].state = TaskState::Blocked;
    // A signal can wake a blocked task without an event; report EINTR unless
    // the event waker overwrites this value first.
    // SAFETY: The frame is live on this task's kernel stack.
    unsafe { (*frame).rax = (-4i64) as u64 };
    yield_current(frame)
}

/// Reads from a Linux file descriptor without changing its offset.
pub fn linux_pread(fd: u64, out: &mut [u8], offset: u64) -> Result<usize, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    let offset = usize::try_from(offset).map_err(|_| LINUX_EINVAL)?;
    match slot.source {
        LinuxFileSource::Root(file) => {
            let Some(file) = crate::linux::file(file) else {
                return Err(LINUX_EBADF);
            };
            if offset > file.data.len() {
                return Ok(0);
            }
            let length = (file.data.len() - offset).min(out.len());
            out[..length].copy_from_slice(&file.data[offset..offset + length]);
            Ok(length)
        }
        LinuxFileSource::Overlay(index) => {
            let Some(entry) = task.linux_overlay.get(index).and_then(Option::as_ref) else {
                return Err(LINUX_EBADF);
            };
            if offset > entry.len {
                return Ok(0);
            }
            let length = (entry.len - offset).min(out.len());
            out[..length].copy_from_slice(&entry.data[offset..offset + length]);
            Ok(length)
        }
        LinuxFileSource::Directory { .. } => Err(LINUX_EISDIR),
        LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. } => Err(LINUX_ESPIPE),
        LinuxFileSource::Pseudo(kind) => {
            let count = pseudo_read(kind, offset, out, &task.vmas, task.tgid, task.parent)?;
            Ok(count)
        }
        LinuxFileSource::PseudoDir(_) => Err(LINUX_EISDIR),
        LinuxFileSource::Inotify => Err(LINUX_ESPIPE),
    }
}

/// Closes a Linux file descriptor.
pub fn linux_close(fd: u64) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get_mut(fd as usize) else {
        return LINUX_EBADF;
    };
    let Some(file) = slot.take() else {
        return LINUX_EBADF;
    };
    let mut notify = None;
    match file.source {
        LinuxFileSource::Inotify => {
            if !task
                .linux_fds
                .iter()
                .flatten()
                .any(|file| matches!(file.source, LinuxFileSource::Inotify))
            {
                let tgid = task.tgid;
                if tgid < MAX_TASKS {
                    // SAFETY: Single-CPU syscall serialization guards the table.
                    unsafe { (*INOTIFY.0.get())[tgid] = None };
                }
            }
        }
        LinuxFileSource::Overlay(index) => {
            if let Some(entry) = task.linux_overlay.get(index).and_then(Option::as_ref) {
                notify = Some((
                    entry.path().as_bytes().to_vec(),
                    if file.writable {
                        IN_CLOSE_WRITE
                    } else {
                        IN_CLOSE_NOWRITE
                    },
                ));
            }
        }
        _ => {}
    }
    if let Some((path, mask)) = notify {
        // The overlay entry path is bounded and UTF-8 by construction.
        if let Ok(path) = core::str::from_utf8(&path) {
            linux_inotify_emit(path, mask, false);
        }
    }
    0
}

/// Repositions a Linux file descriptor.
pub fn linux_lseek(fd: u64, offset: i64, whence: u64) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
        return LINUX_EBADF;
    };
    let size = match slot.source {
        LinuxFileSource::Root(file) => crate::linux::file(file).map_or(0, |file| file.data.len()),
        LinuxFileSource::Overlay(index) => task
            .linux_overlay
            .get(index)
            .and_then(Option::as_ref)
            .map_or(0, |entry| entry.len),
        LinuxFileSource::Directory { .. }
        | LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::PseudoDir(_) => 0,
        LinuxFileSource::Pseudo(PseudoFile::ProcExe) => LINUX_ELF.len(),
        LinuxFileSource::Pseudo(_) => 0,
        LinuxFileSource::Inotify => 0,
    };
    let base = match whence {
        0 => 0i64,
        1 => slot.offset as i64,
        2 => size as i64,
        _ => return LINUX_EINVAL,
    };
    let Some(position) = base.checked_add(offset) else {
        return LINUX_EINVAL;
    };
    if position < 0 || position as usize > size {
        return LINUX_EINVAL;
    }
    slot.offset = position as usize;
    position
}

/// Reports whether a path exists in the read-only root.
#[must_use]
pub fn linux_access(path: &str) -> bool {
    pseudo_file(path).is_some()
        || pseudo_dir(path).is_some()
        || crate::linux::lookup(path).is_some()
        || crate::linux::is_dir(path)
}

/// Reserves a range in the Linux task's VMA window.
///
/// The pages live in the pre-mapped arena, so this does not allocate frames.
/// `hint` is honoured when free; `fixed` requires the exact address and
/// `no_replace` rejects an overlap instead of replacing it.
pub fn linux_vma_insert(
    length: u64,
    prot: u32,
    hint: u64,
    fixed: bool,
    no_replace: bool,
    anonymous: bool,
    shared: bool,
) -> Result<u64, VmaError> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &tasks[current];
    if task.abi != TaskAbi::Linux {
        return Err(VmaError::InvalidRange);
    }
    let tgid = task.tgid;
    let pml4 = task.pml4;
    let mut current_vmas = task.vmas;
    let mut current_anonymous = task.anonymous_vmas;
    let mut current_shared = task.shared_vmas;
    let start = if hint != 0 && hint.is_multiple_of(PAGE_SIZE) {
        match current_vmas.insert_at(hint, length, prot) {
            Ok(()) => hint,
            Err(VmaError::Overlap) if !fixed && !no_replace => current_vmas.insert(length, prot)?,
            Err(error) => return Err(error),
        }
    } else if fixed {
        return Err(VmaError::InvalidRange);
    } else {
        current_vmas.insert(length, prot)?
    };
    if anonymous {
        current_anonymous.insert_at(start, length, prot)?;
    }
    if shared {
        current_shared.insert_at(start, length, prot)?;
    }

    // Validate all peers before changing any record, so a full or inconsistent
    // table cannot leave partially inserted metadata in the address-space group.
    for (index, peer) in tasks.iter().enumerate() {
        if index == current
            || peer.state == TaskState::Empty
            || peer.state == TaskState::Done
            || peer.abi != TaskAbi::Linux
            || peer.tgid != tgid
            || peer.pml4 != pml4
        {
            continue;
        }
        let mut vmas = peer.vmas;
        let mut anonymous_vmas = peer.anonymous_vmas;
        let mut shared_vmas = peer.shared_vmas;
        vmas.insert_at(start, length, prot)?;
        if anonymous {
            anonymous_vmas.insert_at(start, length, prot)?;
        }
        if shared {
            shared_vmas.insert_at(start, length, prot)?;
        }
    }

    let task = &mut tasks[current];
    task.vmas = current_vmas;
    task.anonymous_vmas = current_anonymous;
    task.shared_vmas = current_shared;
    for (index, peer) in tasks.iter_mut().enumerate() {
        if index == current
            || peer.state == TaskState::Empty
            || peer.state == TaskState::Done
            || peer.abi != TaskAbi::Linux
            || peer.tgid != tgid
            || peer.pml4 != pml4
        {
            continue;
        }
        peer.vmas
            .insert_at(start, length, prot)
            .expect("VMA preflight changed");
        if anonymous {
            peer.anonymous_vmas
                .insert_at(start, length, prot)
                .expect("VMA preflight changed");
        }
        if shared {
            peer.shared_vmas
                .insert_at(start, length, prot)
                .expect("VMA preflight changed");
        }
    }
    Ok(start)
}

/// Returns `true` when every page in a Linux range is mapped.
pub fn linux_vma_covers(address: u64, length: u64) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    task.abi == TaskAbi::Linux && task.vmas.covers(address, length)
}

/// Pages in a private-anonymous Linux VMA may be materialized while validating
/// a syscall buffer. This is deliberately limited to the current task and to
/// ranges covered by both VMA tables; ordinary eager mappings retain the
/// existing page-table validation path.
pub fn linux_page_in_user_range(pml4: PhysFrame, address: u64, length: u64, write: bool) -> bool {
    if length == 0 {
        return true;
    }
    let Some(end) = address.checked_add(length) else {
        return false;
    };
    let current = CURRENT.load(Ordering::Relaxed);
    if current >= MAX_TASKS {
        return false;
    }
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[current];
    if task.abi != TaskAbi::Linux || task.pml4 != pml4.start_address() {
        return false;
    }
    if !task.vmas.covers(address, length) || !task.anonymous_vmas.covers(address, length) {
        return false;
    }
    let Some(vma) = task.anonymous_vmas.get(address) else {
        return false;
    };
    if write && vma.prot & aether_memory::vma::PROT_WRITE == 0
        || !write && vma.prot & aether_memory::vma::PROT_READ == 0
    {
        return false;
    }
    let mut page = address & !(PAGE_SIZE - 1);
    while page < end {
        if !paging::user_range_valid(pml4, page, 1, write)
            && !resolve_linux_demand_fault(
                core::ptr::null_mut(),
                page,
                4 | if write { 2 } else { 0 },
            )
        {
            return false;
        }
        page = page.saturating_add(PAGE_SIZE);
    }
    true
}

/// Releases a range from the Linux task's VMA table.
///
/// Returns `true` when at least one tracked area overlapped the range.
pub fn linux_vma_remove(address: u64, length: u64) -> Result<bool, VmaError> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if tasks[current].abi != TaskAbi::Linux {
        return Err(VmaError::NotFound);
    }
    let tgid = tasks[current].tgid;
    let pml4 = tasks[current].pml4;
    let removed = tasks[current].vmas.remove(address, length)?;
    for (index, task) in tasks.iter_mut().enumerate().filter(|(_, task)| {
        task.state != TaskState::Empty && task.tgid == tgid && task.pml4 == pml4
    }) {
        if index != current && task.vmas.covers(address, length) {
            let _ = task.vmas.remove(address, length);
        }
        let _ = task.anonymous_vmas.remove(address, length);
        let _ = task.shared_vmas.remove(address, length);
    }
    Ok(removed)
}

pub fn linux_vma_insert_shared(
    length: u64,
    prot: u32,
    hint: u64,
    fixed: bool,
    no_replace: bool,
) -> Result<u64, VmaError> {
    linux_vma_insert(length, prot, hint, fixed, no_replace, true, true)
}

pub fn linux_shared_vma(address: u64) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)]
        .shared_vmas
        .get(address)
        .is_some()
}

/// Returns anonymous mapping protections for a faulting address.
pub fn linux_anonymous_vma_prot(address: u64) -> Option<u32> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    task.anonymous_vmas.get(address).map(|vma| vma.prot)
}

/// Returns the protection bits for the VMA containing `address`.
#[must_use]
pub fn linux_vma_prot(address: u64) -> Option<u32> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    task.vmas.get(address).map(|vma| vma.prot)
}

/// Changes protections for a mapped Linux range.
pub fn linux_vma_protect(address: u64, length: u64, prot: u32) -> Result<(), VmaError> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if tasks[current].abi != TaskAbi::Linux {
        return Err(VmaError::NotFound);
    }
    let tgid = tasks[current].tgid;
    let pml4 = tasks[current].pml4;
    tasks[current].vmas.protect(address, length, prot)?;
    for (index, task) in tasks.iter_mut().enumerate().filter(|(index, task)| {
        *index != current
            && task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.tgid == tgid
            && task.pml4 == pml4
    }) {
        let _ = index;
        if task.vmas.covers(address, length) {
            let _ = task.vmas.protect(address, length, prot);
        }
        if task.anonymous_vmas.covers(address, length) {
            let _ = task.anonymous_vmas.protect(address, length, prot);
        }
        if task.shared_vmas.covers(address, length) {
            let _ = task.shared_vmas.protect(address, length, prot);
        }
    }
    if tasks[current].anonymous_vmas.covers(address, length) {
        let _ = tasks[current].anonymous_vmas.protect(address, length, prot);
    }
    if tasks[current].shared_vmas.covers(address, length) {
        let _ = tasks[current].shared_vmas.protect(address, length, prot);
    }
    Ok(())
}

/// Returns metadata for a Linux path: `(size, inode, mode)`.
///
/// The root is a flat imported tree, so regular files have a stable inode
/// derived from their index and a synthetic mode.
pub fn linux_stat_path(path: &str) -> Option<(u64, u64, u32)> {
    if let Some(kind) = pseudo_file(path) {
        return Some(pseudo_stat(kind));
    }
    if pseudo_dir(path).is_some() {
        return Some((0, 2, 0o040755));
    }
    if crate::linux::is_dir(path) {
        return Some((0, 2, 0o040755));
    }
    let index = crate::linux::lookup(path)?;
    let file = crate::linux::file(index)?;
    Some((file.data.len() as u64, index as u64 + 1, 0o100644))
}

/// Returns metadata for an open Linux descriptor.
pub fn linux_stat_fd(fd: u64) -> Result<(u64, u64, u32), i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    match slot.source {
        LinuxFileSource::Root(index) => {
            let file = crate::linux::file(index).ok_or(LINUX_EBADF)?;
            Ok((file.data.len() as u64, index as u64 + 1, 0o100644))
        }
        LinuxFileSource::Overlay(index) => {
            let entry = task
                .linux_overlay
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(LINUX_EBADF)?;
            Ok((entry.len as u64, index as u64 + 1, 0o100644))
        }
        LinuxFileSource::Directory { .. } | LinuxFileSource::PseudoDir(_) => Ok((0, 2, 0o040755)),
        LinuxFileSource::Pseudo(kind) => Ok(pseudo_stat(kind)),
        LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. } => Ok((0, 0, 0o010600)),
        LinuxFileSource::Inotify => Ok((0, 0, 0o010600)),
    }
}

/// Duplicates a Linux descriptor into the lowest free slot.
pub fn linux_dup(fd: u64) -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(source) = task.linux_fds.get(fd as usize).copied().flatten() else {
        return Err(LINUX_EBADF);
    };
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(source);
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Duplicates `old` onto `new`, closing an existing target first.
pub fn linux_dup2(old: u64, new: u64) -> Result<u64, i64> {
    if old == new {
        return if linux_stat_fd(old).is_ok() {
            Ok(new)
        } else {
            Err(LINUX_EBADF)
        };
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(source) = task.linux_fds.get(old as usize).copied().flatten() else {
        return Err(LINUX_EBADF);
    };
    let Some(slot) = task.linux_fds.get_mut(new as usize) else {
        return Err(LINUX_EBADF);
    };
    *slot = Some(source);
    Ok(new)
}

/// Returns the bytes in a Linux file descriptor, independent of its offset.
pub fn linux_file_len(fd: u64) -> Result<u64, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    match slot.source {
        LinuxFileSource::Root(file) => crate::linux::file(file)
            .map(|file| file.data.len() as u64)
            .ok_or(LINUX_EBADF),
        LinuxFileSource::Overlay(index) => task
            .linux_overlay
            .get(index)
            .and_then(Option::as_ref)
            .map(|entry| entry.len as u64)
            .ok_or(LINUX_EBADF),
        LinuxFileSource::Directory { .. }
        | LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::PseudoDir(_) => Ok(0),
        LinuxFileSource::Pseudo(PseudoFile::ProcExe) => Ok(LINUX_ELF.len() as u64),
        LinuxFileSource::Pseudo(_) => Ok(0),
        LinuxFileSource::Inotify => Ok(0),
    }
}

/// Reads a Linux file at `offset` without changing the descriptor offset.
pub fn linux_file_read_at(fd: u64, offset: u64, out: &mut [u8]) -> Result<usize, i64> {
    linux_pread(fd, out, offset)
}

/// Returns the Linux program break.
///
/// The break has a fixed 1 MiB window at the start of the arena; the mmap
/// window begins after it. Growth is zero-filled. A request outside the window
/// leaves the break unchanged, matching Linux `brk` failure semantics.
pub fn linux_brk(request: u64) -> u64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    if task.abi != TaskAbi::Linux {
        return 0;
    }
    let base = paging::LINUX_ARENA_BASE;
    let limit = paging::LINUX_MMAP_BASE;
    if request == 0 {
        return task.brk;
    }
    if request < base || request > limit {
        return task.brk;
    }
    let old = task.brk;
    if request == old {
        return old;
    }
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return old;
    };
    let flags = aether_memory::paging::PageFlags::PRESENT
        | aether_memory::paging::PageFlags::USER
        | aether_memory::paging::PageFlags::WRITABLE
        | aether_memory::paging::PageFlags::NO_EXECUTE;
    let (start, length, map) = if request > old {
        let start = old
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
            .unwrap_or(old);
        let end = request
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
            .unwrap_or(request);
        (start, end.saturating_sub(start), true)
    } else {
        let start = request
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
            .unwrap_or(request);
        let end = old
            .checked_add(PAGE_SIZE - 1)
            .map(|value| value & !(PAGE_SIZE - 1))
            .unwrap_or(old);
        (start, end.saturating_sub(start), false)
    };
    if length != 0 {
        let Some(result) = crate::memory::with_frame_allocator(|allocator| {
            if map {
                paging::map_allocated_user_range(allocator, pml4, start, length, flags)
            } else {
                paging::unmap_user_range(allocator, pml4, start, length).map(|_| ())
            }
        }) else {
            return old;
        };
        if result.is_err() {
            return old;
        }
    }
    task.brk = request;
    task.brk
}

/// Sets the current Linux task's FS base (TLS pointer).
pub fn set_linux_fs_base(base: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].fs_base = base;
    // SAFETY: The syscall path runs at CPL0 with interrupts masked.
    unsafe { crate::arch::x86_64::cpu::set_fs_base(base) };
}

/// Returns the current Linux task's FS base.
#[must_use]
pub fn linux_fs_base() -> u64 {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].fs_base
}

/// Releases the Linux task's anonymous mappings and heap pages.
fn release_linux_mappings(index: usize) {
    let (pml4, vmas, brk) = {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[index];
        (task.pml4, task.vmas, task.brk)
    };
    let Some(pml4) = PhysFrame::from_address(pml4) else {
        return;
    };
    let _ = crate::memory::with_frame_allocator(|allocator| {
        for vma in vmas.iter() {
            let _ = paging::unmap_user_range(allocator, pml4, vma.start, vma.end - vma.start);
        }
        let start = paging::LINUX_ARENA_BASE;
        if brk > start {
            let start = start
                .checked_add(PAGE_SIZE - 1)
                .map(|value| value & !(PAGE_SIZE - 1))
                .unwrap_or(start);
            let end = brk
                .checked_add(PAGE_SIZE - 1)
                .map(|value| value & !(PAGE_SIZE - 1))
                .unwrap_or(brk);
            if end > start {
                let _ = paging::unmap_user_range(allocator, pml4, start, end - start);
            }
        }
    });
}

/// Terminates the Linux ABI boot task.
pub fn linux_exit(status: u64) -> *mut InterruptFrame {
    let current = CURRENT.load(Ordering::Relaxed);
    let kind = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[current].kind
    };
    if kind == TaskKind::Process {
        release_linux_mappings(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        let tgid = tasks[current].tgid;
        if tgid < MAX_TASKS {
            // SAFETY: Single-CPU exit path with interrupts masked.
            unsafe { (*INOTIFY.0.get())[tgid] = None };
        }
        tasks[current].futex_address = 0;
        tasks[current].futex_deadline = 0;
        tasks[current].state = TaskState::Done;
        tasks[current].exit_status = ((status & 0xff) << 8) as i64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_pipes = [None; MAX_PIPES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        serial_line!("linux: process exit pid={current} status={status}");
        wake_waiting_parent(current);
        return schedule_next(current);
    }
    if kind == TaskKind::Thread {
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].futex_address = 0;
        tasks[current].futex_deadline = 0;
        tasks[current].state = TaskState::Done;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_pipes = [None; MAX_PIPES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        serial_line!("linux: thread exit tid={current} status={status}");
        return schedule_next(current);
    }
    if (current != LINUX_TASK && current != LINUX_DYN_TASK) || kind != TaskKind::Linux {
        serial_line!("AETHER_BOOT_FAILED reason=linux exit from wrong task");
        halt()
    }

    release_linux_mappings(current);
    let tasks = unsafe { &mut *TASKS.0.get() };
    let tgid = tasks[current].tgid;
    if tgid < MAX_TASKS {
        // SAFETY: Single-CPU exit path with interrupts masked.
        unsafe { (*INOTIFY.0.get())[tgid] = None };
    }
    tasks[current].futex_address = 0;
    tasks[current].futex_deadline = 0;
    tasks[current].state = TaskState::Done;
    tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
    tasks[current].linux_overlay = [None; OVERLAY_FILES];
    tasks[current].linux_pipes = [None; MAX_PIPES];
    tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
    tasks[current].linux_epolls = [None; MAX_EPOLLS];
    tasks[current].vmas.reset();
    tasks[current].anonymous_vmas.reset();
    let result = if status == 0 {
        UserTestResult::Passed
    } else {
        UserTestResult::Failed
    };
    if current == LINUX_DYN_TASK {
        LINUX_DYN_EXITS.fetch_add(1, Ordering::Release);
        LINUX_DYN_RESULT.store(result as usize, Ordering::Release);
    } else {
        LINUX_EXITS.fetch_add(1, Ordering::Release);
        LINUX_RESULT.store(result as usize, Ordering::Release);
    }
    for task in tasks.iter_mut().skip(1) {
        if task.kind == TaskKind::Thread && task.tgid == current {
            task.futex_address = 0;
            task.futex_deadline = 0;
            task.state = TaskState::Done;
        }
    }
    serial_line!("linux: exit pid={} status={status}", current);
    schedule_next(current)
}

/// Handles a fault raised by a ring-3 task. The task is retired and the kernel
/// continues with the next runnable task.
/// Resolves a not-present user fault inside a private anonymous Linux VMA.
/// The exception gate has IF clear, serializing allocator use on this CPU.
pub fn resolve_linux_demand_fault(_frame: *mut InterruptFrame, address: u64, error: u64) -> bool {
    const PRESENT: u64 = 1;
    const WRITE: u64 = 2;
    const USER: u64 = 4;
    const RESERVED: u64 = 8;
    const INSTRUCTION: u64 = 16;
    if CURRENT.load(Ordering::Relaxed) >= MAX_TASKS
        || error & (PRESENT | RESERVED) != 0
        || error & USER == 0
    {
        return false;
    }
    let current = CURRENT.load(Ordering::Relaxed);
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[current];
    if task.abi != TaskAbi::Linux
        || task.kind != TaskKind::Linux
            && task.kind != TaskKind::Process
            && task.kind != TaskKind::Thread
    {
        return false;
    }
    let page = address & !(PAGE_SIZE - 1);
    let Some(prot) = task.anonymous_vmas.get(address).map(|vma| vma.prot) else {
        return false;
    };
    if error & WRITE != 0 && prot & aether_memory::vma::PROT_WRITE == 0
        || error & INSTRUCTION != 0 && prot & aether_memory::vma::PROT_EXEC == 0
    {
        return false;
    }
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return false;
    };
    let mut flags =
        aether_memory::paging::PageFlags::PRESENT | aether_memory::paging::PageFlags::USER;
    if prot & aether_memory::vma::PROT_WRITE != 0 {
        flags = flags | aether_memory::paging::PageFlags::WRITABLE;
    }
    if prot & aether_memory::vma::PROT_EXEC == 0 {
        flags = flags | aether_memory::paging::PageFlags::NO_EXECUTE;
    }
    crate::memory::with_frame_allocator(|allocator| {
        paging::map_allocated_user_page(allocator, pml4, page, flags).is_ok()
    })
    .unwrap_or(false)
}

pub fn on_user_fault(frame: *mut InterruptFrame, vector: u64) -> *mut InterruptFrame {
    let current = CURRENT.load(Ordering::Relaxed);
    let (linux, thread, process, kind) = {
        let tasks = unsafe { &*TASKS.0.get() };
        (
            tasks[current].kind == TaskKind::Linux,
            tasks[current].kind == TaskKind::Thread,
            tasks[current].kind == TaskKind::Process,
            tasks[current].kind,
        )
    };
    if kind != TaskKind::User && !linux && !thread && !process {
        serial_line!("AETHER_BOOT_FAILED reason=user-fault on non-user task");
        halt()
    }
    if process {
        release_linux_mappings(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Done;
        tasks[current].exit_status = 128 + vector as i64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_pipes = [None; MAX_PIPES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        serial_line!("linux: process fault pid={current} vector={vector}");
        wake_waiting_parent(current);
        return schedule_next(current);
    }
    if thread {
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Faulted;
        tasks[current].saved_rsp = frame as u64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_pipes = [None; MAX_PIPES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        serial_line!("linux: thread fault tid={current} vector={vector}");
        return schedule_next(current);
    }

    if linux {
        let signum = if vector == 14 { 7 } else { 11 };
        if linux_deliver_fault(frame, signum) {
            let tasks = unsafe { &mut *TASKS.0.get() };
            tasks[current].state = TaskState::Running;
            return frame;
        }
        release_linux_mappings(current);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[current].state = TaskState::Faulted;
    tasks[current].saved_rsp = frame as u64;
    // Release the task's open-file slots on the fault path for the same
    // reason as on exit. Only faulting-task state is touched here; unrelated
    // tasks and the kernel continue.
    let _closed = crate::fs::close_task_files(&mut tasks[current].caps);
    if linux {
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_pipes = [None; MAX_PIPES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].vmas.reset();
        tasks[current].anonymous_vmas.reset();
        if current == LINUX_DYN_TASK {
            LINUX_DYN_FAULTS.fetch_add(1, Ordering::Release);
            LINUX_DYN_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        } else {
            LINUX_FAULTS.fetch_add(1, Ordering::Release);
            LINUX_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        }
    } else {
        USER_FAULTS.fetch_add(1, Ordering::Release);
        USER_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        crate::apps::faulted(current, vector);
    }
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
    // SAFETY: The scheduler path runs at CPL0 with interrupts masked.
    unsafe { crate::arch::x86_64::cpu::set_fs_base(tasks[next].fs_base) };

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
    let now = TICKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let current = CURRENT.load(Ordering::Relaxed);
    // Deliver a pending signal to the interrupted Linux task before it is
    // saved and possibly scheduled out.
    let _ = linux_deliver_signal(frame);
    let tasks = unsafe { &mut *TASKS.0.get() };

    for task in tasks.iter_mut() {
        if task.state == TaskState::Blocked
            && task.futex_address != 0
            && task.futex_deadline != 0
            && now >= task.futex_deadline
        {
            // SAFETY: saved_rsp is the syscall frame retained when this task
            // blocked in FUTEX_WAIT.
            unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-110i64) as u64 };
            task.futex_address = 0;
            task.futex_deadline = 0;
            task.state = TaskState::Ready;
        }
    }

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
    // SAFETY: The timer path runs at CPL0 with interrupts masked.
    unsafe { crate::arch::x86_64::cpu::set_fs_base(tasks[next].fs_base) };

    if next != current
        && let Some(pml4) = PhysFrame::from_address(tasks[next].pml4)
    {
        paging::activate(pml4);
    }

    tasks[next].saved_rsp as *mut InterruptFrame
}

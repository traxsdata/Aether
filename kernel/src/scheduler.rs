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
    TimerFd { index: usize },
    Inotify,
}

const PIPE_BYTES: usize = 512;
/// Global pipe slots. Forked processes share the same slot, so a write on one
/// side is visible to the other.
const MAX_PIPES: usize = 4;
const MAX_EVENTFDS: usize = 2;
const MAX_EPOLLS: usize = 1;
const MAX_EPOLL_WATCH: usize = 8;
const MAX_TIMERFDS: usize = 2;
const POLL_IN: i16 = 1;
const POLL_OUT: i16 = 4;
const POLL_ERR: i16 = 8;
const POLL_HUP: i16 = 0x10;
const POLL_NVAL: i16 = 0x20;
const EPOLLET: u32 = 1 << 31;
const EPOLLONESHOT: u32 = 1 << 30;

/// One ready epoll entry returned to user space.
#[derive(Clone, Copy)]
pub struct LinuxEpollEvent {
    /// Requested/returned event mask.
    pub events: u32,
    /// Caller data associated with the watched descriptor.
    pub data: u64,
}

#[derive(Clone, Copy)]
struct LinuxEpollWatch {
    fd: u64,
    events: u32,
    data: u64,
    /// Previous readiness for edge-triggered watches.
    last_ready: bool,
    /// Cleared after reporting an `EPOLLONESHOT` watch; `EPOLL_CTL_MOD`
    /// re-arms it.
    armed: bool,
}

#[derive(Clone, Copy)]
struct LinuxEpoll {
    watches: [Option<LinuxEpollWatch>; MAX_EPOLL_WATCH],
}

impl LinuxEpoll {
    const EMPTY: Self = Self {
        watches: [None; MAX_EPOLL_WATCH],
    };
}

/// One bounded monotonic timer descriptor.
#[derive(Clone, Copy)]
struct LinuxTimerFd {
    next_expiry: u64,
    interval: u64,
    armed: bool,
}

impl LinuxTimerFd {
    const EMPTY: Self = Self {
        next_expiry: 0,
        interval: 0,
        armed: false,
    };
}

/// Why a task is parked. Wakeups are filtered by this kind so a pipe write
/// does not spuriously wake an unrelated sleeper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxWaitKind {
    None,
    Poll,
    Sleep,
    PipeRead(usize),
    PipeWrite(usize),
}

/// Public wait reasons for syscall handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinuxWait {
    /// A `poll`/`epoll_wait` or timerfd wait; any readiness transition wakes it.
    Poll,
    /// An interruptible sleep.
    Sleep,
    /// A blocking read on the global pipe slot.
    PipeRead(usize),
    /// A blocking write on the global pipe slot.
    PipeWrite(usize),
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
}

const OVERLAY_FILES: usize = 4;
const OVERLAY_BYTES: usize = 512;
const OVERLAY_PATH: usize = 64;

#[derive(Clone, Copy)]
struct LinuxOverlay {
    path: [u8; OVERLAY_PATH],
    path_len: usize,
    data: [u8; OVERLAY_BYTES],
    len: usize,
}

#[derive(Clone, Copy)]
struct LinuxInotify {
    path: [u8; OVERLAY_PATH],
    path_len: usize,
    watched: bool,
    pending_modify: bool,
}

impl LinuxInotify {
    const EMPTY: Self = Self {
        path: [0; OVERLAY_PATH],
        path_len: 0,
        watched: false,
        pending_modify: false,
    };
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

/// Per-signal `siginfo_t` payload kept until the handler is delivered.
#[derive(Clone, Copy, Default)]
struct SigInfo {
    code: i32,
    pid: u32,
    uid: u32,
    address: u64,
}

/// x86-64 `struct sigcontext` as embedded in `ucontext_t` (`mcontext`).
#[repr(C)]
#[derive(Clone, Copy)]
struct SigContext {
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rax: u64,
    rcx: u64,
    rsp: u64,
    rip: u64,
    eflags: u64,
    cs: u16,
    gs: u16,
    fs: u16,
    _pad0: u16,
    err: u64,
    trapno: u64,
    oldmask: u64,
    cr2: u64,
    fpstate: u64,
    reserved: [u64; 8],
}

/// x86-64 `ucontext_t` (304 bytes with a 64-bit signal set).
#[repr(C)]
#[derive(Clone, Copy)]
struct UContext {
    flags: u64,
    link: u64,
    ss_sp: u64,
    ss_flags: u32,
    _pad1: u32,
    ss_size: u64,
    mcontext: SigContext,
    sigmask: u64,
}

/// x86-64 `struct rt_sigframe`: restorer pointer, ucontext, then siginfo.
#[repr(C)]
#[derive(Clone, Copy)]
struct RtSigFrame {
    restorer: u64,
    context: UContext,
    info: [u8; 128],
}

const MAX_SIGNALS: usize = 64;
const SIGKILL: usize = 9;
const SIGCHLD: usize = 17;

// `rt_sigaction` flag bits.
const SA_SIGINFO: u64 = 0x0000_0004;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;

// `sigaltstack` flag bits.
const SS_ONSTACK: u32 = 1;
const SS_DISABLE: u32 = 2;

// Futex owner fields used by robust-list cleanup.
const FUTEX_WAITERS: u32 = 0x8000_0000;
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;
const FUTEX_TID_MASK: u32 = 0x3fff_ffff;
const ROBUST_LIST_HEAD_SIZE: u64 = 24;

const _: () = assert!(core::mem::size_of::<SigContext>() == 256);
const _: () = assert!(core::mem::size_of::<UContext>() == 304);
const _: () = assert!(core::mem::size_of::<RtSigFrame>() == 440);

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
    linux_inotify: Option<LinuxInotify>,
    // Descriptor-local allocation map; counters live in EVENTFD_COUNTERS and
    // are addressed by (tgid, slot), so cloned threads share them.
    linux_eventfds: [Option<()>; MAX_EVENTFDS],
    linux_epolls: [Option<LinuxEpoll>; MAX_EPOLLS],
    linux_timerfds: [Option<LinuxTimerFd>; MAX_TIMERFDS],
    wait_kind: LinuxWaitKind,
    wait_deadline: u64,
    restart_syscall: u64,
    restart_deadline: u64,
    restart_infinite: bool,
    sigactions: [Option<SigAction>; MAX_SIGNALS],
    sigqueue: [Option<SigInfo>; MAX_SIGNALS],
    sigblocked: u64,
    sigpending: u64,
    sigaltstack: Option<(u64, u64)>,
    sigaltstack_active: bool,
    restart_wait: bool,
    signal_exit: u8,
    robust_list: u64,
    robust_list_len: u64,
    rseq: u64,
    rseq_len: u64,
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
            linux_inotify: None,
            linux_eventfds: [None; MAX_EVENTFDS],
            linux_epolls: [None; MAX_EPOLLS],
            linux_timerfds: [None; MAX_TIMERFDS],
            wait_kind: LinuxWaitKind::None,
            wait_deadline: 0,
            restart_syscall: u64::MAX,
            restart_deadline: 0,
            restart_infinite: false,
            sigactions: [None; MAX_SIGNALS],
            sigqueue: [None; MAX_SIGNALS],
            sigblocked: 0,
            sigpending: 0,
            sigaltstack: None,
            sigaltstack_active: false,
            restart_wait: false,
            signal_exit: 0,
            robust_list: 0,
            robust_list_len: 0,
            rseq: 0,
            rseq_len: 0,
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
/// Pipe storage is global so `fork`-inherited descriptors share one buffer.
/// A slot is freed once no task references either end.
static PIPES: Global<[Option<LinuxPipe>; MAX_PIPES]> = Global(UnsafeCell::new([None; MAX_PIPES]));
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
        parent_inotify,
        parent_eventfds,
        parent_epolls,
        parent_timerfds,
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
            parent.linux_inotify,
            parent.linux_eventfds,
            parent.linux_epolls,
            parent.linux_timerfds,
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
    thread.sigactions = tasks[current].sigactions;
    thread.sigblocked = tasks[current].sigblocked;
    thread.sigaltstack = None;
    thread.robust_list = 0;
    thread.robust_list_len = 0;
    thread.rseq = 0;
    thread.rseq_len = 0;
    thread.linux_fds = parent_fds;
    thread.linux_overlay = parent_overlay;
    thread.linux_inotify = parent_inotify;
    thread.linux_eventfds = parent_eventfds;
    thread.linux_epolls = parent_epolls;
    thread.linux_timerfds = parent_timerfds;
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
        inotify,
        eventfds,
        parent_tgid,
        epolls,
        timerfds,
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
            parent.linux_inotify,
            parent.linux_eventfds,
            parent.tgid,
            parent.linux_epolls,
            parent.linux_timerfds,
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
    child.linux_inotify = inotify;
    child.linux_eventfds = eventfds;
    // fork gets a process-private snapshot; threads continue to address the
    // parent's counters via their shared tgid.
    if parent_tgid < MAX_TASKS {
        let counters = unsafe { &mut *EVENTFD_COUNTERS.0.get() };
        counters[slot] = counters[parent_tgid];
    }
    child.linux_epolls = epolls;
    child.linux_timerfds = timerfds;
    child.sigactions = sigactions;
    child.sigblocked = sigblocked;
    child.sigaltstack = sigaltstack;
    child.sigaltstack_active = false;
    child.robust_list = 0;
    child.robust_list_len = 0;
    child.rseq = 0;
    child.rseq_len = 0;

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

/// Parks the current task and rewinds the saved user context to the
/// `syscall` instruction. When the task is woken, the syscall is re-executed
/// and the handler resumes with its stored absolute deadline. This is the
/// kernel's blocking model: a syscall never continues on the kernel stack
/// after handing control to another task.
pub fn linux_park(
    frame: *mut InterruptFrame,
    syscall: u64,
    kind: LinuxWait,
    deadline: Option<u64>,
) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].wait_kind = match kind {
        LinuxWait::Poll => LinuxWaitKind::Poll,
        LinuxWait::Sleep => LinuxWaitKind::Sleep,
        LinuxWait::PipeRead(index) => LinuxWaitKind::PipeRead(index),
        LinuxWait::PipeWrite(index) => LinuxWaitKind::PipeWrite(index),
    };
    tasks[current].wait_deadline = deadline.unwrap_or(0);
    tasks[current].restart_syscall = syscall;
    tasks[current].restart_deadline = deadline.unwrap_or(0);
    tasks[current].restart_infinite = deadline.is_none();
    tasks[current].state = TaskState::Blocked;
    // SAFETY: `frame` is the live Linux syscall frame on the current task's
    // kernel stack; the Linux entry stub is always reached through `syscall`
    // (0f 05), so rewinding two bytes re-executes the instruction on resume.
    unsafe { (*frame).rip = (*frame).rip.wrapping_sub(2) };
    yield_current(frame)
}

/// Clears a stale restart marker when a different syscall starts.
pub fn linux_begin_syscall(number: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.restart_syscall != u64::MAX && task.restart_syscall != number {
        task.restart_syscall = u64::MAX;
        task.wait_kind = LinuxWaitKind::None;
        task.wait_deadline = 0;
    }
}

/// Consumes a restart marker for `number`. `Some(None)` means an infinite
/// wait; `Some(Some(deadline))` carries the original absolute deadline.
pub fn linux_take_restart(number: u64) -> Option<Option<u64>> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.restart_syscall != number {
        return None;
    }
    let deadline = if task.restart_infinite {
        None
    } else {
        Some(task.restart_deadline)
    };
    task.restart_syscall = u64::MAX;
    task.wait_kind = LinuxWaitKind::None;
    task.wait_deadline = 0;
    Some(deadline)
}

fn linux_wake_matching(matches: impl Fn(LinuxWaitKind) -> bool) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    for task in tasks.iter_mut() {
        if task.state == TaskState::Blocked && matches(task.wait_kind) {
            task.wait_kind = LinuxWaitKind::None;
            task.wait_deadline = 0;
            task.state = TaskState::Ready;
        }
    }
}

/// Wakes every poll, epoll, and timerfd waiter. Spurious wakeups are harmless:
/// the syscall handler re-evaluates readiness and parks again.
pub fn linux_wake_pollers() {
    linux_wake_matching(|kind| kind == LinuxWaitKind::Poll);
}

/// Wakes tasks blocked on either end of a pipe when its state changes.
fn linux_wake_pipe(index: usize) {
    linux_wake_matching(|kind| {
        matches!(
            kind,
            LinuxWaitKind::PipeRead(slot) | LinuxWaitKind::PipeWrite(slot) if slot == index
        )
    });
}

/// Returns true when an unblocked signal with an installed handler is pending.
/// This is the "deliverable" condition used for `EINTR`.
#[must_use]
pub fn linux_signal_pending() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return false;
    }
    let pending = task.sigpending & !task.sigblocked;
    if pending == 0 {
        return false;
    }
    (0..MAX_SIGNALS).any(|index| {
        pending & (1u64 << index) != 0
            && task.sigactions[index]
                .is_some_and(|action| action.handler != 0 && action.restorer != 0)
    })
}

/// Registers a bounded robust futex list for the current thread.
///
/// Only the exact 24-byte x86-64 `robust_list_head` layout is accepted. The
/// list is walked on thread/process exit and entries whose futex word still
/// names this thread are marked `FUTEX_OWNER_DIED` before a waiter is woken.
pub fn linux_set_robust_list(head: u64, length: u64) -> i64 {
    if head == 0 && length == 0 {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
        task.robust_list = 0;
        task.robust_list_len = 0;
        return 0;
    }
    if length != ROBUST_LIST_HEAD_SIZE || head == 0 || !head.is_multiple_of(8) {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    task.robust_list = head;
    task.robust_list_len = length;
    0
}

/// Returns the registered robust-list head and length for `pid` (0 = caller).
pub fn linux_get_robust_list(pid: usize) -> Result<(u64, u64), i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let session = tasks[current].sid;
    let target = if pid == 0 {
        Some(current)
    } else {
        tasks.iter().position(|task| {
            task.state != TaskState::Empty
                && task.state != TaskState::Done
                && task.abi == TaskAbi::Linux
                && task.sid == session
                && (task.tgid == pid || (task.kind != TaskKind::Thread && task.tgid == pid))
        })
    };
    let Some(index) = target else {
        return Err(LINUX_ESRCH);
    };
    Ok((tasks[index].robust_list, tasks[index].robust_list_len))
}

/// Records a validated `rseq` area for the current thread.
pub fn linux_set_rseq(address: u64, length: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    task.rseq = address;
    task.rseq_len = if address == 0 { 0 } else { length };
}

fn robust_read_u32(pml4: PhysFrame, address: u64) -> Option<u32> {
    if !address.is_multiple_of(4) {
        return None;
    }
    let mut raw = [0u8; 4];
    if !paging::read_user_bytes(pml4, address, &mut raw) {
        return None;
    }
    Some(u32::from_ne_bytes(raw))
}

fn robust_write_u32(pml4: PhysFrame, address: u64, value: u32) -> bool {
    if !address.is_multiple_of(4) {
        return false;
    }
    paging::write_user_bytes(pml4, address, &value.to_ne_bytes())
}

/// Walks a thread's robust futex list on exit, marking owned words that still
/// name this thread with `FUTEX_OWNER_DIED` and waking one waiter each.
fn robust_futex_cleanup(index: usize) {
    const MAX_ROBUST_ENTRIES: usize = 128;
    let (head, pml4) = {
        let tasks = unsafe { &*TASKS.0.get() };
        (tasks[index].robust_list, tasks[index].pml4)
    };
    if head == 0 || !head.is_multiple_of(8) {
        return;
    }
    let Some(pml4) = PhysFrame::from_address(pml4) else {
        return;
    };
    let mut raw = [0u8; ROBUST_LIST_HEAD_SIZE as usize];
    if !paging::read_user_bytes(pml4, head, &mut raw) {
        return;
    }
    let mut entry = u64::from_ne_bytes(raw[0..8].try_into().unwrap_or([0; 8]));
    let futex_offset = i64::from_ne_bytes(raw[8..16].try_into().unwrap_or([0; 8]));
    let pending = u64::from_ne_bytes(raw[16..24].try_into().unwrap_or([0; 8]));
    let owner = (index as u32) & FUTEX_TID_MASK;

    let mut count = 0usize;
    while entry != 0 && entry != head && count < MAX_ROBUST_ENTRIES {
        let mut next_raw = [0u8; 8];
        if !paging::read_user_bytes(pml4, entry, &mut next_raw) {
            break;
        }
        let next = u64::from_ne_bytes(next_raw);
        robust_release_entry(pml4, entry, futex_offset, owner);
        entry = next;
        count += 1;
    }
    if pending != 0 && count < MAX_ROBUST_ENTRIES {
        robust_release_entry(pml4, pending, futex_offset, owner);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[index].robust_list = 0;
    tasks[index].robust_list_len = 0;
}

fn robust_release_entry(pml4: PhysFrame, entry: u64, futex_offset: i64, owner: u32) {
    let Some(address) = entry.checked_add_signed(futex_offset) else {
        return;
    };
    let Some(value) = robust_read_u32(pml4, address) else {
        return;
    };
    if value & FUTEX_TID_MASK != owner {
        return;
    }
    let updated = (value & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
    if robust_write_u32(pml4, address, updated) {
        let _ = linux_futex_wake(address, 1);
    }
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
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    task.sigaltstack = if address == 0 && size == 0 {
        None
    } else {
        Some((address, size))
    };
    task.sigaltstack_active = false;
}

/// Returns the alternate signal stack and whether the task is running on it.
#[must_use]
pub fn linux_altstack_state() -> (Option<(u64, u64)>, bool) {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    (task.sigaltstack, task.sigaltstack_active)
}

fn queue_signal(index: usize, signum: usize, info: SigInfo) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    if signum == 0 || signum > MAX_SIGNALS {
        return;
    }
    let bit = 1u64 << (signum - 1);
    if tasks[index].sigpending & bit == 0 {
        tasks[index].sigqueue[signum - 1] = Some(info);
    }
    tasks[index].sigpending |= bit;
}

/// Sends a signal to one process or a specific thread in that process.
///
/// A process-directed signal (`tid == None`) is queued on the thread-group
/// leader, matching the bounded model where signal actions are per process.
/// Signal zero performs only the existence check.
pub fn linux_send_signal_to(tgid: usize, tid: Option<usize>, signum: u64) -> i64 {
    if signum > MAX_SIGNALS as u64 {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let session = tasks[current].sid;
    let sender = tasks[current].tgid as u32;
    let targets: [bool; MAX_TASKS] = core::array::from_fn(|index| {
        let task = &tasks[index];
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.sid == session
            && task.tgid == tgid
            && match tid {
                Some(tid) => tid == index,
                None => task.kind != TaskKind::Thread,
            }
    });
    if !targets.iter().any(|target| *target) {
        return LINUX_ESRCH;
    }
    if signum == 0 {
        return 0;
    }
    let code = if tid.is_some() { -6 } else { 0 };
    for (index, target) in targets.iter().enumerate() {
        if !*target {
            continue;
        }
        queue_signal(
            index,
            signum as usize,
            SigInfo {
                code,
                pid: sender,
                uid: 0,
                address: 0,
            },
        );
        if index != current {
            // A scheduler-parked wait (poll/epoll/pipe/sleep) must re-execute
            // its syscall before the handler runs: that lets the handler return
            // EINTR and recover the stored deadline. Other blocked waits have
            // no rewound instruction, so their handler is installed now.
            let defer =
                tasks[index].restart_syscall != u64::MAX && pending_signal_has_handler(index);
            interrupt_blocked(index);
            if !defer {
                deliver_target(index);
            }
        }
    }
    0
}

/// Returns true when the first deliverable pending signal restarts `wait4`.
fn pending_signal_restarts(index: usize) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[index];
    let pending = task.sigpending & !task.sigblocked;
    if pending == 0 {
        return false;
    }
    let signum = pending.trailing_zeros() as usize + 1;
    task.sigactions
        .get(signum - 1)
        .copied()
        .flatten()
        .is_some_and(|action| action.handler > 1 && action.flags & SA_RESTART != 0)
}

/// Returns true when a pending unblocked signal has an installed handler.
fn pending_signal_has_handler(index: usize) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[index];
    let pending = task.sigpending & !task.sigblocked;
    if pending == 0 {
        return false;
    }
    (0..MAX_SIGNALS).any(|signal| {
        pending & (1u64 << signal) != 0
            && task.sigactions[signal]
                .is_some_and(|action| action.handler != 0 && action.restorer != 0)
    })
}

/// Interrupts a blocked syscall after a signal was queued.
fn interrupt_blocked(index: usize) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[index];
    if task.state != TaskState::Blocked || task.saved_rsp == 0 {
        return;
    }
    if task.futex_address != 0 {
        // Futex waits are not restartable: POSIX defines EINTR here.
        task.futex_address = 0;
        task.futex_deadline = 0;
        task.restart_wait = false;
        unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-4i64) as u64 };
    } else if task.wait_pid != 0 {
        if pending_signal_restarts(index) {
            // ERESTARTSYS is internal; `rt_sigreturn` re-enters wait4 after the
            // handler when the action has SA_RESTART.
            task.restart_wait = true;
            unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-512i64) as u64 };
        } else {
            task.restart_wait = false;
            task.wait_pid = 0;
            task.wait_status_pointer = 0;
            unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-4i64) as u64 };
        }
    } else if task.restart_syscall != u64::MAX {
        // A scheduler-parked wait (poll/epoll/pipe/sleep) resumes at the
        // rewound `syscall` instruction. Leave RAX alone so the re-executed
        // handler can report EINTR and recover the stored absolute deadline.
        task.restart_wait = false;
    } else {
        task.restart_wait = false;
        unsafe { (*(task.saved_rsp as *mut InterruptFrame)).rax = (-4i64) as u64 };
    }
    task.state = TaskState::Ready;
}

/// Delivers a pending signal to a target task and falls back to EINTR if a
/// restartable wait could not be intercepted by a handler.
fn deliver_target(index: usize) {
    let frame = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[index].saved_rsp as *mut InterruptFrame
    };
    let delivered = deliver_signal_to(index, frame);
    if !delivered {
        let tasks = unsafe { &mut *TASKS.0.get() };
        if tasks[index].restart_wait && tasks[index].signal_exit == 0 {
            tasks[index].restart_wait = false;
            tasks[index].wait_pid = 0;
            tasks[index].wait_status_pointer = 0;
            if frame.is_null() {
                return;
            }
            unsafe { (*frame).rax = (-4i64) as u64 };
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultAction {
    Terminate,
    Ignore,
}

fn default_action(signum: usize) -> DefaultAction {
    match signum {
        // SIGCHLD, SIGCONT, SIGWINCH, SIGURG and the unsupported stop signals
        // are non-terminating in this bounded model.
        17 | 18 | 19 | 20 | 21 | 22 | 23 | 28 => DefaultAction::Ignore,
        _ => DefaultAction::Terminate,
    }
}

/// Writes the x86-64 `siginfo_t` payload for a queued signal.
fn write_siginfo(info: &mut [u8; 128], signum: usize, queued: SigInfo) {
    info[0..4].copy_from_slice(&(signum as i32).to_ne_bytes());
    info[4..8].copy_from_slice(&0i32.to_ne_bytes());
    info[8..12].copy_from_slice(&queued.code.to_ne_bytes());
    match queued.code {
        0 | -6 | -1 => {
            info[16..20].copy_from_slice(&(queued.pid as i32).to_ne_bytes());
            info[20..24].copy_from_slice(&queued.uid.to_ne_bytes());
        }
        _ => {
            info[16..24].copy_from_slice(&queued.address.to_ne_bytes());
        }
    }
}

/// Delivers the lowest pending unblocked signal to `index` by installing an
/// x86-64 `rt_sigframe`. Returns true when a handler frame was installed.
fn deliver_signal_to(index: usize, frame: *mut InterruptFrame) -> bool {
    let tasks = unsafe { &mut *TASKS.0.get() };
    if index >= tasks.len() || frame.is_null() || tasks[index].abi != TaskAbi::Linux {
        return false;
    }
    loop {
        let task = &tasks[index];
        let pending =
            (task.sigpending & !task.sigblocked) | (task.sigpending & (1u64 << (SIGKILL - 1)));
        if pending == 0 {
            return false;
        }
        let signum = pending.trailing_zeros() as usize + 1;
        let bit = 1u64 << (signum - 1);
        let action = task.sigactions[signum - 1];
        let handler = action.map_or(0, |action| action.handler);
        if handler == 1 {
            tasks[index].sigpending &= !bit;
            continue;
        }
        if handler == 0 {
            match default_action(signum) {
                DefaultAction::Ignore => {
                    tasks[index].sigpending &= !bit;
                    continue;
                }
                DefaultAction::Terminate => {
                    tasks[index].sigpending &= !bit;
                    tasks[index].signal_exit = signum as u8;
                    return false;
                }
            }
        }
        let Some(action) = action else {
            return false;
        };
        if action.restorer == 0 {
            return false;
        }
        let saved = unsafe { *frame };
        if saved.cs & 3 != 3 {
            return false;
        }
        let Some(pml4) = PhysFrame::from_address(tasks[index].pml4) else {
            return false;
        };

        // A signal delivered while already running on the alternate stack must
        // not nest there; Linux falls back to the interrupted stack.
        let frame_size = core::mem::size_of::<RtSigFrame>() as u64;
        let mut frame_pointer = 0u64;
        let mut used_altstack = false;
        if action.flags & SA_ONSTACK != 0
            && !tasks[index].sigaltstack_active
            && let Some((base, size)) = tasks[index].sigaltstack
            && size >= frame_size + 64
        {
            let top = base.saturating_add(size) & !15;
            if top > base && top <= USER_LIMIT {
                let candidate = (top - frame_size) & !15;
                if candidate >= base && paging::user_range_valid(pml4, candidate, frame_size, true)
                {
                    frame_pointer = candidate;
                    used_altstack = true;
                }
            }
        }
        if !used_altstack {
            let Some(candidate) = saved.rsp.checked_sub(frame_size).map(|value| value & !15) else {
                return false;
            };
            if candidate < USER_BASE || !paging::user_range_valid(pml4, candidate, frame_size, true)
            {
                return false;
            }
            frame_pointer = candidate;
        }

        let queued = tasks[index].sigqueue[signum - 1].take().unwrap_or_default();
        let mut info = [0u8; 128];
        write_siginfo(&mut info, signum, queued);
        let (ss_sp, ss_size, ss_flags) = match tasks[index].sigaltstack {
            Some((base, size)) => (base, size, if used_altstack { SS_ONSTACK } else { 0 }),
            None => (0, 0, SS_DISABLE),
        };
        let old_mask = tasks[index].sigblocked;
        let context = UContext {
            flags: 0,
            link: 0,
            ss_sp,
            ss_flags,
            _pad1: 0,
            ss_size,
            mcontext: SigContext {
                r8: saved.r8,
                r9: saved.r9,
                r10: saved.r10,
                r11: saved.r11,
                r12: saved.r12,
                r13: saved.r13,
                r14: saved.r14,
                r15: saved.r15,
                rdi: saved.rdi,
                rsi: saved.rsi,
                rbp: saved.rbp,
                rbx: saved.rbx,
                rdx: saved.rdx,
                rax: saved.rax,
                rcx: saved.rcx,
                rsp: saved.rsp,
                rip: saved.rip,
                eflags: saved.rflags,
                cs: saved.cs as u16,
                gs: 0,
                fs: 0,
                _pad0: 0,
                err: saved.error,
                trapno: saved.vector,
                oldmask: old_mask,
                cr2: queued.address,
                fpstate: 0,
                reserved: [0; 8],
            },
            sigmask: old_mask,
        };
        let signal_frame = RtSigFrame {
            restorer: action.restorer,
            context,
            info,
        };
        let ucontext_pointer = frame_pointer + 8;
        let siginfo_pointer = ucontext_pointer + core::mem::size_of::<UContext>() as u64;
        // SAFETY: `signal_frame` is a plain local value; the byte view covers
        // exactly its initialized representation and is only read here.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                &signal_frame as *const RtSigFrame as *const u8,
                core::mem::size_of::<RtSigFrame>(),
            )
        };
        if !paging::write_user_bytes(pml4, frame_pointer, bytes) {
            return false;
        }
        tasks[index].sigpending &= !bit;
        tasks[index].sigaltstack_active = used_altstack;
        let mut blocked = old_mask | action.mask;
        if action.flags & SA_NODEFER == 0 {
            blocked |= bit;
        }
        tasks[index].sigblocked = blocked;
        // SAFETY: `frame` is the live interrupt/syscall frame for this task,
        // retained on its private kernel stack while it is not running.
        unsafe {
            (*frame).rax = 0;
            (*frame).rdi = signum as u64;
            if action.flags & SA_SIGINFO != 0 {
                (*frame).rsi = siginfo_pointer;
                (*frame).rdx = ucontext_pointer;
            } else {
                (*frame).rsi = 0;
                (*frame).rdx = 0;
            }
            (*frame).rip = action.handler;
            (*frame).rsp = frame_pointer;
        }
        return true;
    }
}

/// Delivers the lowest pending unblocked signal to the current task.
pub fn linux_deliver_signal(frame: *mut InterruptFrame) -> bool {
    deliver_signal_to(CURRENT.load(Ordering::Relaxed), frame)
}

/// Consumes a signal frame and restores the interrupted context.
pub fn linux_sigreturn(frame: *mut InterruptFrame) -> bool {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &mut tasks[current];
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return false;
    };
    // The handler returned through the restorer, popping the restorer pointer;
    // RSP is now eight bytes past the start of the rt_sigframe.
    let Some(frame_pointer) = unsafe { (*frame).rsp }.checked_sub(8) else {
        return false;
    };
    let frame_size = core::mem::size_of::<RtSigFrame>() as u64;
    if !paging::user_range_valid(pml4, frame_pointer, frame_size, false) {
        return false;
    }
    // SAFETY: The complete rt_sigframe was validated as readable.
    let saved = unsafe { *(frame_pointer as *const RtSigFrame) };
    let context = saved.context;
    if context.mcontext.cs & 3 != 3
        || context.mcontext.rip < USER_BASE
        || context.mcontext.rip >= USER_LIMIT
        || context.mcontext.rsp < USER_BASE
        || context.mcontext.rsp >= USER_LIMIT
    {
        return false;
    }
    // SAFETY: The caller supplied the live interrupt frame.
    unsafe {
        (*frame).r8 = context.mcontext.r8;
        (*frame).r9 = context.mcontext.r9;
        (*frame).r10 = context.mcontext.r10;
        (*frame).r11 = context.mcontext.r11;
        (*frame).r12 = context.mcontext.r12;
        (*frame).r13 = context.mcontext.r13;
        (*frame).r14 = context.mcontext.r14;
        (*frame).r15 = context.mcontext.r15;
        (*frame).rdi = context.mcontext.rdi;
        (*frame).rsi = context.mcontext.rsi;
        (*frame).rbp = context.mcontext.rbp;
        (*frame).rbx = context.mcontext.rbx;
        (*frame).rdx = context.mcontext.rdx;
        (*frame).rax = context.mcontext.rax;
        (*frame).rcx = context.mcontext.rcx;
        (*frame).rsp = context.mcontext.rsp;
        (*frame).rip = context.mcontext.rip;
        (*frame).rflags = context.mcontext.eflags;
        (*frame).cs = context.mcontext.cs as u64;
    }
    task.sigblocked = context.sigmask;
    task.sigaltstack_active = false;
    true
}

/// Takes the saved `wait4` selector when `rt_sigreturn` must restart it.
pub fn linux_take_wait_restart() -> Option<(i64, u64)> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if !task.restart_wait {
        return None;
    }
    task.restart_wait = false;
    let pid = task.wait_pid;
    if pid == 0 {
        return None;
    }
    Some((pid, task.wait_status_pointer))
}

/// Queues and delivers a fault signal to the current task.
pub fn linux_deliver_fault(
    frame: *mut InterruptFrame,
    signum: u64,
    code: i32,
    address: u64,
) -> bool {
    let current = CURRENT.load(Ordering::Relaxed);
    if signum == 0 || signum > MAX_SIGNALS as u64 {
        return false;
    }
    queue_signal(
        current,
        signum as usize,
        SigInfo {
            code,
            pid: 0,
            uid: 0,
            address,
        },
    );
    deliver_signal_to(current, frame)
}

/// Returns true when delivery selected a default termination for the caller.
#[must_use]
pub fn linux_signal_pending_exit() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].signal_exit != 0
}

/// Applies a default-terminating signal to the current thread group.
pub fn linux_signal_exit(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    let current = CURRENT.load(Ordering::Relaxed);
    let signum = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[current].signal_exit as u64
    };
    if signum == 0 {
        return frame;
    }
    {
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].signal_exit = 0;
    }
    terminate_thread_group(current, signum);
    schedule_next(current)
}

/// Retires every live task in a thread group for a default fatal signal.
fn terminate_thread_group(index: usize, signum: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    if index >= tasks.len() {
        return;
    }
    let tgid = tasks[index].tgid;
    let mut leader = index;
    for (slot, task) in tasks.iter().enumerate() {
        if task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.tgid == tgid
            && task.kind != TaskKind::Thread
        {
            leader = slot;
            break;
        }
    }
    for (slot, task) in tasks.iter_mut().enumerate() {
        if task.state == TaskState::Empty
            || task.state == TaskState::Done
            || task.tgid != tgid
            || task.abi != TaskAbi::Linux
        {
            continue;
        }
        robust_futex_cleanup(slot);
        task.futex_address = 0;
        task.futex_deadline = 0;
        task.state = TaskState::Done;
        task.linux_fds = [None; crate::linux::MAX_FDS];
        task.linux_overlay = [None; OVERLAY_FILES];
        task.linux_eventfds = [None; MAX_EVENTFDS];
        task.linux_epolls = [None; MAX_EPOLLS];
    }
    // Pipes are global slots shared by every descriptor; dropping the group's
    // descriptors can release slots and must wake blocked peers.
    linux_release_task_pipes();
    release_linux_mappings(leader);
    tasks[leader].exit_status = signum as i64;
    serial_line!("linux: signal exit pid={leader} signal={signum}");
    wake_waiting_parent(leader);
    queue_child_signal(leader);
}

/// Queues the default-ignored SIGCHLD notification on a Linux parent.
fn queue_child_signal(child: usize) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let parent = tasks[child].parent;
    if parent == 0 || parent >= tasks.len() || parent == child {
        return;
    }
    if tasks[parent].abi != TaskAbi::Linux
        || tasks[parent].state == TaskState::Empty
        || tasks[parent].state == TaskState::Done
    {
        return;
    }
    let child_tgid = tasks[child].tgid as u32;
    let bit = 1u64 << (SIGCHLD - 1);
    if tasks[parent].sigpending & bit == 0 {
        tasks[parent].sigqueue[SIGCHLD - 1] = Some(SigInfo {
            code: 1, // CLD_EXITED; bounded for both exits and fatal signals.
            pid: child_tgid,
            uid: 0,
            address: 0,
        });
    }
    tasks[parent].sigpending |= bit;
}

/// Returns the thread-group id for `getpid`.
#[must_use]
pub fn linux_getpid() -> usize {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].tgid
}

/// Returns the parent process id, or 0 when the task has no live parent.
#[must_use]
pub fn linux_getppid() -> i64 {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &tasks[current];
    let parent = task.parent;
    if parent >= MAX_TASKS || parent == current {
        return 0;
    }
    let parent_task = &tasks[parent];
    if parent_task.state == TaskState::Empty || parent_task.state == TaskState::Done {
        0
    } else {
        parent_task.tgid as i64
    }
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
    // Linux returns the group of any live process; the session-scoped kill
    // checks below still prevent cross-session signal delivery.
    Ok(task.pgid)
}

/// Returns the session id of a live Linux process, or `ESRCH`.
pub fn linux_getsid(pid: usize) -> i64 {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let target = if pid == 0 { tasks[current].tgid } else { pid };
    tasks
        .iter()
        .find(|task| {
            task.state != TaskState::Empty
                && task.state != TaskState::Done
                && task.abi == TaskAbi::Linux
                && task.tgid == target
                && task.kind != TaskKind::Thread
        })
        .map_or(LINUX_ESRCH, |task| task.sid as i64)
}

/// Restricted Linux setpgid: current process or its direct child, same session,
/// and only an existing group in that session or a new group led by the target.
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
    // Only the caller or one of its direct children may be moved.
    if target != self_tgid && tasks[index].parent != current {
        return LINUX_EPERM;
    }
    // A session leader cannot change its process group.
    if tasks[index].sid == target || tasks[index].sid != tasks[current].sid {
        return LINUX_EPERM;
    }
    let group = if pgid == 0 { target } else { pgid };
    // A process-group leader cannot move itself into another group.
    if tasks[index].pgid == target && group != target {
        return LINUX_EPERM;
    }
    if group != target
        && !tasks.iter().any(|task| {
            task.state != TaskState::Empty
                && task.state != TaskState::Done
                && task.abi == TaskAbi::Linux
                && task.pgid == group
                && task.sid == tasks[current].sid
        })
    {
        // The requested group does not exist in the caller's session.
        return LINUX_EINVAL;
    }
    for task in tasks
        .iter_mut()
        .filter(|task| task.state != TaskState::Empty && task.tgid == target)
    {
        task.pgid = group;
    }
    0
}

/// Starts a new session for the caller, unless it is already a group leader.
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
    let sender = tasks[current].tgid as u32;
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
    if signal == 0 {
        return 0;
    }
    for (index, target) in targets.iter().enumerate() {
        if !*target {
            continue;
        }
        queue_signal(
            index,
            signal as usize,
            SigInfo {
                code: 0,
                pid: sender,
                uid: 0,
                address: 0,
            },
        );
        if index != current {
            // A scheduler-parked wait (poll/epoll/pipe/sleep) must re-execute
            // its syscall before the handler runs: that lets the handler return
            // EINTR and recover the stored deadline. Other blocked waits have
            // no rewound instruction, so their handler is installed now.
            let defer =
                tasks[index].restart_syscall != u64::MAX && pending_signal_has_handler(index);
            interrupt_blocked(index);
            if !defer {
                deliver_target(index);
            }
        }
    }
    for index in 0..tasks.len() {
        if targets[index] && index != current && tasks[index].signal_exit != 0 {
            let signum = tasks[index].signal_exit as u64;
            tasks[index].signal_exit = 0;
            terminate_thread_group(index, signum);
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
const LINUX_ECHILD: i64 = -10;
const LINUX_EEXIST: i64 = -17;
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
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Opens a read-only file, preferring an existing overlay entry.
pub fn linux_open_read(path: &str) -> Result<u64, i64> {
    if path.starts_with("/tmp/")
        && let Ok(fd) = linux_overlay_open(path, false, false)
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
    if path.len() > 64 || !crate::linux::is_dir(path) {
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
pub fn linux_getdents(fd: u64, out: &mut [u8]) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    let LinuxFileSource::Directory { path, len } = slot.source else {
        return Err(LINUX_ENOTDIR);
    };
    let path = core::str::from_utf8(&path[..len]).map_err(|_| LINUX_EBADF)?;
    let mut written = 0usize;
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
    Ok(written)
}

/// Opens or creates a file in the task's private writable overlay.
///
/// The overlay is deliberately separate from the read-only root. Only `/tmp`
/// paths are accepted, entries are bounded, and the overlay disappears with the
/// task. This is the prototype of the per-application writable layer.
pub fn linux_overlay_open(path: &str, create: bool, truncate: bool) -> Result<u64, i64> {
    if !path.starts_with("/tmp/") || !crate::linux::valid_path(path) {
        return Err(LINUX_EROFS);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
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
    }
    let index = overlay_index.unwrap();
    if truncate && let Some(entry) = task.linux_overlay[index].as_mut() {
        entry.len = 0;
    }
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Overlay(index),
                offset: 0,
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Counts open read and write ends of a global pipe slot.
fn pipe_users(index: usize) -> (usize, usize) {
    let tasks = unsafe { &*TASKS.0.get() };
    let mut readers = 0usize;
    let mut writers = 0usize;
    for task in tasks.iter() {
        for file in task.linux_fds.iter().flatten() {
            if let LinuxFileSource::Pipe { index: slot, write } = file.source
                && slot == index
            {
                if write {
                    writers += 1;
                } else {
                    readers += 1;
                }
            }
        }
    }
    (readers, writers)
}

/// Returns the pipe slot and direction for a descriptor.
#[must_use]
pub fn linux_pipe_end(fd: u64) -> Option<(usize, bool)> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    match task
        .linux_fds
        .get(fd as usize)
        .and_then(Option::as_ref)?
        .source
    {
        LinuxFileSource::Pipe { index, write } => Some((index, write)),
        _ => None,
    }
}

/// Number of open writer ends for a pipe slot.
#[must_use]
pub fn linux_pipe_writers(index: usize) -> usize {
    pipe_users(index).1
}

/// Number of open reader ends for a pipe slot.
#[must_use]
pub fn linux_pipe_readers(index: usize) -> usize {
    pipe_users(index).0
}

/// Creates a bounded pipe and returns its read and write descriptors.
pub fn linux_pipe() -> Result<(u64, u64), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(index) = (unsafe { &*PIPES.0.get() })
        .iter()
        .position(Option::is_none)
    else {
        return Err(LINUX_EMFILE);
    };
    unsafe { (*PIPES.0.get())[index] = Some(LinuxPipe::EMPTY) };
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
            });
            read_fd = Some(descriptor as u64);
        } else if write_fd.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Pipe { index, write: true },
                offset: 0,
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
            unsafe { (*PIPES.0.get())[index] = None };
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
            });
            return Ok(descriptor as u64);
        }
    }
    task.linux_eventfds[index] = None;
    unsafe { (*EVENTFD_COUNTERS.0.get())[tgid][index] = None };
    Err(LINUX_EMFILE)
}

/// Creates a monotonic timer descriptor. `CLOCK_REALTIME` (0) and
/// `CLOCK_MONOTONIC` (1) are accepted and backed by the tick clock; any other
/// clock id is rejected.
pub fn linux_timerfd_create(clockid: u64, flags: u64) -> Result<u64, i64> {
    const O_NONBLOCK: u64 = 0x800;
    const O_CLOEXEC: u64 = 0x80000;
    if clockid > 1 || flags & !(O_NONBLOCK | O_CLOEXEC) != 0 {
        return Err(LINUX_EINVAL);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(index) = task.linux_timerfds.iter().position(|timer| timer.is_none()) else {
        return Err(LINUX_EMFILE);
    };
    task.linux_timerfds[index] = Some(LinuxTimerFd::EMPTY);
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::TimerFd { index },
                offset: 0,
            });
            return Ok(descriptor as u64);
        }
    }
    task.linux_timerfds[index] = None;
    Err(LINUX_EMFILE)
}

/// Returns a timer descriptor slot for a file descriptor.
fn timerfd_slot(fd: u64) -> Result<(usize, usize), i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let task = &tasks[current];
    let Some(entry) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    let LinuxFileSource::TimerFd { index } = entry.source else {
        return Err(LINUX_EINVAL);
    };
    if !task.linux_timerfds.get(index).is_some_and(Option::is_some) {
        return Err(LINUX_EBADF);
    }
    Ok((current, index))
}

/// Arms or disarms a timer descriptor. `deadline` is an absolute tick count;
/// `None` disarms. Returns the previous remaining ticks and interval.
pub fn linux_timerfd_settime(
    fd: u64,
    deadline: Option<u64>,
    interval: u64,
) -> Result<(u64, u64), i64> {
    let (current, index) = timerfd_slot(fd)?;
    let now = TICKS.load(Ordering::Relaxed);
    let tasks = unsafe { &mut *TASKS.0.get() };
    let Some(timer) = tasks[current]
        .linux_timerfds
        .get_mut(index)
        .and_then(Option::as_mut)
    else {
        return Err(LINUX_EBADF);
    };
    let previous = if timer.armed {
        (timer.next_expiry.saturating_sub(now), timer.interval)
    } else {
        (0, 0)
    };
    match deadline {
        Some(deadline) => {
            timer.next_expiry = deadline;
            timer.interval = interval;
            timer.armed = true;
        }
        None => {
            timer.next_expiry = 0;
            timer.interval = 0;
            timer.armed = false;
        }
    }
    linux_wake_pollers();
    Ok(previous)
}

/// Returns the remaining ticks and interval of a timer descriptor.
pub fn linux_timerfd_gettime(fd: u64) -> Result<(u64, u64), i64> {
    let (current, index) = timerfd_slot(fd)?;
    let now = TICKS.load(Ordering::Relaxed);
    let tasks = unsafe { &*TASKS.0.get() };
    let Some(timer) = tasks[current]
        .linux_timerfds
        .get(index)
        .and_then(Option::as_ref)
    else {
        return Err(LINUX_EBADF);
    };
    if timer.armed {
        Ok((timer.next_expiry.saturating_sub(now), timer.interval))
    } else {
        Ok((0, 0))
    }
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
    let Some(target) = task
        .linux_fds
        .get(target_fd as usize)
        .and_then(Option::as_ref)
    else {
        return Err(LINUX_EBADF);
    };
    // Nested epoll watches would need recursive readiness evaluation; reject
    // them explicitly instead of reporting a false readiness.
    if matches!(target.source, LinuxFileSource::Epoll { .. }) {
        return Err(LINUX_EINVAL);
    }
    let Some(epoll) = task.linux_epolls.get_mut(index).and_then(Option::as_mut) else {
        return Err(LINUX_EBADF);
    };
    let existing = epoll
        .watches
        .iter()
        .position(|entry| entry.is_some_and(|watch| watch.fd == target_fd));
    match operation {
        1 => {
            if existing.is_some() {
                return Err(LINUX_EEXIST);
            }
            let Some(entry) = epoll.watches.iter_mut().find(|entry| entry.is_none()) else {
                return Err(LINUX_ENOSPC);
            };
            *entry = Some(LinuxEpollWatch {
                fd: target_fd,
                events,
                data,
                last_ready: false,
                armed: true,
            });
        }
        2 => {
            let Some(position) = existing else {
                return Err(LINUX_ENOENT);
            };
            epoll.watches[position] = None;
        }
        3 => {
            let Some(position) = existing else {
                return Err(LINUX_ENOENT);
            };
            epoll.watches[position] = Some(LinuxEpollWatch {
                fd: target_fd,
                events,
                data,
                last_ready: false,
                armed: true,
            });
        }
        _ => return Err(LINUX_EINVAL),
    }
    Ok(())
}

/// Collects ready epoll entries, updating edge/one-shot state. Level-triggered
/// watches are reported on every call while ready; edge-triggered watches only
/// on a not-ready to ready transition; one-shot watches are disarmed after a
/// report and re-armed by `EPOLL_CTL_MOD`.
pub fn linux_epoll_wait(epoll_fd: u64, out: &mut [LinuxEpollEvent]) -> Result<usize, i64> {
    let current = CURRENT.load(Ordering::Relaxed);
    let watches = {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[current];
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
        epoll.watches
    };
    let mut updated = watches;
    let mut count = 0usize;
    for watch in updated.iter_mut().flatten() {
        if count == out.len() {
            break;
        }
        let requested = watch.events & 0x1f;
        let ready = linux_poll(watch.fd, requested as i16) as u32 & requested;
        let edge = watch.events & EPOLLET != 0;
        let oneshot = watch.events & EPOLLONESHOT != 0;
        let report = watch.armed && ready != 0 && if edge { !watch.last_ready } else { true };
        if edge {
            watch.last_ready = ready != 0;
        }
        if report {
            out[count] = LinuxEpollEvent {
                events: ready,
                data: watch.data,
            };
            count += 1;
            if oneshot {
                watch.armed = false;
            }
        }
    }
    if count != 0 || updated.iter().flatten().any(|watch| watch.armed) {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[current];
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
        if let Some(epoll) = task.linux_epolls.get_mut(index).and_then(Option::as_mut) {
            epoll.watches = updated;
        }
    }
    Ok(count)
}

/// Refreshes edge-triggered watch state after a descriptor changes, so a
/// readiness drop that happens between two `epoll_wait` calls is observed and
/// the next transition is reported. Only watches in the calling task's epoll
/// instances can be refreshed here.
fn refresh_epoll_edges(fd: u64) {
    let current = CURRENT.load(Ordering::Relaxed);
    let mut updates = [(usize::MAX, false); MAX_EPOLLS * MAX_EPOLL_WATCH];
    let mut count = 0usize;
    {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[current];
        for (epoll_index, slot) in task.linux_epolls.iter().enumerate() {
            let Some(epoll) = slot else {
                continue;
            };
            for (watch_index, watch) in epoll.watches.iter().enumerate() {
                let Some(watch) = watch else {
                    continue;
                };
                if watch.fd != fd || watch.events & EPOLLET == 0 {
                    continue;
                }
                let ready = linux_poll(fd, (watch.events & 0x1f) as i16) != 0;
                updates[count] = (epoll_index * MAX_EPOLL_WATCH + watch_index, ready);
                count += 1;
            }
        }
    }
    if count == 0 {
        return;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[current];
    for (slot, ready) in updates[..count].iter() {
        let epoll_index = slot / MAX_EPOLL_WATCH;
        let watch_index = slot % MAX_EPOLL_WATCH;
        if let Some(watch) = task
            .linux_epolls
            .get_mut(epoll_index)
            .and_then(Option::as_mut)
            .and_then(|epoll| epoll.watches.get_mut(watch_index))
            .and_then(Option::as_mut)
        {
            watch.last_ready = *ready;
        }
    }
}

/// Returns poll readiness bits for a Linux descriptor.
pub fn linux_poll(fd: u64, events: i16) -> i16 {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return POLL_NVAL;
    };
    let mut ready = 0i16;
    match slot.source {
        LinuxFileSource::Pipe { index, write } => {
            let Some(pipe) = (unsafe { &*PIPES.0.get() })
                .get(index)
                .and_then(Option::as_ref)
            else {
                return POLL_NVAL;
            };
            let (readers, writers) = pipe_users(index);
            if !write {
                if events & POLL_IN != 0 && pipe.len > 0 {
                    ready |= POLL_IN;
                }
                // End-of-file is a hangup once the last writer closes.
                if writers == 0 {
                    ready |= POLL_HUP;
                }
            } else {
                if events & POLL_OUT != 0 && pipe.len < PIPE_BYTES {
                    ready |= POLL_OUT;
                }
                if readers == 0 {
                    ready |= POLL_ERR;
                }
            }
        }
        LinuxFileSource::Root(_)
        | LinuxFileSource::Overlay(_)
        | LinuxFileSource::Directory { .. } => {
            ready = events & (POLL_IN | POLL_OUT);
        }
        LinuxFileSource::EventFd { index } => {
            let counter = if task.tgid < MAX_TASKS {
                unsafe { (*EVENTFD_COUNTERS.0.get())[task.tgid][index].unwrap_or(0) }
            } else {
                0
            };
            if events & POLL_IN != 0 && counter > 0 {
                ready |= POLL_IN;
            }
            if events & POLL_OUT != 0 && counter < u64::MAX {
                ready |= POLL_OUT;
            }
        }
        LinuxFileSource::Epoll { index } => {
            let Some(epoll) = task.linux_epolls.get(index).and_then(Option::as_ref) else {
                return POLL_NVAL;
            };
            let ready_child = epoll.watches.iter().flatten().any(|watch| {
                watch.armed && linux_poll(watch.fd, (watch.events & 0x1f) as i16) != 0
            });
            if ready_child {
                ready = events & POLL_IN;
            }
        }
        LinuxFileSource::TimerFd { index } => {
            let Some(timer) = task.linux_timerfds.get(index).and_then(Option::as_ref) else {
                return POLL_NVAL;
            };
            if timer.armed && TICKS.load(Ordering::Relaxed) >= timer.next_expiry {
                ready |= events & POLL_IN;
            }
        }
        LinuxFileSource::Inotify => {
            if events & POLL_IN != 0 && task.linux_inotify.is_some_and(|watch| watch.pending_modify)
            {
                ready |= POLL_IN;
            }
        }
    }
    ready
}

/// Reads from a Linux file descriptor into a validated user buffer.
pub fn linux_read(fd: u64, out: &mut [u8]) -> Result<usize, i64> {
    let pipe = linux_pipe_end(fd);
    let pollers = {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[CURRENT.load(Ordering::Relaxed)];
        matches!(
            task.linux_fds
                .get(fd as usize)
                .and_then(Option::as_ref)
                .map(|file| file.source),
            Some(
                LinuxFileSource::EventFd { .. }
                    | LinuxFileSource::TimerFd { .. }
                    | LinuxFileSource::Inotify
            )
        )
    };
    let result = {
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
                let Some(pipe) = (unsafe { &mut (*PIPES.0.get())[index] }).as_mut() else {
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
            LinuxFileSource::TimerFd { index } => {
                let Some(timer) = task.linux_timerfds.get(index).and_then(Option::as_ref) else {
                    return Err(LINUX_EBADF);
                };
                let timer = *timer;
                let now = TICKS.load(Ordering::Relaxed);
                if !timer.armed || now < timer.next_expiry {
                    return Err(LINUX_EAGAIN);
                }
                if out.len() < 8 {
                    return Err(LINUX_EINVAL);
                }
                let (count, next, armed) = if timer.interval == 0 {
                    (1u64, 0u64, false)
                } else {
                    let elapsed = now - timer.next_expiry;
                    let count = elapsed.checked_div(timer.interval).unwrap_or(0) + 1;
                    (
                        count,
                        timer
                            .next_expiry
                            .saturating_add(count.saturating_mul(timer.interval)),
                        true,
                    )
                };
                if let Some(timer) = task.linux_timerfds.get_mut(index).and_then(Option::as_mut) {
                    timer.next_expiry = next;
                    timer.armed = armed;
                    if !armed {
                        timer.interval = 0;
                    }
                }
                out[..8].copy_from_slice(&count.to_le_bytes());
                Ok(8)
            }
            LinuxFileSource::Inotify => {
                let watch = task.linux_inotify.as_mut().ok_or(LINUX_EBADF)?;
                if !watch.pending_modify {
                    return Err(LINUX_EAGAIN);
                }
                if out.len() < 16 {
                    return Err(LINUX_EINVAL);
                }
                out[..16].fill(0);
                out[..4].copy_from_slice(&1i32.to_ne_bytes());
                out[4..8].copy_from_slice(&2u32.to_ne_bytes());
                watch.pending_modify = false;
                Ok(16)
            }
        }
    };
    if result.is_ok() {
        if let Some((index, false)) = pipe {
            linux_wake_pipe(index);
        }
        if pipe.is_some() || pollers {
            linux_wake_pollers();
        }
    }
    if result.is_ok() && pipe.is_some() {
        refresh_epoll_edges(fd);
    }
    result
}

/// Writes to a writable overlay or pipe descriptor.
pub fn linux_write_file(fd: u64, data: &[u8]) -> Result<usize, i64> {
    let pipe = linux_pipe_end(fd);
    let result = {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
        let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
            return Err(LINUX_EBADF);
        };
        match slot.source {
            LinuxFileSource::Overlay(index) => {
                let Some(entry) = task.linux_overlay.get_mut(index).and_then(Option::as_mut) else {
                    return Err(LINUX_EBADF);
                };
                let end = slot.offset.checked_add(data.len()).ok_or(LINUX_ENOSPC)?;
                if end > OVERLAY_BYTES {
                    return Err(LINUX_ENOSPC);
                }
                entry.data[slot.offset..end].copy_from_slice(data);
                entry.len = entry.len.max(end);
                slot.offset = end;
                let path = entry.path();
                if let Some(watch) = task.linux_inotify.as_mut()
                    && watch.watched
                    && &watch.path[..watch.path_len] == path.as_bytes()
                {
                    watch.pending_modify = true;
                }
                Ok(data.len())
            }
            LinuxFileSource::Pipe { index, write: true } => {
                let Some(pipe) = (unsafe { &mut (*PIPES.0.get())[index] }).as_mut() else {
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
    };
    if result.is_ok() {
        if let Some((index, true)) = pipe {
            linux_wake_pipe(index);
        }
        if pipe.is_some() {
            linux_wake_pollers();
        }
    }
    if result.is_ok() && pipe.is_some() {
        refresh_epoll_edges(fd);
    }
    result
}

/// Creates one inotify descriptor per task. Only `IN_NONBLOCK` is supported;
/// flags 0 would request Linux's blocking behavior, which this prototype lacks.
pub fn linux_inotify_init(flags: u64) -> Result<u64, i64> {
    const IN_NONBLOCK: u64 = 0x800;
    if flags != IN_NONBLOCK {
        return Err(LINUX_EINVAL);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.linux_inotify.is_some() {
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
    task.linux_inotify = Some(LinuxInotify::EMPTY);
    task.linux_fds[fd] = Some(LinuxFile {
        source: LinuxFileSource::Inotify,
        offset: 0,
    });
    Ok(fd as u64)
}

/// Installs one exact-path `/tmp` watch for IN_MODIFY only (mask 0x2).
pub fn linux_inotify_add(fd: u64, path: &str, mask: u32) -> Result<i32, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if !matches!(
        task.linux_fds.get(fd as usize).and_then(Option::as_ref),
        Some(LinuxFile {
            source: LinuxFileSource::Inotify,
            ..
        })
    ) {
        return Err(LINUX_EBADF);
    }
    if mask != 2
        || !path.starts_with("/tmp/")
        || !crate::linux::valid_path(path)
        || path.len() > OVERLAY_PATH
    {
        return Err(LINUX_EINVAL);
    }
    if !task
        .linux_overlay
        .iter()
        .flatten()
        .any(|entry| entry.path() == path)
    {
        return Err(LINUX_ENOENT);
    }
    let watch = task.linux_inotify.as_mut().ok_or(LINUX_EBADF)?;
    watch.path.fill(0);
    watch.path[..path.len()].copy_from_slice(path.as_bytes());
    watch.path_len = path.len();
    watch.watched = true;
    watch.pending_modify = false;
    Ok(1)
}

pub fn linux_inotify_remove(fd: u64, wd: u64) -> Result<(), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if !matches!(
        task.linux_fds.get(fd as usize).and_then(Option::as_ref),
        Some(LinuxFile {
            source: LinuxFileSource::Inotify,
            ..
        })
    ) {
        return Err(LINUX_EBADF);
    }
    if wd != 1 {
        return Err(LINUX_EINVAL);
    }
    let watch = task.linux_inotify.as_mut().ok_or(LINUX_EBADF)?;
    if !watch.watched {
        return Err(LINUX_EINVAL);
    }
    watch.watched = false;
    watch.pending_modify = false;
    Ok(())
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
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. } => Err(LINUX_ESPIPE),
        LinuxFileSource::Inotify => Err(LINUX_ESPIPE),
    }
}

/// Closes a Linux file descriptor.
pub fn linux_close(fd: u64) -> i64 {
    let current = CURRENT.load(Ordering::Relaxed);
    let pipe = {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[current];
        let Some(slot) = task.linux_fds.get_mut(fd as usize) else {
            return LINUX_EBADF;
        };
        let source = slot.as_ref().map(|file| file.source);
        if slot.take().is_none() {
            return LINUX_EBADF;
        }
        let mut pipe = None;
        match source {
            Some(LinuxFileSource::Pipe { index, .. }) => pipe = Some(index),
            Some(LinuxFileSource::Epoll { index })
                if !task.linux_fds.iter().flatten().any(
                    |file| matches!(file.source, LinuxFileSource::Epoll { index: slot } if slot == index)
                ) =>
            {
                if let Some(epoll) = task.linux_epolls.get_mut(index) {
                    *epoll = None;
                }
            }
            Some(LinuxFileSource::TimerFd { index }) => {
                if !task
                    .linux_fds
                    .iter()
                    .flatten()
                    .any(|file| matches!(file.source, LinuxFileSource::TimerFd { index: slot } if slot == index))
                    && let Some(timer) = task.linux_timerfds.get_mut(index)
                {
                    *timer = None;
                }
            }
            Some(LinuxFileSource::EventFd { index })
                if !task.linux_fds.iter().flatten().any(
                    |file| matches!(file.source, LinuxFileSource::EventFd { index: slot } if slot == index)
                ) =>
            {
                if let Some(eventfd) = task.linux_eventfds.get_mut(index) {
                    *eventfd = None;
                }
                let tgid = task.tgid;
                if tgid < MAX_TASKS {
                    unsafe { (*EVENTFD_COUNTERS.0.get())[tgid][index] = None };
                }
            }
            Some(LinuxFileSource::Inotify)
                if !task
                    .linux_fds
                    .iter()
                    .flatten()
                    .any(|file| matches!(file.source, LinuxFileSource::Inotify)) =>
            {
                task.linux_inotify = None;
            }
            _ => {}
        }
        pipe
    };
    if let Some(index) = pipe {
        linux_release_pipe(index);
        linux_wake_pipe(index);
        linux_wake_pollers();
    }
    0
}

/// Frees a global pipe slot once no task references either end.
fn linux_release_pipe(index: usize) {
    if index >= MAX_PIPES {
        return;
    }
    let (readers, writers) = pipe_users(index);
    if readers == 0 && writers == 0 {
        unsafe { (*PIPES.0.get())[index] = None };
    }
}

/// Releases pipe slots that no task references after an exit or fault and
/// wakes pipe peers so they observe end-of-file or a closed reader.
fn linux_release_task_pipes() {
    for index in 0..MAX_PIPES {
        linux_release_pipe(index);
    }
    linux_wake_matching(|kind| {
        matches!(
            kind,
            LinuxWaitKind::PipeRead(_) | LinuxWaitKind::PipeWrite(_)
        )
    });
    linux_wake_pollers();
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
        | LinuxFileSource::TimerFd { .. } => 0,
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
    crate::linux::lookup(path).is_some() || crate::linux::is_dir(path)
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
        LinuxFileSource::Directory { .. } => Ok((0, 2, 0o040755)),
        LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. } => Ok((0, 0, 0o010600)),
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
        | LinuxFileSource::TimerFd { .. } => Ok(0),
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
        robust_futex_cleanup(current);
        release_linux_mappings(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].futex_address = 0;
        tasks[current].futex_deadline = 0;
        tasks[current].state = TaskState::Done;
        tasks[current].exit_status = ((status & 0xff) << 8) as i64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
        serial_line!("linux: process exit pid={current} status={status}");
        linux_release_task_pipes();
        wake_waiting_parent(current);
        queue_child_signal(current);
        return schedule_next(current);
    }
    if kind == TaskKind::Thread {
        robust_futex_cleanup(current);
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].futex_address = 0;
        tasks[current].futex_deadline = 0;
        tasks[current].state = TaskState::Done;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
        serial_line!("linux: thread exit tid={current} status={status}");
        linux_release_task_pipes();
        return schedule_next(current);
    }
    if (current != LINUX_TASK && current != LINUX_DYN_TASK) || kind != TaskKind::Linux {
        serial_line!("AETHER_BOOT_FAILED reason=linux exit from wrong task");
        halt()
    }

    robust_futex_cleanup(current);
    release_linux_mappings(current);
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[current].futex_address = 0;
    tasks[current].futex_deadline = 0;
    tasks[current].state = TaskState::Done;
    tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
    tasks[current].linux_overlay = [None; OVERLAY_FILES];
    tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
    tasks[current].linux_epolls = [None; MAX_EPOLLS];
    tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
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
    linux_release_task_pipes();
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

pub fn on_user_fault(frame: *mut InterruptFrame, vector: u64, address: u64) -> *mut InterruptFrame {
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
        robust_futex_cleanup(current);
        release_linux_mappings(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Done;
        tasks[current].exit_status = 128 + vector as i64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
        serial_line!("linux: process fault pid={current} vector={vector}");
        linux_release_task_pipes();
        wake_waiting_parent(current);
        queue_child_signal(current);
        return schedule_next(current);
    }
    if thread {
        robust_futex_cleanup(current);
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Faulted;
        tasks[current].saved_rsp = frame as u64;
        tasks[current].linux_fds = [None; crate::linux::MAX_FDS];
        tasks[current].linux_overlay = [None; OVERLAY_FILES];
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
        serial_line!("linux: thread fault tid={current} vector={vector}");
        linux_release_task_pipes();
        return schedule_next(current);
    }

    if linux {
        let signum = if vector == 14 { 7 } else { 11 };
        let code = if vector == 14 { 1 } else { 0x80 };
        let fault_address = if vector == 14 { address } else { 0 };
        if linux_deliver_fault(frame, signum, code, fault_address) {
            let tasks = unsafe { &mut *TASKS.0.get() };
            tasks[current].state = TaskState::Running;
            return frame;
        }
        robust_futex_cleanup(current);
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
        tasks[current].linux_eventfds = [None; MAX_EVENTFDS];
        tasks[current].linux_epolls = [None; MAX_EPOLLS];
        tasks[current].linux_timerfds = [None; MAX_TIMERFDS];
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
    linux_release_task_pipes();
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
    let default_exit = {
        let tasks = unsafe { &mut *TASKS.0.get() };
        if tasks[current].signal_exit != 0 {
            let signum = tasks[current].signal_exit as u64;
            tasks[current].signal_exit = 0;
            Some(signum)
        } else {
            None
        }
    };
    if let Some(signum) = default_exit {
        terminate_thread_group(current, signum);
        return schedule_next(current);
    }
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
        // Sleep, poll, epoll, and pipe waits re-check their own condition when
        // resumed, so only the deadline expiry needs a wake here.
        if task.state == TaskState::Blocked && task.wait_deadline != 0 && now >= task.wait_deadline
        {
            task.wait_kind = LinuxWaitKind::None;
            task.wait_deadline = 0;
            task.state = TaskState::Ready;
        }
    }

    // An armed timerfd crossing its deadline wakes poll/epoll waiters so they
    // can report the descriptor as readable. The handlers re-check the timer.
    let timer_expired = tasks.iter().any(|task| {
        task.linux_timerfds
            .iter()
            .flatten()
            .any(|timer| timer.armed && now >= timer.next_expiry)
    });
    if timer_expired {
        for task in tasks.iter_mut() {
            if task.state == TaskState::Blocked && task.wait_kind == LinuxWaitKind::Poll {
                task.wait_kind = LinuxWaitKind::None;
                task.state = TaskState::Ready;
            }
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

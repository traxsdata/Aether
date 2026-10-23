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
use aether_abi::user::{
    ARGUMENT_BLOCK_BYTES, MAX_ARGUMENTS, PROCESS_FLAG_SESSION, PROCESS_FLAG_USER,
    PROCESS_NAME_BYTES, ProcessKind, ProcessRecord, ProcessState, USER_BASE, USER_LIMIT,
    USER_STACK_PAGES, USER_STACK_TOP, UserTestResult, deadline_elapsed, errno, fixed_name,
    sleep_wake_deadline,
};
use aether_boot_common::elf;
use aether_fs::FsError;
use aether_ipc::{CapError, CapHandle, CapTable, IpcError, ObjectType, Rights};
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use aether_memory::vma::{VmaError, VmaTable};
use aether_services::ServiceError;
use aether_services::runtime::RuntimeError;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

/// Number of ring-0 worker tasks in the isolation test.
pub const WORKER_COUNT: usize = 3;
/// Task index of the ring-3 process.
pub const USER_TASK: usize = WORKER_COUNT + 1;
/// Task index of the Linux ABI boot test.
pub const LINUX_TASK: usize = USER_TASK + 1;
/// Task index of the dynamic Linux loader test.
pub const LINUX_DYN_TASK: usize = LINUX_TASK + 1;
/// First task index of the programs launched from imported user AppImages.
pub const APPIMAGE_TASK: usize = LINUX_DYN_TASK + 1;
/// Task index of the ring-3 `linuxd` launch-policy service.
pub const LINUXD_TASK: usize = APPIMAGE_TASK + crate::linux::MAX_APPS;

/// Returns `true` for a reserved imported-app task index.
const fn is_app_task(index: usize) -> bool {
    index >= APPIMAGE_TASK && index < APPIMAGE_TASK + crate::linux::MAX_APPS
}

// Linux-sized bounds for large applications such as Firefox: a browser runs
// several processes with dozens of threads each.
#[cfg(feature = "large-apps")]
const MAX_TASKS: usize = WORKER_COUNT + 512;
#[cfg(all(feature = "gate4-gui-test", not(feature = "large-apps")))]
const MAX_TASKS: usize = WORKER_COUNT + 96;
// Room for the boot tasks, a terminal session (the shell, a pipeline, and a
// full-screen program), and the application thread tables. Feature builds keep
// their larger budgets.
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
const MAX_TASKS: usize = WORKER_COUNT + 64;

/// Maximum number of `MAP_SHARED` VMA entries per Linux application domain.
#[cfg(feature = "gate4-gui-test")]
const MAX_DOMAIN_SHARED_VMAS: usize = 64;
#[cfg(not(feature = "gate4-gui-test"))]
const MAX_DOMAIN_SHARED_VMAS: usize = 16;
const STACK_SIZE: usize = 16 * 1024;
#[cfg(any(feature = "gate4-gui-test", feature = "large-apps"))]
const KERNEL_STACK_SIZE: usize = 256 * 1024;
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
const KERNEL_STACK_SIZE: usize = 32 * 1024;
const ROUNDS: usize = 4;
const MAGIC_PREFIX: u64 = 0xa5a5_0000_0000_0000;

/// Native user program embedded by the kernel build.
static USER_ELF: &[u8] = include_bytes!(env!("AETHER_USER_ELF"));
/// Native ring-3 `linuxd` service embedded by the kernel build.
static LINUXD_ELF: &[u8] = include_bytes!(env!("AETHER_LINUXD_ELF"));
/// Native userland (shell and commands) started by the terminal.
static USERLAND_ELF: &[u8] = include_bytes!(env!("AETHER_USERLAND_ELF"));
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
    /// Native ring-3 service with a restricted syscall surface.
    Service,
    Linux,
    Thread,
    Process,
    /// Native ring-3 program started in the terminal session.
    Terminal,
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
#[allow(clippy::large_enum_variant)]
// `Portal` carries a full filesystem backing path, so the variant is wide.
// It lives per-descriptor in static task state, and 512-byte copies are safe
// on even the 16 KiB ring-0 worker stacks.
enum LinuxFileSource {
    Root(usize),
    Overlay(usize),
    /// An exact-file portal grant resolved to a kernel filesystem backing path.
    Portal {
        path: [u8; PORTAL_PATH_BYTES],
        len: u16,
        rights: u8,
    },
    /// A bounded staging slot for `<granted-file>.sctmp`.
    PortalTemp {
        index: usize,
        generation: u32,
        rights: u8,
    },
    Directory {
        path: [u8; 64],
        len: usize,
    },
    Pipe {
        index: usize,
        write: bool,
    },
    EventFd {
        index: usize,
    },
    Epoll {
        index: usize,
    },
    TimerFd {
        index: usize,
    },
    Inotify,
    Pseudo(PseudoFile),
    PseudoDir(PseudoDir),
    /// Anonymous shared-memory object page owner.
    MemFd {
        index: usize,
    },
    /// The task's one synthetic display endpoint.
    Display,
    /// A slot in the global socket table (`crate::socket`). Descriptors that
    /// share a slot share the socket, like one Linux open file description.
    Socket {
        index: usize,
    },
    /// The serial console. Descriptors 0, 1 and 2 start here, and `dup2`
    /// can replace them like any other descriptor. Writes go to the serial
    /// port; reads report end-of-file because the console has no input.
    Console,
}

/// Maximum backing-path bytes carried by a portal descriptor.
const PORTAL_PATH_BYTES: usize = aether_fs::MAX_PATH;
const PIPE_BYTES: usize = 512;
/// Global pipe slots. Forked processes share the same slot, so a write on one
/// side is visible to the other.
#[cfg(feature = "large-apps")]
const MAX_PIPES: usize = 128;
#[cfg(all(feature = "gate4-gui-test", not(feature = "large-apps")))]
const MAX_PIPES: usize = 16;
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
const MAX_PIPES: usize = 4;
#[cfg(feature = "large-apps")]
const MAX_EVENTFDS: usize = 64;
#[cfg(all(feature = "gate4-gui-test", not(feature = "large-apps")))]
const MAX_EVENTFDS: usize = 16;
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
const MAX_EVENTFDS: usize = 2;
#[cfg(feature = "large-apps")]
const MAX_EPOLLS: usize = 16;
#[cfg(all(feature = "gate4-gui-test", not(feature = "large-apps")))]
const MAX_EPOLLS: usize = 8;
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
const MAX_EPOLLS: usize = 1;
// Firefox's IPC I/O thread watches one descriptor per child channel.
#[cfg(feature = "large-apps")]
const MAX_EPOLL_WATCH: usize = 64;
#[cfg(not(feature = "large-apps"))]
const MAX_EPOLL_WATCH: usize = 8;
#[cfg(feature = "large-apps")]
const MAX_TIMERFDS: usize = 32;
#[cfg(all(feature = "gate4-gui-test", not(feature = "large-apps")))]
const MAX_TIMERFDS: usize = 8;
#[cfg(not(any(feature = "gate4-gui-test", feature = "large-apps")))]
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
    /// Created on `CLOCK_REALTIME`: absolute deadlines are wall-clock time.
    realtime: bool,
}

impl LinuxTimerFd {
    const EMPTY: Self = Self {
        next_expiry: 0,
        interval: 0,
        armed: false,
        realtime: false,
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
    /// A `vfork` parent waiting for the child (by tgid) to exec or exit.
    Vfork(usize),
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
    /// `/etc/resolv.conf`, generated from the DHCP lease.
    ResolvConf,
}

/// Pseudo-directory enumerated by `getdents64`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PseudoDir {
    Proc,
    ProcSelf,
    ProcSelfFd,
    Dev,
}

const OVERLAY_FILES: usize = 16;
/// Per-file overlay capacity. Gate 3 writes a small CSV plus a temporary save
/// file, so 4 KiB leaves comfortable headroom without a heap allocation.
const OVERLAY_BYTES: usize = 4096;
const OVERLAY_PATH: usize = 64;
/// Total writable-overlay bytes allowed per application domain.
pub const OVERLAY_APP_BYTES: usize = 8192;

const MAX_INOTIFY_WATCHES: usize = 4;
const MAX_INOTIFY_EVENTS: usize = 8;
const INOTIFY_NAME_BYTES: usize = 64;
const IN_WATCH_PATH_BYTES: usize = 64;

#[derive(Clone, Copy)]
struct LinuxOverlay {
    /// Application store this entry belongs to.
    app: u8,
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
        app: 0,
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
    /// `si_errno`; the `SECCOMP_RET_DATA` of a seccomp trap.
    errno: i32,
    /// `si_syscall` of a seccomp trap.
    syscall: i32,
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
const SIGSYS: usize = 31;
/// `si_code` of a seccomp `SIGSYS`.
const SYS_SECCOMP: i32 = 1;

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
    /// Mapping tables of the owning process, shared by its threads like the
    /// descriptor views below.
    vmas: VmaSlots,
    anonymous_vmas: AnonymousVmaSlots,
    shared_vmas: SharedVmaSlots,
    brk: u64,
    fs_base: u64,
    tgid: usize,
    pgid: usize,
    sid: usize,
    /// Key of the futex word this task waits on: a physical address for a
    /// `MAP_SHARED` frame (`futex_space == 0`), otherwise the virtual address
    /// in the address space named by `futex_space`. Zero when not waiting.
    futex_address: u64,
    futex_space: u64,
    futex_deadline: u64,
    clear_child_tid: u64,
    /// Descriptors of the owning process. The threads of one process share
    /// the table, so the view names the thread group id.
    linux_fds: FdSlots,
    /// Imported app-domain slot; `crate::linux::MAX_APPS` for native tasks.
    app_slot: usize,
    /// Path of the running Linux executable, reported by
    /// `readlink("/proc/self/exe")`. Empty for the boot fixtures.
    exe_path: ExePath,
    /// Per-application overlay store selected by the runtime manager.
    overlay_app: u8,
    inotify_wait_pointer: u64,
    inotify_wait_length: usize,
    // Descriptor-local allocation map. The slots live in the per-process
    // table so that cloned threads share them; the event counters live in
    // EVENTFD_COUNTERS and are addressed by (tgid, slot).
    linux_eventfds: EventfdSlots,
    linux_epolls: EpollSlots,
    linux_timerfds: TimerfdSlots,
    wait_kind: LinuxWaitKind,
    /// Most recent Linux system call number; used by the Gate 4 block trace.
    #[allow(dead_code)]
    last_syscall: u64,
    /// Last system call number printed by the Gate 4 call trace.
    #[allow(dead_code)]
    traced_syscall: u64,
    wait_deadline: u64,
    restart_syscall: u64,
    restart_deadline: u64,
    restart_infinite: bool,
    /// Address of the rewound `syscall` instruction of a parked wait.
    restart_rip: u64,
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
    /// Boot reserved this slot for a task that is created after an approval.
    reserved: bool,
    /// Narrow display-endpoint grant. Set only for the boot display fixture and
    /// a verified GridCraft AppImage; never inherited through `fork`.
    display: bool,
    /// Writable+executable mappings allowed. Set only from a verified
    /// AppImage record that declares `memory=jit`; threads, `fork` and
    /// `execve` keep it, since they stay inside the same application.
    jit: bool,
    /// Eligible to ask for outbound network connections. Set only from a
    /// verified AppImage record that declares `network=client`; inherited
    /// like `jit`. The user's consent is checked on every socket.
    network_client: bool,
    /// Stacked seccomp filters, newest first. Threads and forks share the
    /// chain, `execve` keeps it, and nothing can remove a filter.
    seccomp: Option<Rc<crate::seccomp::Filter>>,
    /// `SECCOMP_MODE_STRICT`: only read, write, exit, and rt_sigreturn.
    seccomp_strict: bool,
    /// `PR_SET_NO_NEW_PRIVS`; required before a filter can be installed and
    /// never cleared.
    no_new_privs: bool,
    /// `PR_SET_DUMPABLE`. Nothing reads another process's memory, so the
    /// flag is bookkeeping for programs that check it.
    dumpable: bool,
    /// Terminal session attachment and standard input/output routing.
    io: crate::terminal::TaskIo,
    /// Timer ticks this task was interrupted on the CPU.
    cpu_ticks: u64,
    /// Frames allocated for this task's address space, in KiB.
    memory_kib: u32,
    /// Display name for process listings (NUL padded).
    name: [u8; PROCESS_NAME_BYTES],
    /// Argument block of a terminal program (see `Syscall::Spawn`).
    args: [u8; ARGUMENT_BLOCK_BYTES],
    args_len: u16,
}

impl Task {
    /// Points the descriptor views at the process that owns this task.
    ///
    /// The threads of one process share a descriptor table, so the views name
    /// the thread group id rather than the task index.
    fn bind_file_state(&mut self, tgid: usize) {
        ensure_file_state(tgid);
        self.linux_fds = FdSlots(tgid);
        self.linux_eventfds = EventfdSlots(tgid);
        self.linux_epolls = EpollSlots(tgid);
        self.linux_timerfds = TimerfdSlots(tgid);
        self.vmas = VmaSlots(tgid);
        self.anonymous_vmas = AnonymousVmaSlots(tgid);
        self.shared_vmas = SharedVmaSlots(tgid);
    }

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
            vmas: VmaSlots(0),
            anonymous_vmas: AnonymousVmaSlots(0),
            shared_vmas: SharedVmaSlots(0),
            brk: 0,
            fs_base: 0,
            tgid: 0,
            pgid: 0,
            sid: 0,
            futex_address: 0,
            futex_space: 0,
            futex_deadline: 0,
            clear_child_tid: 0,
            linux_fds: FdSlots(0),
            app_slot: crate::linux::MAX_APPS,
            exe_path: ExePath::EMPTY,
            overlay_app: 0,
            inotify_wait_pointer: 0,
            inotify_wait_length: 0,
            linux_eventfds: EventfdSlots(0),
            linux_epolls: EpollSlots(0),
            linux_timerfds: TimerfdSlots(0),
            wait_kind: LinuxWaitKind::None,
            last_syscall: u64::MAX,
            traced_syscall: u64::MAX,
            wait_deadline: 0,
            restart_syscall: u64::MAX,
            restart_deadline: 0,
            restart_infinite: false,
            restart_rip: 0,
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
            reserved: false,
            display: false,
            jit: false,
            network_client: false,
            seccomp: None,
            seccomp_strict: false,
            no_new_privs: false,
            dumpable: true,
            io: crate::terminal::TaskIo::NONE,
            cpu_ticks: 0,
            memory_kib: 0,
            name: [0; PROCESS_NAME_BYTES],
            args: [0; ARGUMENT_BLOCK_BYTES],
            args_len: 0,
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
/// Writable overlay entries, keyed by application store and path.
///
/// Entries intentionally outlive the task that created them. They are cleared
/// only by the user-visible erasure path, so settings/logs survive a process
/// exit within a boot. The store is RAM-backed and does not survive a power
/// cycle because the filesystem service has no persistent block device.
static OVERLAYS: Global<[Option<LinuxOverlay>; OVERLAY_FILES]> =
    Global(UnsafeCell::new([None; OVERLAY_FILES]));
/// Saved extended processor state, one area per task.
///
/// The area is large enough for the legacy FXSAVE block, the XSAVE header and
/// the 256-byte YMM upper-half component (832 bytes); 1024 bytes leaves the
/// required 64-byte alignment headroom.
#[repr(align(64))]
#[derive(Clone, Copy)]
// The bytes are only ever addressed through `fpu_save`/`fpu_restore`.
#[allow(dead_code)]
struct FpuState([u8; 1024]);
const FPU_STATE_ZERO: FpuState = FpuState([0; 1024]);
static FPU_STATES: Global<[FpuState; MAX_TASKS]> =
    Global(UnsafeCell::new([FPU_STATE_ZERO; MAX_TASKS]));
static FPU_XSAVE: AtomicBool = AtomicBool::new(false);
/// Architectural default vector state, captured once at boot. A recycled task
/// slot starts from it rather than from the previous owner's registers.
static FPU_DEFAULT: Global<FpuState> = Global(UnsafeCell::new(FPU_STATE_ZERO));
static EVENTFD_COUNTERS: Global<[[Option<u64>; MAX_EVENTFDS]; MAX_TASKS]> =
    Global(UnsafeCell::new([[None; MAX_EVENTFDS]; MAX_TASKS]));
/// Pipe storage is global so `fork`-inherited descriptors share one buffer.
/// A slot is freed once no task references either end.
static PIPES: Global<[Option<LinuxPipe>; MAX_PIPES]> = Global(UnsafeCell::new([None; MAX_PIPES]));
/// One inotify instance per process id (tgid). Threads share it; fork copies it.
static INOTIFY: Global<[Option<LinuxInotify>; MAX_TASKS]> =
    Global(UnsafeCell::new([None; MAX_TASKS]));

/// Descriptor state of one Linux process.
///
/// The threads of a process share one descriptor table, exactly like Linux
/// `CLONE_FILES`. The state is addressed by the thread group id, so a
/// descriptor created by one thread is immediately visible to the other
/// threads of the same process. `fork` copies the table into the child's own
/// slot; `clone` keeps the parent's slot.
struct LinuxFileState {
    // The tables are heap-allocated when a process slot is bound and freed
    // when the process is gone, so large descriptor limits cost memory only
    // for live processes and do not grow the kernel image.
    fds: Vec<Option<LinuxFile>>,
    eventfds: Vec<Option<()>>,
    epolls: Vec<Option<LinuxEpoll>>,
    timerfds: Vec<Option<LinuxTimerFd>>,
}

impl LinuxFileState {
    const fn empty() -> Self {
        Self {
            fds: Vec::new(),
            eventfds: Vec::new(),
            epolls: Vec::new(),
            timerfds: Vec::new(),
        }
    }

    fn new() -> Self {
        const CONSOLE: Option<LinuxFile> = Some(LinuxFile {
            source: LinuxFileSource::Console,
            offset: 0,
            writable: true,
        });
        let mut fds = vec![None; crate::linux::MAX_FDS];
        fds[0] = CONSOLE;
        fds[1] = CONSOLE;
        fds[2] = CONSOLE;
        Self {
            fds,
            eventfds: vec![None; MAX_EVENTFDS],
            epolls: vec![None; MAX_EPOLLS],
            timerfds: vec![None; MAX_TIMERFDS],
        }
    }
}

static FILE_STATE: Global<[LinuxFileState; MAX_TASKS]> = Global(UnsafeCell::new(
    [const { LinuxFileState::empty() }; MAX_TASKS],
));

/// Declares a view of one shared descriptor slot table.
///
/// The view keeps the existing `task.linux_fds[..]` call syntax while the
/// storage lives in the per-process table, so every descriptor access resolves
/// to the owning process.
macro_rules! process_slot_view {
    ($name:ident, $field:ident, $type:ty) => {
        /// A view of one process's shared descriptor slot table.
        #[derive(Clone, Copy)]
        struct $name(usize);

        impl core::ops::Deref for $name {
            type Target = [$type];

            fn deref(&self) -> &Self::Target {
                // SAFETY: The table is static and the kernel is single-CPU;
                // see the module documentation on the serialization model.
                unsafe { &(*FILE_STATE.0.get())[self.0].$field }
            }
        }

        impl core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                // SAFETY: as above.
                unsafe { &mut (*FILE_STATE.0.get())[self.0].$field }
            }
        }
    };
}

process_slot_view!(FdSlots, fds, Option<LinuxFile>);
process_slot_view!(EventfdSlots, eventfds, Option<()>);
process_slot_view!(EpollSlots, epolls, Option<LinuxEpoll>);
process_slot_view!(TimerfdSlots, timerfds, Option<LinuxTimerFd>);

/// Mapping tables of one Linux process.
///
/// Every thread of a process shares one address space, so the tables live per
/// thread group like the descriptor table: an `mmap` in one thread is one
/// update, not a copy per sibling.
struct LinuxAddressSpace {
    /// Every tracked mapping, including the program images.
    vmas: VmaTable,
    /// Private anonymous mappings, which may be demand-paged.
    anonymous: VmaTable,
    /// `MAP_SHARED` mappings, charged to the domain's shared quota.
    shared: VmaTable,
}

impl LinuxAddressSpace {
    const fn new() -> Self {
        Self {
            vmas: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
            anonymous: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
            shared: VmaTable::with_window(USER_BASE, paging::LINUX_MMAP_BASE, USER_LIMIT),
        }
    }

    fn clone_from_process(&mut self, other: &Self) {
        self.vmas = other.vmas.clone();
        self.anonymous = other.anonymous.clone();
        self.shared = other.shared.clone();
    }
}

static ADDRESS_SPACES: Global<[LinuxAddressSpace; MAX_TASKS]> = Global(UnsafeCell::new(
    [const { LinuxAddressSpace::new() }; MAX_TASKS],
));

/// Declares a view of one process's mapping table, like
/// [`process_slot_view`] does for descriptors.
macro_rules! process_vma_view {
    ($name:ident, $field:ident) => {
        /// A view of one process's shared mapping table.
        #[derive(Clone, Copy)]
        struct $name(usize);

        impl core::ops::Deref for $name {
            type Target = VmaTable;

            fn deref(&self) -> &Self::Target {
                // SAFETY: The table is static and the kernel is single-CPU;
                // see the module documentation on the serialization model.
                unsafe { &(*ADDRESS_SPACES.0.get())[self.0].$field }
            }
        }

        impl core::ops::DerefMut for $name {
            fn deref_mut(&mut self) -> &mut Self::Target {
                // SAFETY: as above.
                unsafe { &mut (*ADDRESS_SPACES.0.get())[self.0].$field }
            }
        }
    };
}

process_vma_view!(VmaSlots, vmas);
process_vma_view!(AnonymousVmaSlots, anonymous);
process_vma_view!(SharedVmaSlots, shared);

/// Allocates the standard descriptor table for a process slot that has none.
fn ensure_file_state(tgid: usize) {
    // SAFETY: Single-CPU scheduler state; callers run with interrupts masked.
    let state = unsafe { &mut *FILE_STATE.0.get() };
    if let Some(slot) = state.get_mut(tgid)
        && slot.fds.is_empty()
    {
        *slot = LinuxFileState::new();
    }
}

/// Clears the descriptor table of one process.
///
/// The table belongs to the whole thread group, so this runs only when the
/// last thread of a process is gone.
fn clear_file_state(tgid: usize) {
    if tgid >= MAX_TASKS {
        return;
    }
    // SAFETY: Single-CPU exit path with interrupts masked.
    let state = unsafe { &mut *FILE_STATE.0.get() };
    state[tgid] = LinuxFileState::empty();
}

#[repr(align(16))]
struct WorkerStacks([[u8; STACK_SIZE]; WORKER_COUNT]);

static STACKS: Global<WorkerStacks> = Global(UnsafeCell::new(WorkerStacks(
    [[0; STACK_SIZE]; WORKER_COUNT],
)));
/// Physical base of each task slot's kernel stack, or zero before first use.
///
/// Stacks come from the frame allocator the first time a slot runs a task and
/// stay with the slot for reuse, so the kernel image does not reserve
/// `MAX_TASKS` stacks in `.bss` up front.
static KERNEL_STACKS: Global<[u64; MAX_TASKS]> = Global(UnsafeCell::new([0; MAX_TASKS]));

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
static APPIMAGE_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static APPIMAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
static LINUXD_RESULT: AtomicUsize = AtomicUsize::new(UserTestResult::Pending as usize);
static LINUXD_FAULTS: AtomicUsize = AtomicUsize::new(0);
static MEM_FREE_MIB: AtomicUsize = AtomicUsize::new(0);
static MEM_TOTAL_MIB: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "gate4-portal-test")]
static GATE4_PORTAL_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_PORTAL_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_TEMP_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_RENAMED_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_RENAMES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_DENIED_READS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_DENIED_WRITES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-portal-test")]
static GATE4_REVOKED: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-gui-test")]
static GATE4_GUI_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-gui-test")]
static GATE4_GUI_SAVE_MARKER: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "gate4-gui-test")]
static GATE4_GUI_POST_EXIT: AtomicUsize = AtomicUsize::new(0);

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
    /// The imported AppImage program is malformed or its interpreter is missing.
    LinuxAppImage,
    /// The imported AppImage arguments or environment exceed loader limits.
    LinuxAppArguments,
    /// The embedded signed Linux runtime rootfs could not be imported.
    LinuxRoot,
    /// The user AppImage could not be imported or launched.
    Linuxd,
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
            && APPIMAGE_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
            && LINUXD_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize
        {
            timer::disable_interrupts();
            #[cfg(feature = "gate4-gui-test")]
            if APPIMAGE_RESULT.load(Ordering::Acquire) == UserTestResult::Passed as usize {
                // The GUI's save/revoke evidence is self-contained: it only
                // needs the verified AppImage task to have exited. Emit it
                // before the unrelated static Linux fixture result is
                // checked, so a pre-existing fixture failure cannot hide the
                // kernel-owned portal evidence.
                crate::linuxd::post_app_exit();
            }
            let denied = DENIED.load(Ordering::Acquire);
            let messages = MESSAGES.load(Ordering::Acquire);
            if denied != 2 || messages != 3 {
                serial_line!("AETHER_BOOT_FAILED reason=ipc denials={denied} messages={messages}");
                self_tests_failed()
            }
            let user_result = USER_RESULT.load(Ordering::Acquire);
            if user_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=user-test result={} faults={}",
                    user_result,
                    USER_FAULTS.load(Ordering::Acquire)
                );
                self_tests_failed()
            }
            let linux_result = LINUX_RESULT.load(Ordering::Acquire);
            if linux_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linux-test result={} faults={}",
                    linux_result,
                    LINUX_FAULTS.load(Ordering::Acquire)
                );
                self_tests_failed()
            }
            let dyn_result = LINUX_DYN_RESULT.load(Ordering::Acquire);
            if dyn_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linux-dynamic-test result={} faults={}",
                    dyn_result,
                    LINUX_DYN_FAULTS.load(Ordering::Acquire)
                );
                self_tests_failed()
            }
            let linuxd_result = LINUXD_RESULT.load(Ordering::Acquire);
            if linuxd_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linuxd-service result={} faults={}",
                    linuxd_result,
                    LINUXD_FAULTS.load(Ordering::Acquire)
                );
                self_tests_failed()
            }
            if !crate::linuxd::launch_approved() {
                serial_line!("AETHER_BOOT_FAILED reason=app launched without linuxd approval");
                self_tests_failed()
            }
            let app_result = APPIMAGE_RESULT.load(Ordering::Acquire);
            if app_result != UserTestResult::Passed as usize {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=linux-appimage-test result={} faults={}",
                    app_result,
                    APPIMAGE_FAULTS.load(Ordering::Acquire)
                );
                self_tests_failed()
            }
            // Kernel-owned evidence that a dynamically linked AppImage task
            // exited with status zero. The selected path comes from the signed
            // record; a guest string alone is not accepted as proof of a run.
            if crate::linux::app_is_dynamic(0) {
                let path = crate::linux::app_identity(0).map_or("unknown", |(_, _, path)| path);
                serial_line!("AETHER_GATE3_APP_OK status=0 pid={APPIMAGE_TASK} path={path}");
                // Kernel-owned denial evidence: the imported task carries no
                // portal or native capability, sees the imported tree as
                // read-only, and writes only to its private overlay domain.
                if app_capabilities_empty() {
                    serial_line!(
                        "AETHER_GATE3_DENIAL_OK portal=none native=denied root=readonly overlay=private"
                    );
                }
            }
            serial_line!(
                "AETHER_LINUX_OK abi=linux exits={} faults={} spaces_released={} frames_released={}",
                linux_exits(),
                LINUX_FAULTS.load(Ordering::Acquire),
                RELEASED_SPACES.load(Ordering::Relaxed),
                RELEASED_FRAMES.load(Ordering::Relaxed)
            );
            crate::linuxd::post_app_exit();
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
            // Running evidence that same-address-space switches avoided the
            // CR3 reload: the boot self-test guarantees at least one skip, and
            // Linux threads reusing a process space add more.
            serial_line!(
                "sched: cr3 reloads={} skips={}",
                crate::memory::paging::cr3_reloads(),
                crate::memory::paging::cr3_skips()
            );
            serial_line!("AETHER_CAP_OK denials={denied}");
            serial_line!("AETHER_IPC_OK messages={messages}");
            let escapes = crate::fs::escape_denials();
            if escapes < 2 {
                serial_line!("AETHER_BOOT_FAILED reason=sandbox escapes={escapes}");
                self_tests_failed()
            }
            // The ring-3 IPC demo must have exercised the bound positive and
            // negative paths: sends, receives, and each denial the kernel
            // enforces from user mode: oversize payload (EINVAL), forged
            // handle (EBADF), re-transferred capability (EACCES), endpoint
            // pool exhaustion (ENOSPC), stale handle after close (EBADF),
            // and a file capability used as an endpoint (EBADF). A full or
            // empty queue answers `EAGAIN` and is flow control, not a
            // denial, so those two demo cases are not in this count. A user
            // task that exited by claiming success without producing the
            // evidence fails the boot.
            if crate::syscall::ipc_sends() < 8
                || crate::syscall::ipc_receives() < 8
                || crate::syscall::ipc_denials() < 6
                || crate::syscall::ipc_blocks() < 1
                || crate::syscall::ipc_timeouts() < 1
                || crate::syscall::ipc_closes() < 1
                || crate::fs::quota_denials() < 1
            {
                serial_line!(
                    "AETHER_BOOT_FAILED reason=user-ipc sends={} receives={} denials={} \
                     blocks={} timeouts={} closes={} quota={}",
                    crate::syscall::ipc_sends(),
                    crate::syscall::ipc_receives(),
                    crate::syscall::ipc_denials(),
                    crate::syscall::ipc_blocks(),
                    crate::syscall::ipc_timeouts(),
                    crate::syscall::ipc_closes(),
                    crate::fs::quota_denials()
                );
                self_tests_failed()
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
                "AETHER_IPC_USER_OK sends={} receives={} denials={} blocks={} timeouts={} closes={}",
                crate::syscall::ipc_sends(),
                crate::syscall::ipc_receives(),
                crate::syscall::ipc_denials(),
                crate::syscall::ipc_blocks(),
                crate::syscall::ipc_timeouts(),
                crate::syscall::ipc_closes()
            );
            // Before AETHER_BOOT_OK, so the harness cannot take the boot
            // for a success; the desktop is already on screen.
            #[cfg(feature = "crash-screen-selftest")]
            crash_screen_selftest();
            serial_line!(
                "AETHER_BOOT_OK stage=kernel version={}",
                env!("CARGO_PKG_VERSION")
            );
            // The boot self-tests are complete. Keep the console alive so the
            // shell test and a human can inspect the running system.
            console_loop()
        }
        crate::shell::poll();
        // The display boot test injects a real PS/2 key and pointer packet
        // while the Linux task waits in `poll`. Route them through the normal
        // input/desktop path before parking for the next interrupt.
        crate::input::poll();
        crate::desktop::tick();
        crate::net::poll();
        timer::halt_until_interrupt();
    }
}

/// Faults on an unmapped kernel address once the desktop is on screen and
/// every self-test passed, so the crash report must replace the desktop.
#[cfg(feature = "crash-screen-selftest")]
fn crash_screen_selftest() {
    const PROBE: u64 = 0x0000_7f00_dead_0000;
    serial_line!("boot: crash screen self-test: reading {PROBE:#x}");
    // SAFETY: Deliberately reads an address with no PML4 entry; the page
    // fault is fatal and the handler paints the crash report.
    let _ = unsafe { core::ptr::read_volatile(PROBE as *const u64) };
    serial_line!("AETHER_BOOT_FAILED reason=crash screen self-test did not fault");
}

/// Keeps the console and desktop running after a boot self-test failed.
///
/// The `AETHER_BOOT_FAILED` marker is already on the serial line, so the
/// harnesses still see the failure. Halting with interrupts off here only
/// froze the desktop a person was looking at.
fn self_tests_failed() -> ! {
    serial_line!("boot: self-tests failed; the console and desktop stay interactive");
    console_loop()
}

/// Services the shell, input, and desktop once the boot self-tests are done.
fn console_loop() -> ! {
    timer::enable_interrupts();
    loop {
        crate::shell::poll();
        crate::input::poll();
        crate::desktop::tick();
        crate::net::poll();
        timer::halt_until_interrupt();
    }
}

fn init(allocator: &mut FrameAllocator<'_>) -> Result<(), SchedulerError> {
    let kernel_pml4 = paging::current_pml4().ok_or(SchedulerError::MissingKernelPml4)?;
    let tasks = unsafe { &mut *TASKS.0.get() };
    let stacks = unsafe { &mut *STACKS.0.get() };

    // Task 0 is the idle task: it is the boot context and keeps the kernel
    // address space. Its kernel stack is the stack used during boot.
    let mut idle = Task::new();
    idle.state = TaskState::Running;
    idle.kind = TaskKind::Idle;
    idle.name = fixed_name("kernel");
    idle.pml4 = kernel_pml4.start_address();
    idle.kernel_stack_top = kernel_stack_top(allocator, 0)?;
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
        task.name = fixed_name("kworker");
        task.saved_rsp = saved_rsp;
        task.pml4 = pml4.start_address();
        task.kernel_stack_top = kernel_stack_top(allocator, task_id)?;
        tasks[task_id] = task;
        serial_line!(
            "sched: task {task_id} ready pml4={:#018x} private={:#018x}",
            pml4.start_address(),
            private.start_address()
        );
    }

    // Create two endpoints and grant table-local capabilities. Worker 3 gets a
    // receive-only capability on endpoint A to exercise insufficient rights.
    let endpoint_a = crate::ipc::create_endpoint(1).ok_or(SchedulerError::NoEndpoint)?;
    let endpoint_b = crate::ipc::create_endpoint(2).ok_or(SchedulerError::NoEndpoint)?;
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
    crate::net::consent::init();
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
    let user_kernel_top = kernel_stack_top(allocator, USER_TASK)?;
    let user_saved_rsp =
        prepare_user_stack(user_kernel_top, image.entry(), (USER_STACK_TOP - 16) & !0xf);

    let mut user = Task::new();
    user.state = TaskState::Ready;
    user.kind = TaskKind::User;
    user.name = fixed_name("aether-init");
    user.saved_rsp = user_saved_rsp;
    user.pml4 = user_pml4.start_address();
    user.kernel_stack_top = user_kernel_top;
    user.root = crate::fs::DEMO_ROOT;
    user.tgid = USER_TASK;
    user.bind_file_state(USER_TASK);
    tasks[USER_TASK] = user;
    crate::apps::started(USER_TASK);

    // Verify and import the embedded signed runtime rootfs before the Linux
    // task can resolve any file.
    let (root_files, root_bytes) = crate::linux::init().map_err(|_| SchedulerError::LinuxRoot)?;
    serial_line!("AETHER_LINUX_IMPORT_OK files={root_files} bytes={root_bytes}");

    // Reserve every imported-application task slot. Process and thread
    // creation skip them; a slot stays empty until `linuxd` approves launch.
    for slot in 0..crate::linux::MAX_APPS {
        tasks[APPIMAGE_TASK + slot] = Task::new();
        tasks[APPIMAGE_TASK + slot].reserved = true;
    }
    serial_line!(
        "sched: linux appimage slots reserved base={} count={} state=awaiting-linuxd",
        APPIMAGE_TASK,
        crate::linux::MAX_APPS
    );

    // Load the ring-3 launch manager into its own PML4. It receives exactly
    // two capabilities: receive on the launch-request endpoint and send on the
    // approval endpoint. It cannot create endpoints or open files.
    let linuxd_image = elf::parse(LINUXD_ELF).map_err(SchedulerError::UserImage)?;
    let linuxd_pml4 = paging::create_user_address_space(allocator, &linuxd_image, LINUXD_ELF)?;
    let linuxd_kernel_top = kernel_stack_top(allocator, LINUXD_TASK)?;
    let linuxd_saved_rsp = prepare_user_stack(
        linuxd_kernel_top,
        linuxd_image.entry(),
        (USER_STACK_TOP - 16) & !0xf,
    );
    let mut linuxd_task = Task::new();
    linuxd_task.state = TaskState::Ready;
    linuxd_task.kind = TaskKind::Service;
    linuxd_task.name = fixed_name("linuxd");
    linuxd_task.saved_rsp = linuxd_saved_rsp;
    linuxd_task.pml4 = linuxd_pml4.start_address();
    linuxd_task.kernel_stack_top = linuxd_kernel_top;
    linuxd_task.root = crate::fs::DEMO_ROOT;
    linuxd_task.tgid = LINUXD_TASK;
    linuxd_task.bind_file_state(LINUXD_TASK);
    tasks[LINUXD_TASK] = linuxd_task;
    serial_line!(
        "sched: linuxd service ready pid={} pml4={:#018x} entry={:#018x} cpl=3",
        LINUXD_TASK,
        linuxd_pml4.start_address(),
        linuxd_image.entry()
    );
    crate::linuxd::init(&mut tasks[LINUXD_TASK].caps).map_err(|_| SchedulerError::Linuxd)?;

    // Load the static Linux test program. It is a boot gate for the Linux
    // syscall entry, not yet an application domain with its own root.
    let linux_image = elf::parse(LINUX_ELF).map_err(|_| SchedulerError::LinuxImage)?;
    let linux_pml4 = paging::create_user_address_space(allocator, &linux_image, LINUX_ELF)?;
    let linux_kernel_top = kernel_stack_top(allocator, LINUX_TASK)?;
    let linux_saved_rsp = prepare_user_stack(
        linux_kernel_top,
        linux_image.entry(),
        (USER_STACK_TOP - 16) & !0xf,
    );

    let mut linux = Task::new();
    linux.state = TaskState::Ready;
    linux.kind = TaskKind::Linux;
    linux.name = fixed_name("linux-hello");
    linux.abi = TaskAbi::Linux;
    linux.saved_rsp = linux_saved_rsp;
    linux.pml4 = linux_pml4.start_address();
    linux.kernel_stack_top = linux_kernel_top;
    linux.root = crate::fs::DEMO_ROOT;
    linux.brk = paging::LINUX_ARENA_BASE;
    linux.tgid = LINUX_TASK;
    linux.bind_file_state(LINUX_TASK);
    linux.pgid = LINUX_TASK;
    linux.sid = LINUX_TASK;
    // Kernel-owned boot fixture grant. This is the only static Linux task, and
    // its display test is part of the boot gate.
    linux.display = true;
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
    let dynamic_kernel_top = kernel_stack_top(allocator, LINUX_DYN_TASK)?;
    let dynamic_saved_rsp =
        prepare_user_stack(dynamic_kernel_top, dynamic.entry, dynamic.stack_pointer);
    let mut dynamic_task = Task::new();
    dynamic_task.state = TaskState::Ready;
    dynamic_task.kind = TaskKind::Linux;
    dynamic_task.name = fixed_name("linux-dynamic");
    dynamic_task.abi = TaskAbi::Linux;
    dynamic_task.saved_rsp = dynamic_saved_rsp;
    dynamic_task.pml4 = dynamic.pml4.start_address();
    dynamic_task.kernel_stack_top = dynamic_kernel_top;
    dynamic_task.root = crate::fs::DEMO_ROOT;
    dynamic_task.brk = paging::LINUX_ARENA_BASE;
    dynamic_task.tgid = LINUX_DYN_TASK;
    dynamic_task.bind_file_state(LINUX_DYN_TASK);
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

/// Creates the imported AppImage task after ring-3 `linuxd` approval.
///
/// The signed image is parsed and mapped here, not at boot. Until
/// `linuxd::service_tick` observes the approval message, no application task
/// exists and nothing can be scheduled from the imported payload.
/// Records that the resident launch service completed its approval handshake.
///
/// The service intentionally stays alive for later desktop launches, so boot
/// completion cannot wait for its task to exit.
pub fn mark_linuxd_ready() {
    LINUXD_RESULT.store(UserTestResult::Passed as usize, Ordering::Release);
}

pub fn launch_appimage(slot: usize) -> Result<(), SchedulerError> {
    if slot >= crate::linux::MAX_APPS {
        return Err(SchedulerError::Linuxd);
    }
    let task_index = APPIMAGE_TASK + slot;
    if slot == 0 && APPIMAGE_RESULT.load(Ordering::Acquire) != UserTestResult::Pending as usize {
        return Err(SchedulerError::Linuxd);
    }
    let program = crate::linux::app_program(slot).ok_or(SchedulerError::Linuxd)?;
    // The Linux parser rejects the static fixture's linker layout (its program
    // headers are not covered by a PT_LOAD), so a parse failure falls back to
    // the existing static loader rather than becoming a hard error. A malformed
    // image is still rejected when both parsers fail.
    let linux_image = match aether_linux_elf::parse(program) {
        Ok(image) => Some(image),
        Err(error) => {
            serial_line!("sched: appimage uses the static loader ({error:?})");
            None
        }
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    if tasks[task_index].state != TaskState::Empty || !tasks[task_index].reserved {
        return Err(SchedulerError::Linuxd);
    }

    // A dynamically linked image is entered through the interpreter named by
    // PT_INTERP. The interpreter and its shared libraries come from the
    // verified runtime rootfs; the AppImage supplies only the main program.
    let dynamic = if let Some(image) = linux_image {
        let Some(interpreter_path) = image.interpreter() else {
            serial_line!("sched: appimage has no PT_INTERP");
            return Err(SchedulerError::LinuxImage);
        };
        let interpreter = crate::linux::file_data(interpreter_path).ok_or_else(|| {
            serial_line!("sched: appimage interpreter missing: {interpreter_path}");
            SchedulerError::LinuxAppImage
        })?;
        let interpreter_image = aether_linux_elf::parse(interpreter).map_err(|error| {
            serial_line!("sched: appimage interpreter parse failed: {error:?}");
            SchedulerError::LinuxAppImage
        })?;
        let program_path = crate::linux::app_identity(slot)
            .map(|(_, _, path)| path)
            .ok_or(SchedulerError::LinuxAppImage)?;
        let (configured, count) =
            crate::linux::app_arguments(slot).ok_or(SchedulerError::LinuxAppArguments)?;
        let mut arguments = [""; crate::linux::MAX_APPIMAGE_ARGUMENTS + 1];
        arguments[0] = program_path;
        arguments[1..1 + count].copy_from_slice(&configured[..count]);
        serial_line!(
            "sched: linux appimage argv={} first={} second={}",
            1 + count,
            program_path,
            if count > 0 { configured[0] } else { "" }
        );
        #[cfg(feature = "gate4-portal-test")]
        let environment = [
            "LD_LIBRARY_PATH=/lib/x86_64-linux-gnu:/lib64",
            "HOME=/tmp",
            "TMPDIR=/tmp",
            crate::fs::GATE4_ENVIRONMENT,
        ];
        #[cfg(all(feature = "gate4-gui-test", not(feature = "gate4-portal-test")))]
        let environment = [
            "LD_LIBRARY_PATH=/lib/x86_64-linux-gnu:/lib64",
            "HOME=/tmp",
            "TMPDIR=/tmp",
            "XDG_RUNTIME_DIR=/run/user/1000",
            "WAYLAND_DISPLAY=wayland-0",
            "WAYLAND_SOCKET=3",
            "WGPU_BACKEND=vulkan",
            "VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json",
            "GRIDCRAFT_NO_PREFS=1",
            // Per-request bridge tracing is deliberately off. It writes one
            // serial line per Wayland request/emit/dispatch, and the emulated
            // UART on the single guest CPU makes that dominate interaction.
            // Use a dedicated diagnostic build when the request stream is the
            // subject of the test.
            // Lavapipe runs one rasterizer thread; the prototype schedules a
            // single CPU, and the extra worker adds contention without gain.
            "LP_NUM_THREADS=1",
            "LANG=C.UTF-8",
            "LC_ALL=C.UTF-8",
            "TZ=UTC",
            crate::fs::GATE4_GUI_ENVIRONMENT,
        ];
        #[cfg(all(not(feature = "gate4-portal-test"), not(feature = "gate4-gui-test")))]
        let environment = [
            "LD_LIBRARY_PATH=/lib/x86_64-linux-gnu:/lib64",
            "HOME=/tmp",
            "TMPDIR=/tmp",
        ];
        // The harness may add variables for the staged app; the loader takes
        // at most MAX_APPIMAGE_ENVIRONMENT entries, so reject rather than drop.
        let (extra, extra_count) =
            crate::linux::app_environment(slot).ok_or(SchedulerError::LinuxAppArguments)?;
        let mut combined = [""; crate::linux::MAX_APPIMAGE_ENVIRONMENT];
        let total = environment.len() + extra_count;
        if total > combined.len() {
            serial_line!("sched: appimage environment has {total} entries");
            return Err(SchedulerError::LinuxAppArguments);
        }
        combined[..environment.len()].copy_from_slice(&environment);
        combined[environment.len()..total].copy_from_slice(&extra[..extra_count]);
        let loaded = crate::memory::with_frame_allocator(|allocator| {
            crate::linux_loader::load(
                allocator,
                program,
                interpreter,
                program_path,
                &arguments[..1 + count],
                &combined[..total],
            )
        })
        .ok_or(SchedulerError::OutOfMemory)?
        .map_err(|error| {
            serial_line!("sched: appimage loader failed: {error:?}");
            match error {
                crate::linux_loader::LoadError::Stack => SchedulerError::LinuxAppArguments,
                _ => SchedulerError::LinuxAppImage,
            }
        })?;
        Some((loaded, image, interpreter_image))
    } else {
        None
    };

    let (pml4, entry, stack_pointer) = if let Some((loaded, _, _)) = dynamic {
        (loaded.pml4, loaded.entry, loaded.stack_pointer)
    } else {
        let static_image = elf::parse(program).map_err(|_| SchedulerError::LinuxAppImage)?;
        let pml4 = crate::memory::with_frame_allocator(|allocator| {
            paging::create_user_address_space(allocator, &static_image, program)
        })
        .ok_or(SchedulerError::OutOfMemory)??;
        (pml4, static_image.entry(), (USER_STACK_TOP - 16) & !0xf)
    };

    // This task slot was reserved for the imported payload; no other task owns
    // its stack.
    let kernel_top =
        crate::memory::with_frame_allocator(|allocator| kernel_stack_top(allocator, task_index))
            .ok_or(SchedulerError::OutOfMemory)??;
    let saved_rsp = prepare_user_stack(kernel_top, entry, stack_pointer);
    let mut app_task = Task::new();
    app_task.state = TaskState::Ready;
    app_task.kind = TaskKind::Linux;
    app_task.name = fixed_name("appimage");
    app_task.abi = TaskAbi::Linux;
    app_task.saved_rsp = saved_rsp;
    app_task.pml4 = pml4.start_address();
    app_task.kernel_stack_top = kernel_top;
    app_task.root = crate::fs::DEMO_ROOT;
    app_task.brk = paging::LINUX_ARENA_BASE;
    app_task.app_slot = slot;
    if let Some((_, _, path)) = crate::linux::app_identity(slot) {
        app_task.exe_path = ExePath::new(path);
    }
    app_task.overlay_app = slot as u8;
    app_task.tgid = task_index;
    app_task.bind_file_state(task_index);
    app_task.pgid = task_index;
    app_task.sid = task_index;
    #[cfg(feature = "gate4-gui-test")]
    {
        // wayland-client's connect_to_env() consumes a supplied fd before the
        // bridge is loaded. A read-only /dev/null at fd 3 satisfies the
        // fcntl/UnixStream checks; the bridge ignores the fd and opens the
        // real /dev/aether-display endpoint itself.
        app_task.linux_fds[3] = Some(LinuxFile {
            source: LinuxFileSource::Pseudo(PseudoFile::DevNull),
            offset: 0,
            writable: false,
        });
    }
    // Narrow display grant tied to the verified import identity. The guest
    // cannot choose this: `app_identity` comes from the signed import record.
    // The GridCraft-only production policy stays in normal builds; the Gate 4
    // test build grants the endpoint to any signed AppImage so the same
    // display path can be exercised for other Craft apps.
    #[cfg(feature = "gate4-gui-test")]
    let display_granted = crate::linux::app_identity(slot).is_some();
    #[cfg(not(feature = "gate4-gui-test"))]
    let display_granted =
        crate::linux::app_identity(slot).is_some_and(|(id, _, _)| id == "ai.storyteller.gridcraft");
    app_task.display = display_granted;
    app_task.jit = crate::linux::app_jit(slot);
    app_task.network_client = crate::linux::app_network_client(slot);
    if let Some((_, main_image, interpreter_image)) = dynamic {
        seed_linux_vmas(&mut app_task, &main_image, crate::linux_loader::MAIN_BASE);
        seed_linux_vmas(
            &mut app_task,
            &interpreter_image,
            crate::linux_loader::INTERP_BASE,
        );
        let _ = app_task.vmas.insert_at(
            USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE,
            USER_STACK_PAGES * PAGE_SIZE,
            aether_memory::vma::PROT_READ | aether_memory::vma::PROT_WRITE,
        );
    }
    tasks[task_index] = app_task;
    // The runtime and AppImage imports run with interrupts masked. Recover
    // any PS/2 bytes the user generated during that window before the window
    // starts accepting pointer input.
    crate::input::rearm_mouse();
    serial_line!(
        "sched: linux appimage task ready pid={} pml4={:#018x} entry={:#018x} approved=linuxd dynamic={}",
        task_index,
        pml4.start_address(),
        entry,
        dynamic.is_some()
    );
    Ok(())
}

/// Records the VMAs of a freshly `execve`d dynamic image in the current task.
///
/// `linux_replace_image` clears the tables; without these entries the
/// dynamic linker's RELRO `mprotect` fails with `ENOMEM` (issue #107).
pub fn linux_seed_exec_vmas(
    main_image: &aether_linux_elf::Image<'_>,
    interpreter_image: &aether_linux_elf::Image<'_>,
) {
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: Task-table access is serialized by the single-CPU scheduler and
    // interrupt masking on the system call path.
    let tasks = unsafe { &mut *TASKS.0.get() };
    let Some(task) = tasks.get_mut(current) else {
        return;
    };
    seed_linux_vmas(task, main_image, crate::linux_loader::MAIN_BASE);
    seed_linux_vmas(task, interpreter_image, crate::linux_loader::INTERP_BASE);
    let _ = task.vmas.insert_at(
        USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE,
        USER_STACK_PAGES * PAGE_SIZE,
        aether_memory::vma::PROT_READ | aether_memory::vma::PROT_WRITE,
    );
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

/// Returns the top of `slot`'s kernel stack, allocating it on first use.
fn kernel_stack_top(
    allocator: &mut FrameAllocator<'_>,
    slot: usize,
) -> Result<u64, SchedulerError> {
    // SAFETY: Single-CPU scheduler state; callers run with interrupts masked.
    let stacks = unsafe { &mut *KERNEL_STACKS.0.get() };
    let base = stacks.get_mut(slot).ok_or(SchedulerError::OutOfMemory)?;
    if *base == 0 {
        let frames = allocator
            .allocate_contiguous((KERNEL_STACK_SIZE as u64) / PAGE_SIZE)
            .ok_or(SchedulerError::OutOfMemory)?;
        *base = frames.start_address();
    }
    Ok(*base + KERNEL_STACK_SIZE as u64)
}

/// [`kernel_stack_top`] for a Linux `clone`/`fork`, as a Linux error.
fn slot_kernel_stack_top(slot: usize) -> Result<u64, i64> {
    crate::memory::with_frame_allocator(|allocator| kernel_stack_top(allocator, slot).ok())
        .flatten()
        .ok_or(LINUX_ENOMEM)
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

/// Returns the imported app-domain slot of the current task.
///
/// Native and service tasks return [`crate::linux::MAX_APPS`], which no app
/// domain can occupy.
#[must_use]
pub fn current_app_slot() -> usize {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].app_slot
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

/// Reports whether the imported AppImage task ran without any capability.
///
/// The Gate 3 denial evidence is derived from the task table, not from a guest
/// string: the app task has no portal, device, or native service handle.
#[must_use]
pub fn app_capabilities_empty() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[APPIMAGE_TASK].caps.is_empty()
}

/// Returns the syscall ABI of the running task.
#[must_use]
pub fn current_abi() -> TaskAbi {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].abi
}

/// Returns `true` when the running native task is a restricted service.
///
/// Native services use `int 0x80`, but the dispatcher applies a narrower
/// allowlist than the one for ordinary native applications.
#[must_use]
pub fn current_is_service() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].kind == TaskKind::Service
}

/// Returns `true` when the running task may own the display endpoint.
///
/// This is a narrow kernel policy, not a general device grant. Normal builds
/// allow only a Linux task explicitly marked at creation (the boot display
/// fixture) or the verified GridCraft AppImage identity. The Gate 4 test build
/// allows any signed AppImage so other Craft apps can be exercised through the
/// same endpoint.
#[must_use]
pub fn current_display_granted() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    task.abi == TaskAbi::Linux && task.display
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
        parent_brk,
        parent_fs_base,
        parent_tgid,
        parent_overlay_app,
        parent_handles,
    ) = {
        let parent = &tasks[current];
        (
            parent.abi,
            parent.pml4,
            parent.root,
            parent.brk,
            parent.fs_base,
            parent.tgid,
            parent.overlay_app,
            parent.handles,
        )
    };
    let Some(slot) = tasks
        .iter()
        .position(|task| task.state == TaskState::Empty && !task.reserved)
    else {
        return Err(LINUX_EAGAIN);
    };
    let kernel_top = slot_kernel_stack_top(slot)?;

    let mut thread = Task::new();
    thread.state = TaskState::Ready;
    thread.kind = TaskKind::Thread;
    thread.name = tasks[current].name;
    thread.abi = parent_abi;
    thread.saved_rsp = 0;
    thread.pml4 = parent_pml4;
    thread.kernel_stack_top = kernel_top;
    thread.root = parent_root;
    thread.brk = parent_brk;
    thread.fs_base = if flags & CLONE_SETTLS != 0 {
        tls
    } else {
        parent_fs_base
    };
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
    // A thread shares the descriptor table of its process, exactly like Linux
    // `CLONE_FILES`, so the views keep naming the parent's thread group.
    thread.tgid = parent_tgid;
    thread.bind_file_state(parent_tgid);
    thread.app_slot = tasks[current].app_slot;
    thread.exe_path = tasks[current].exe_path;
    thread.overlay_app = parent_overlay_app;
    thread.handles = parent_handles;
    thread.jit = tasks[current].jit;
    thread.network_client = tasks[current].network_client;
    thread.seccomp = tasks[current].seccomp.clone();
    thread.seccomp_strict = tasks[current].seccomp_strict;
    thread.no_new_privs = tasks[current].no_new_privs;
    thread.dumpable = tasks[current].dumpable;
    // A thread belongs to the same application domain, so it may use the
    // display endpoint when the process was granted it. Some Craft apps create
    // their Wayland connection on a worker thread; without this the shim fell
    // back to a non-compositor fd and winit reported NoCompositor.
    thread.display = tasks[current].display;

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
    linux_fork_with(frame, ForkSpec::default())
}

/// How a process-creating `clone` differs from a plain `fork`.
#[derive(Clone, Copy, Default)]
pub struct ForkSpec {
    /// New user stack pointer for the child; 0 keeps the parent's.
    pub stack: u64,
    /// `CLONE_SETTLS`: the child's `fs` base.
    pub tls: Option<u64>,
    /// `CLONE_CHILD_CLEARTID`: cleared and futex-woken when the child exits.
    pub clear_child_tid: u64,
}

/// Creates a child process with a private copy of the caller's address
/// space and descriptor table, and returns its id.
pub fn linux_fork_with(frame: *mut InterruptFrame, spec: ForkSpec) -> Result<usize, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let domain = tasks[current].overlay_app;
    let inherited_shared = tasks[current].shared_vmas.len();
    if inherited_shared != 0
        && shared_vma_usage(domain).saturating_add(inherited_shared) > MAX_DOMAIN_SHARED_VMAS
    {
        return Err(LINUX_ENOMEM);
    }
    let parent_pml4 = tasks[current].pml4;
    let Some(slot) = tasks
        .iter()
        .position(|task| task.state == TaskState::Empty && !task.reserved)
    else {
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

    let (abi, root, brk, fs_base, overlay_app, parent_tgid, sigactions, sigblocked, sigaltstack) = {
        let parent = &tasks[current];
        (
            parent.abi,
            parent.root,
            parent.brk,
            parent.fs_base,
            parent.overlay_app,
            parent.tgid,
            parent.sigactions,
            parent.sigblocked,
            parent.sigaltstack,
        )
    };
    let kernel_top = slot_kernel_stack_top(slot)?;

    let mut child = Task::new();
    child.state = TaskState::Ready;
    child.kind = TaskKind::Process;
    child.name = tasks[current].name;
    child.abi = abi;
    child.pml4 = child_pml4.start_address();
    child.kernel_stack_top = kernel_top;
    child.root = root;
    child.brk = brk;
    child.fs_base = fs_base;
    child.tgid = slot;
    child.pgid = tasks[current].pgid;
    child.sid = tasks[current].sid;
    child.parent = current;
    child.app_slot = tasks[current].app_slot;
    child.exe_path = tasks[current].exe_path;
    child.overlay_app = overlay_app;
    child.jit = tasks[current].jit;
    child.network_client = tasks[current].network_client;
    child.seccomp = tasks[current].seccomp.clone();
    child.seccomp_strict = tasks[current].seccomp_strict;
    child.no_new_privs = tasks[current].no_new_privs;
    child.dumpable = tasks[current].dumpable;
    // The fork belongs to the same signed application domain and inherits its
    // display grant. A child that opens the endpoint first owns it; a second
    // opener still gets EBUSY from the display service.
    child.display = tasks[current].display;
    // fork gets a process-private snapshot of the descriptor table; threads
    // continue to address the parent's table via their shared tgid.
    child.bind_file_state(slot);
    if parent_tgid < MAX_TASKS {
        let state = unsafe { &mut *FILE_STATE.0.get() };
        state[slot].fds = state[parent_tgid].fds.clone();
        state[slot].eventfds = state[parent_tgid].eventfds.clone();
        state[slot].epolls = state[parent_tgid].epolls.clone();
        state[slot].timerfds = state[parent_tgid].timerfds.clone();
        retain_shared_fds(&state[slot].fds);
        // fork copies the parent's mappings into the child's own tables.
        let spaces = unsafe { &mut *ADDRESS_SPACES.0.get() };
        let (parent_space, child_space) = if parent_tgid < slot {
            let (low, high) = spaces.split_at_mut(slot);
            (&low[parent_tgid], &mut high[0])
        } else {
            let (low, high) = spaces.split_at_mut(parent_tgid);
            (&high[0], &mut low[slot])
        };
        child_space.clone_from_process(parent_space);
        let counters = unsafe { &mut *EVENTFD_COUNTERS.0.get() };
        counters[slot] = counters[parent_tgid];
        // Inotify is shared by threads but snapshotted by fork.
        let inotify = unsafe { &mut *INOTIFY.0.get() };
        inotify[slot] = inotify[parent_tgid];
    }
    child.sigactions = sigactions;
    child.sigblocked = sigblocked;
    child.sigaltstack = sigaltstack;
    child.sigaltstack_active = false;
    child.robust_list = 0;
    child.robust_list_len = 0;
    child.rseq = 0;
    child.rseq_len = 0;
    child.clear_child_tid = spec.clear_child_tid;
    if let Some(tls) = spec.tls {
        child.fs_base = tls;
    }

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
        if spec.stack != 0 {
            (*(frame_start as *mut InterruptFrame)).rsp = spec.stack;
        }
    }
    child.saved_rsp = frame_start;
    tasks[slot] = child;
    Ok(slot)
}

/// Writes a `u32` into task `slot`'s own memory, through its page tables
/// rather than the active ones (`CLONE_CHILD_SETTID` for a new process).
pub fn linux_write_task_u32(slot: usize, pointer: u64, value: u32) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks
        .get(slot)
        .and_then(|task| PhysFrame::from_address(task.pml4))
        .is_some_and(|pml4| paging::write_user_bytes(pml4, pointer, &value.to_ne_bytes()))
}

/// Suspends the caller until child `tgid` execs or exits, as `vfork`
/// requires. The call returns the child's id when the parent resumes.
pub fn linux_vfork_wait(frame: *mut InterruptFrame, tgid: usize) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: The syscall entry passes a live frame for this task.
    unsafe { (*frame).rax = tgid as u64 };
    let child = &tasks[tgid];
    if child.state == TaskState::Empty || child.state == TaskState::Done {
        return frame;
    }
    tasks[current].wait_kind = LinuxWaitKind::Vfork(tgid);
    tasks[current].wait_deadline = 0;
    tasks[current].state = TaskState::Blocked;
    yield_current(frame)
}

/// Bounded copy of a Linux executable path for `/proc/self/exe`.
#[derive(Clone, Copy)]
struct ExePath {
    bytes: [u8; EXE_PATH_BYTES],
    length: u16,
}

/// Longest executable path kept; `execve` already rejects longer paths.
const EXE_PATH_BYTES: usize = 256;

impl ExePath {
    const EMPTY: Self = Self {
        bytes: [0; EXE_PATH_BYTES],
        length: 0,
    };

    /// Keeps `path`, or nothing when it does not fit.
    fn new(path: &str) -> Self {
        let mut value = Self::EMPTY;
        if let Some(target) = value.bytes.get_mut(..path.len()) {
            target.copy_from_slice(path.as_bytes());
            value.length = path.len() as u16;
        }
        value
    }
}

/// Records the image path after a successful `execve` of the current process.
pub fn linux_set_exe_path(path: &str) {
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: Task-table access is serialized by the single-CPU scheduler and
    // interrupt masking on the system call path.
    let tasks = unsafe { &mut *TASKS.0.get() };
    if let Some(task) = tasks.get_mut(current) {
        task.exe_path = ExePath::new(path);
    }
}

/// Copies the current executable path into `buffer` and returns its length,
/// or `None` when the task has no recorded path (the boot fixtures).
pub fn linux_exe_path(buffer: &mut [u8; 256]) -> Option<usize> {
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: Task-table access is serialized by the single-CPU scheduler and
    // interrupt masking on the system call path.
    let tasks = unsafe { &*TASKS.0.get() };
    let path = &tasks.get(current)?.exe_path;
    let length = usize::from(path.length);
    if length == 0 {
        return None;
    }
    buffer[..length].copy_from_slice(&path.bytes[..length]);
    Some(length)
}

/// Replaces the current task's address space after `execve`.
pub fn linux_replace_image(pml4: PhysFrame) {
    let current = CURRENT.load(Ordering::Relaxed);
    // execve replaces the whole process: other threads of the group would
    // otherwise keep running in the address space released below.
    stop_sibling_threads(current);
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[current];
    let old_pml4 = PhysFrame::from_address(task.pml4);
    let old_address = task.pml4;
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
    task.shared_vmas.reset();
    task.brk = paging::LINUX_ARENA_BASE;
    task.fs_base = 0;
    let tgid = task.tgid;
    if old_address != pml4.start_address() {
        retire_address_space(old_address);
    }
    linux_vfork_done(tgid);
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
    #[cfg(feature = "gate4-gui-test")]
    trace_block("wait4");
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

/// Records the main thread's `clear_child_tid` pointer and returns its TID.
///
/// glibc calls `set_tid_address` to initialise the TCB `tid` field for the
/// initial thread.  Returning `ENOSYS` leaves that field zero, which makes
/// glibc's rwlock recursion checks treat a zero-initialised lock as if the
/// caller already owned it (`EDEADLK`).
pub fn linux_set_tid_address(pointer: u64) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].clear_child_tid = pointer;
    current as i64
}

/// Returns the wait-queue key `(space, address)` for a futex word in the
/// current address space, following Linux's `get_futex_key`.
///
/// A word in a `MAP_SHARED` frame is keyed by its physical address (space
/// zero): that frame is the same in every process that maps it, so shared
/// futexes meet across `fork`. Any other word is private to the process and
/// is keyed by its virtual address in this address space; threads share the
/// page table, and so the key. A physical key is not stable for private
/// memory: `fork` makes the page copy-on-write and the next write moves it
/// to a new frame, which stranded waiters that blocked before the fork
/// (issue #146). The private key also keeps one application from reaching
/// another's waiters.
fn futex_key(address: u64) -> Option<(u64, u64)> {
    let tasks = unsafe { &*TASKS.0.get() };
    let space = tasks[CURRENT.load(Ordering::Relaxed)].pml4;
    let pml4 = PhysFrame::from_address(space)?;
    let shared = paging::user_physical_address(pml4, address).filter(|physical| {
        PhysFrame::from_address(physical & !0xfff).is_some_and(crate::memory::shared::is_registered)
    });
    Some(match shared {
        Some(physical) => (0, physical),
        None => (space, address),
    })
}

/// Blocks the current Linux task on a futex word.
pub fn linux_futex_wait(
    frame: *mut InterruptFrame,
    address: u64,
    timeout_ticks: Option<u64>,
) -> *mut InterruptFrame {
    let Some((space, key)) = futex_key(address) else {
        // SAFETY: `frame` is the live Linux syscall frame on this task's stack.
        unsafe { (*frame).rax = LINUX_EFAULT as u64 };
        return frame;
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].futex_space = space;
    tasks[current].futex_address = key;
    tasks[current].futex_deadline = timeout_ticks
        .and_then(|ticks| TICKS.load(Ordering::Relaxed).checked_add(ticks))
        .unwrap_or(0);
    #[cfg(feature = "gate4-gui-test")]
    trace_block("futex");
    // A successful FUTEX_WAIT returns zero when a waker releases the task. The
    // syscall entry leaves the number in RAX, so set the result before
    // blocking; the timeout path overwrites it with ETIMEDOUT.
    // SAFETY: `frame` is the live Linux syscall frame on this task's stack.
    unsafe { (*frame).rax = 0 };
    tasks[current].state = TaskState::Blocked;
    yield_current(frame)
}

/// Wakes up to `count` tasks blocked on `address`.
pub fn linux_futex_wake(address: u64, count: u64) -> i64 {
    let Some((space, address)) = futex_key(address) else {
        return 0;
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let mut woken = 0u64;
    for task in tasks.iter_mut() {
        if woken >= count {
            break;
        }
        if task.state == TaskState::Blocked
            && task.futex_address == address
            && task.futex_space == space
        {
            task.state = TaskState::Ready;
            task.futex_address = 0;
            task.futex_deadline = 0;
            woken += 1;
        }
    }
    woken as i64
}

/// Debug-only dump of every Linux task's wait state (issue #146).
///
/// For a blocked task the saved frame is the syscall frame, so `rip` is the
/// user return address and `rax`/`rdi`/`rsi`/`rdx`/`r10` the call's result
/// slot and arguments. For a futex waiter the line also prints the word's
/// current value and the frame its virtual address resolves to now, so a
/// waiter keyed by a stale frame is visible.
#[cfg(feature = "linux-wait-dump")]
fn linux_wait_dump(now: u64) {
    let tasks = unsafe { &*TASKS.0.get() };
    let running = CURRENT.load(Ordering::Relaxed);
    for (index, task) in tasks.iter().enumerate() {
        if task.abi != TaskAbi::Linux || task.state == TaskState::Empty || index == running {
            continue;
        }
        // SAFETY: a task that is not running saved its interrupt frame at
        // `saved_rsp` on its own kernel stack, which stays allocated.
        let frame = unsafe { &*(task.saved_rsp as *const InterruptFrame) };
        let resolved = PhysFrame::from_address(task.pml4)
            .and_then(|pml4| paging::user_physical_address(pml4, frame.rdi))
            .unwrap_or(0);
        let word = if task.futex_address != 0 && resolved != 0 && frame.rdi.is_multiple_of(4) {
            // SAFETY: `resolved` is the identity-mapped physical address of
            // a present, 4-byte aligned word of this task's user mapping.
            unsafe { core::ptr::read_volatile(resolved as *const u32) }
        } else {
            0
        };
        serial_line!(
            "WAITDUMP t={} task={} tgid={} state={:?} wait={:?} last={} rip={:#x} rax={:#x} rdi={:#x} rsi={:#x} rdx={:#x} r10={:#x} futex_space={:#x} futex_key={:#x} now_phys={:#x} word={:#x} deadline={} wait_deadline={} cpu={}",
            now / 100,
            index,
            task.tgid,
            task.state,
            task.wait_kind,
            task.last_syscall,
            frame.rip,
            frame.rax,
            frame.rdi,
            frame.rsi,
            frame.rdx,
            frame.r10,
            task.futex_space,
            task.futex_address,
            resolved,
            word,
            task.futex_deadline,
            task.wait_deadline,
            task.cpu_ticks
        );
    }
}

/// Implements the wake-then-requeue half of `FUTEX_CMP_REQUEUE`.
///
/// Up to `wake` waiters on `address` become ready; every further waiter on the
/// same address (up to `requeue` more) keeps blocking but switches its futex
/// word to `target`, so a later `FUTEX_WAKE(target)` reaches it. This mirrors
/// the POSIX condition-variable hand-off glibc performs on broadcast.
pub fn linux_futex_requeue(address: u64, target: u64, wake: u64, requeue: u64) -> i64 {
    let (Some((space, address)), Some((target_space, target))) =
        (futex_key(address), futex_key(target))
    else {
        return LINUX_EFAULT;
    };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let mut woken = 0u64;
    let mut moved = 0u64;
    for task in tasks.iter_mut() {
        if woken >= wake && moved >= requeue {
            break;
        }
        if task.state != TaskState::Blocked
            || task.futex_address != address
            || task.futex_space != space
        {
            continue;
        }
        if woken < wake {
            task.state = TaskState::Ready;
            task.futex_address = 0;
            task.futex_deadline = 0;
            woken += 1;
        } else {
            task.futex_space = target_space;
            task.futex_address = target;
            task.futex_deadline = 0;
            moved += 1;
        }
    }
    (woken + moved) as i64
}

#[cfg(not(feature = "gate4-gui-test"))]
#[allow(unused_variables)]
pub fn futex_trace(command: u64, address: u64, value: u32, arg4: u64, target: u64, result: i64) {}

/// Bounded Gate 4 diagnostic: one line per distinct futex command and result.
///
/// The GUI application exercises futex intensively, so the trace aggregates
/// repeats instead of printing a line per call. Each (command, result) pair
/// logs once with its total worry count; the table is fixed size so the trace
/// stays bounded no matter how busy the render loop becomes.
#[cfg(feature = "gate4-gui-test")]
pub fn futex_trace(command: u64, address: u64, value: u32, arg4: u64, target: u64, result: i64) {
    // Bounded counters, one slot per supported command plus an overflow slot.
    static SEEN: [AtomicU64; 16] = {
        #[allow(clippy::declare_interior_mutable_const)]
        const NEW: AtomicU64 = AtomicU64::new(0);
        [NEW; 16]
    };
    let slot = (command as usize).min(SEEN.len() - 1);
    let count = SEEN[slot].fetch_add(1, Ordering::Relaxed) + 1;
    // The first sighting of a command and every 4096-th repeat produce one
    // line, which keeps even a pathological caller at a handful of lines.
    if count == 1 || count % 4096 == 0 {
        let task = current_task();
        serial_line!(
            "linux: futex cmd={} addr={:#x} val={} arg4={:#x} target={:#x} result={} seen={} task={}",
            command,
            address,
            value,
            arg4,
            target,
            result,
            count,
            task
        );
    }
}

/// Parks the current task and rewinds the saved user context to the
/// `syscall` instruction. When the task is woken, the syscall is re-executed
/// and the handler resumes with its stored absolute deadline. This is the
/// kernel's blocking model: a syscall never continues on the kernel stack
/// after handing control to another task.
/// Records the most recent Linux system call number for the current task.
///
/// Only the Gate 4 test build uses this: it makes a blocked GUI thread
/// identifiable from the serial log without a debugger. The trace prints one
/// line whenever the call number changes, so a burst of identical calls
/// collapses into a single line.
#[cfg(any(feature = "gate4-gui-test", feature = "linux-wait-dump"))]
pub fn linux_set_last_syscall(number: u64) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if current >= tasks.len() {
        return;
    }
    let task = &mut tasks[current];
    task.last_syscall = number;
}

/// Traces a system call the Gate 4 GUI application needs and the prototype
/// does not implement.
///
/// The trace is bounded: the emulated serial port cannot carry an unbounded
/// number of lines.
#[cfg(any(feature = "gate4-gui-test", feature = "linux-wait-dump"))]
pub fn linux_trace_enosys(number: u64) {
    const TRACE_LIMIT: usize = 32;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    let task = current_task();
    serial_line!("linux: enosys nr={number} task={task}");
}

/// Traces a system call of the Gate 4 GUI application that failed.
///
/// The trace is bounded: the emulated serial port cannot carry an unbounded
/// number of lines.
#[cfg(feature = "gate4-gui-test")]
pub fn linux_trace_error(number: u64, result: i64) {
    const TRACE_LIMIT: usize = 64;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    let task = current_task();
    serial_line!("linux: err nr={number} task={task} result={result}");
}

/// Traces one system call of the Gate 4 GUI application.///
/// The trace is bounded: the emulated serial port cannot carry an unbounded
/// number of lines, and the application may repeat a call in a loop. Calls
/// that only move memory around are skipped because they are too noisy to
/// show where the application stops.
#[cfg(feature = "gate4-gui-test")]
pub fn linux_trace_syscall(number: u64, a0: u64, a1: u64) {
    const TRACE_LIMIT: usize = 1024;
    const QUIET: [u64; 5] = [9, 11, 25, 26, 12];
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if QUIET.contains(&number) {
        return;
    }
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    let task = current_task();
    serial_line!("linux: sc nr={number} task={task} a0={a0:#x} a1={a1:#x}");
}

/// Traces the result of one system call of the Gate 4 GUI application.
///
/// The trace is bounded like [`linux_trace_syscall`].
#[cfg(feature = "gate4-gui-test")]
pub fn linux_trace_syscall_result(number: u64, result: i64) {
    const TRACE_LIMIT: usize = 1024;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    let task = current_task();
    serial_line!("linux: sc result nr={number} task={task} result={result}");
}

/// Traces one descriptor call of the Gate 4 GUI application.
///
/// The trace is bounded: the emulated serial port cannot carry an unbounded
/// number of lines, and the application may repeat a call in a loop.
#[cfg(feature = "gate4-gui-test")]
pub fn linux_trace_fdcall(number: u64, fd: u64, result: i64) {
    const TRACE_LIMIT: usize = 32;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    serial_line!("linux: fdcall nr={} fd={} result={}", number, fd, result);
}

/// Returns `true` when the current task belongs to the Gate 4 GUI program.
#[cfg(any(feature = "gate4-gui-test", feature = "linux-wait-dump"))]
pub fn linux_is_appimage_task() -> bool {
    current_app_slot() < crate::linux::MAX_APPS
}

/// Traces a task that is about to block, with the system call it is in.
///
/// This is a bounded diagnostic for the Gate 4 GUI test only. It prints one
/// line per block, which is enough to identify the wait the application is
/// stuck in without flooding the serial log.
#[cfg(feature = "gate4-gui-test")]
fn trace_block(kind: &str) {
    /// Upper bound on the number of trace lines, so the serial log stays
    /// readable and the emulated serial port does not saturate.
    const TRACE_LIMIT: usize = 32;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    if current >= tasks.len() || tasks[current].app_slot >= crate::linux::MAX_APPS {
        return;
    }
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    let task = &tasks[current];
    serial_line!(
        "linux: block task={} tgid={} kind={} syscall={}",
        current,
        task.tgid,
        kind,
        task.last_syscall
    );
}

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
    #[cfg(feature = "gate4-gui-test")]
    trace_block("park");
    tasks[current].state = TaskState::Blocked;
    // SAFETY: `frame` is the live Linux syscall frame on the current task's
    // kernel stack; the Linux entry stub is always reached through `syscall`
    // (0f 05), so rewinding two bytes re-executes the instruction on resume.
    unsafe { (*frame).rip = (*frame).rip.wrapping_sub(2) };
    tasks[current].restart_rip = unsafe { (*frame).rip };
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

/// Releases a `vfork` parent once its child (`tgid`) has exec'd or exited.
fn linux_vfork_done(tgid: usize) {
    linux_wake_matching(|kind| kind == LinuxWaitKind::Vfork(tgid));
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

/// Returns `true` if the current task may create writable+executable
/// mappings (its signed AppImage record declares `memory=jit`).
#[must_use]
/// Returns `true` if the current Linux task belongs to an application whose
/// signed record declares `network=client`. Consent is checked separately.
pub fn linux_network_declared() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].network_client
}

pub fn linux_jit_allowed() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].jit
}

/// Returns the registered robust-list head and length for `pid` (0 = caller).
pub fn linux_get_robust_list(pid: usize) -> Result<(u64, u64), i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let session = tasks[current].sid;
    let domain = tasks[current].overlay_app;
    let target = if pid == 0 {
        Some(current)
    } else {
        tasks.iter().position(|task| {
            task.state != TaskState::Empty
                && task.state != TaskState::Done
                && task.abi == TaskAbi::Linux
                && task.sid == session
                && task.overlay_app == domain
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
    // Session ids are task slots and can be reused, so the application
    // domain is checked too: a recycled session never reaches another app.
    let domain = tasks[current].overlay_app;
    let sender = tasks[current].tgid as u32;
    let targets: [bool; MAX_TASKS] = core::array::from_fn(|index| {
        let task = &tasks[index];
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.sid == session
            && task.overlay_app == domain
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
                ..SigInfo::default()
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
    // A `vfork` parent waits like Linux's killable wait: signals stay
    // pending until the child execs or exits, and the parent's result is
    // already the child's id.
    if matches!(task.wait_kind, LinuxWaitKind::Vfork(_)) {
        return;
    }
    // A blocked inotify read has no rewound syscall instruction; clear its
    // waiter record so a later readiness event cannot overwrite the result.
    task.inotify_wait_pointer = 0;
    task.inotify_wait_length = 0;
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
    info[4..8].copy_from_slice(&queued.errno.to_ne_bytes());
    info[8..12].copy_from_slice(&queued.code.to_ne_bytes());
    if signum == SIGSYS && queued.code == SYS_SECCOMP {
        // `_sigsys`: call address, system call number, audit architecture.
        info[16..24].copy_from_slice(&queued.address.to_ne_bytes());
        info[24..28].copy_from_slice(&queued.syscall.to_ne_bytes());
        info[28..32].copy_from_slice(&aether_seccomp::AUDIT_ARCH_X86_64.to_ne_bytes());
        return;
    }
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
                // SysV enters the handler as if by `call`: the restorer word
                // at [rsp] is the return address, so rsp must be 8 mod 16.
                let candidate = (top - frame_size) & !15;
                let Some(candidate) = candidate.checked_sub(8) else {
                    return false;
                };
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
            let Some(candidate) = candidate.checked_sub(8) else {
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

/// RFLAGS bits a signal frame may change: CF, PF, AF, ZF, SF, TF, DF, OF, AC, RF.
const SIGRETURN_USER_RFLAGS: u64 = 0x0004_0dd5 | (1 << 16);
/// RFLAGS every restored user context starts from: IF plus reserved bit 1.
const SIGRETURN_BASE_RFLAGS: u64 = 0x202;

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
    // Copy the frame out through the page tables rather than dereferencing
    // the user address.
    let mut raw = [0u8; core::mem::size_of::<RtSigFrame>()];
    if !paging::read_user_bytes(pml4, frame_pointer, &mut raw) {
        return false;
    }
    // SAFETY: `raw` holds a complete `RtSigFrame`, a `repr(C)` record of
    // integers for which every bit pattern is valid.
    let saved: RtSigFrame = unsafe { ptr::read_unaligned(raw.as_ptr().cast()) };
    let context = saved.context;
    if context.mcontext.cs != USER_CODE_SELECTOR
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
        // Only the arithmetic, direction, trap, alignment, and resume flags
        // are user-controlled, as with Linux `FIX_EFLAGS`. IOPL, NT, and the
        // rest stay at their kernel-chosen values, and IF is always set: an
        // unmasked restore would let a guest gain port I/O or make `iretq`
        // fault in ring 0.
        (*frame).rflags = SIGRETURN_BASE_RFLAGS | (context.mcontext.eflags & SIGRETURN_USER_RFLAGS);
        (*frame).cs = u64::from(USER_CODE_SELECTOR);
        (*frame).ss = u64::from(USER_DATA_SELECTOR);
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
            ..SigInfo::default()
        },
    );
    deliver_signal_to(current, frame)
}

/// Runs `f` on the current task's seccomp state: its newest filter and
/// whether strict mode is on.
pub fn linux_with_seccomp<R>(f: impl FnOnce(Option<&crate::seccomp::Filter>, bool) -> R) -> R {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    f(task.seccomp.as_deref(), task.seccomp_strict)
}

/// The current task's newest seccomp filter, to stack a new one on.
#[must_use]
pub fn linux_seccomp_filter() -> Option<Rc<crate::seccomp::Filter>> {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].seccomp.clone()
}

/// Enters `SECCOMP_MODE_STRICT`. Fails with `EINVAL` once a filter is set.
pub fn linux_seccomp_set_strict() -> Result<(), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.seccomp.is_some() {
        return Err(LINUX_EINVAL);
    }
    task.seccomp_strict = true;
    Ok(())
}

/// Installs `filter` as the current task's newest seccomp filter.
///
/// With `sync` (`SECCOMP_FILTER_FLAG_TSYNC`) every other thread of the
/// process gets it too, which is allowed only when each thread's filter is
/// one the caller's chain already contains. Otherwise nothing changes and
/// the error is the first such thread's id, or `ESRCH` with `esrch`.
pub fn linux_seccomp_install(
    filter: Rc<crate::seccomp::Filter>,
    sync: bool,
    esrch: bool,
) -> Result<(), i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let tgid = tasks[current].tgid;
    let live = |task: &Task| {
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.tgid == tgid
    };
    if sync {
        for (slot, task) in tasks.iter().enumerate() {
            if slot == current || !live(task) {
                continue;
            }
            let ancestor = match &task.seccomp {
                None => !task.seccomp_strict,
                Some(theirs) => filter.descends_from(theirs),
            };
            if !ancestor {
                return Err(if esrch { LINUX_ESRCH } else { slot as i64 });
            }
        }
        let no_new_privs = tasks[current].no_new_privs;
        for (slot, task) in tasks.iter_mut().enumerate() {
            if slot != current && live(task) {
                task.seccomp = Some(Rc::clone(&filter));
                task.no_new_privs |= no_new_privs;
            }
        }
    }
    tasks[current].seccomp = Some(filter);
    Ok(())
}

/// `PR_GET_NO_NEW_PRIVS`.
#[must_use]
pub fn linux_no_new_privs() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].no_new_privs
}

/// `PR_SET_NO_NEW_PRIVS`; the flag is never cleared.
pub fn linux_set_no_new_privs() {
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].no_new_privs = true;
}

/// `PR_GET_DUMPABLE`.
#[must_use]
pub fn linux_dumpable() -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].dumpable
}

/// `PR_SET_DUMPABLE`.
pub fn linux_set_dumpable(dumpable: bool) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].dumpable = dumpable;
}

/// `PR_GET_NAME`: the task's name, NUL padded to Linux `TASK_COMM_LEN`.
#[must_use]
pub fn linux_task_name() -> [u8; 16] {
    let tasks = unsafe { &*TASKS.0.get() };
    let name = &tasks[CURRENT.load(Ordering::Relaxed)].name;
    let mut out = [0u8; 16];
    let len = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len())
        .min(15);
    out[..len].copy_from_slice(&name[..len]);
    out
}

/// `PR_SET_NAME`: renames the calling thread (at most 15 bytes).
pub fn linux_set_task_name(name: &[u8]) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let len = name.len().min(15);
    task.name = [0; PROCESS_NAME_BYTES];
    task.name[..len].copy_from_slice(&name[..len]);
}

/// Ends the calling process for a seccomp kill with `signum`. The system
/// call never runs.
pub fn linux_seccomp_kill(frame: *mut InterruptFrame, signum: usize) -> *mut InterruptFrame {
    {
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].signal_exit = signum as u8;
    }
    linux_signal_exit(frame)
}

/// `SECCOMP_RET_TRAP`: skips the system call and raises `SIGSYS` with the
/// call's number and `errno` as `si_errno`. Like Linux `force_sig`, a
/// blocked or ignored `SIGSYS` is unblocked and reset to the default, so the
/// violation either reaches a handler or ends the process.
pub fn linux_seccomp_trap(
    frame: *mut InterruptFrame,
    number: u64,
    errno: u16,
) -> *mut InterruptFrame {
    let current = CURRENT.load(Ordering::Relaxed);
    {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[current];
        let bit = 1u64 << (SIGSYS - 1);
        task.sigblocked &= !bit;
        if task.sigactions[SIGSYS - 1].is_some_and(|action| action.handler == 1) {
            task.sigactions[SIGSYS - 1] = None;
        }
    }
    // SAFETY: The syscall entry passes a live frame for this task.
    let rip = unsafe { (*frame).rip };
    queue_signal(
        current,
        SIGSYS,
        SigInfo {
            code: SYS_SECCOMP,
            address: rip,
            errno: i32::from(errno),
            syscall: number as i32,
            ..SigInfo::default()
        },
    );
    // Linux rolls the registers back, so the handler sees the call number
    // in `rax` and decides the result itself.
    unsafe { (*frame).rax = number };
    linux_deliver_signal(frame);
    if linux_signal_pending_exit() {
        return linux_signal_exit(frame);
    }
    frame
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
        release_linux_task_resources(&mut task.linux_fds);
    }
    // The whole process is gone, so its shared descriptor table goes with it.
    clear_file_state(tgid);
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
    linux_vfork_done(child);
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
            ..SigInfo::default()
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
    if tasks[index].sid == target
        || tasks[index].sid != tasks[current].sid
        || tasks[index].overlay_app != tasks[current].overlay_app
    {
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
                && task.overlay_app == tasks[current].overlay_app
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
    // Session ids are task slots and can be reused, so the application
    // domain is checked too: a recycled session never reaches another app.
    let domain = tasks[current].overlay_app;
    let sender = tasks[current].tgid as u32;
    let targets: [bool; MAX_TASKS] = core::array::from_fn(|index| {
        let task = &tasks[index];
        task.state != TaskState::Empty
            && task.state != TaskState::Done
            && task.abi == TaskAbi::Linux
            && task.sid == session
            && task.overlay_app == domain
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
                ..SigInfo::default()
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
    // Write through the exiting task's own page tables: it need not be the
    // address space that is active now.
    if !paging::write_user_bytes(pml4, pointer, &0u32.to_ne_bytes()) {
        return;
    }
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
    if tasks[current].kind == TaskKind::Terminal {
        retire_terminal_task(current, (status & 0xff) as i64, false);
        return schedule_next(current);
    }
    if current == LINUXD_TASK && tasks[current].kind == TaskKind::Service {
        tasks[current].state = TaskState::Done;
        let closed = crate::fs::close_task_files(&mut tasks[current].caps);
        let result = if status == 0 {
            UserTestResult::Passed
        } else {
            UserTestResult::Failed
        };
        LINUXD_RESULT.store(result as usize, Ordering::Release);
        serial_line!("linuxd: service exit pid={current} status={status} closed={closed}");
        return schedule_next(current);
    }
    if current != USER_TASK || tasks[current].kind != TaskKind::User {
        serial_line!("AETHER_BOOT_FAILED reason=exit from non-user task");
        halt()
    }

    tasks[current].state = TaskState::Done;
    USER_EXITS.fetch_add(1, Ordering::Release);
    // Release the task's open-file slots so a completed task cannot shrink
    // the filesystem's bounded open-file pool for everyone else.
    let closed = crate::fs::close_task_files(&mut tasks[current].caps);
    // Destroy endpoints owned by the task and drop every endpoint alias it
    // holds. Other tasks that received a transferred alias now see a stale
    // object generation instead of a usable reference to a recycled slot.
    let (endpoint_handles, endpoints) =
        crate::ipc::close_task_endpoints(&mut tasks[current].caps, current as u16);
    let result = if status == 0 {
        UserTestResult::Passed
    } else {
        UserTestResult::Failed
    };
    USER_RESULT.store(result as usize, Ordering::Release);
    crate::apps::exited(current, status as i64);
    serial_line!(
        "user: exit pid={} status={status} closed={closed} endpoints={endpoints} \
         endpoint_handles={endpoint_handles}",
        current
    );

    schedule_next(current)
}

/// Returns the argument block's program name (its second entry).
fn program_name(args: &[u8]) -> Option<&str> {
    let mut entries = args.split(|&byte| byte == 0);
    let _directory = entries.next()?;
    core::str::from_utf8(entries.next()?).ok()
}

/// Checks an argument block: UTF-8, a working directory, a known program, and
/// at most [`MAX_ARGUMENTS`] entries.
fn valid_arguments(args: &[u8]) -> Result<&str, i64> {
    if args.is_empty() || args.len() > ARGUMENT_BLOCK_BYTES || core::str::from_utf8(args).is_err() {
        return Err(errno::EINVAL);
    }
    let trimmed = args.strip_suffix(&[0]).unwrap_or(args);
    if trimmed.split(|&byte| byte == 0).count() > MAX_ARGUMENTS {
        return Err(errno::EINVAL);
    }
    let program = program_name(args).ok_or(errno::EINVAL)?;
    if !aether_abi::user::is_terminal_program(program) {
        return Err(errno::ENOENT);
    }
    Ok(program)
}

/// Starts a native terminal program as a child of `parent` (0 for the
/// session's shell, which the desktop starts).
///
/// The program image is the kernel-embedded userland; its argument block
/// selects the entry point. The child runs at CPL3 in a fresh address space
/// under the user's terminal root, holds no capabilities, and inherits no
/// pipe ends beyond the standard input and output in `io`.
pub fn spawn_terminal_program(
    parent: usize,
    args: &[u8],
    io: crate::terminal::TaskIo,
) -> Result<usize, i64> {
    let _guard = timer::InterruptGuard::enter();
    let program = valid_arguments(args)?;
    let tasks = unsafe { &mut *TASKS.0.get() };
    let slot = tasks
        .iter()
        .position(|task| task.state == TaskState::Empty && !task.reserved)
        .ok_or(errno::EAGAIN)?;
    let image = elf::parse(USERLAND_ELF).map_err(|_| errno::EIO)?;
    let (space, frames) = crate::memory::with_frame_allocator(|allocator| {
        let before = allocator.stats().free_frames;
        let space = paging::create_user_address_space(allocator, &image, USERLAND_ELF);
        (space, before.saturating_sub(allocator.stats().free_frames))
    })
    .ok_or(errno::ENOSPC)?;
    let pml4 = space.map_err(|_| errno::ENOSPC)?;

    let kernel_top = slot_kernel_stack_top(slot)?;
    let mut task = Task::new();
    task.state = TaskState::Ready;
    task.kind = TaskKind::Terminal;
    task.saved_rsp = prepare_user_stack(kernel_top, image.entry(), (USER_STACK_TOP - 16) & !0xf);
    task.pml4 = pml4.start_address();
    task.kernel_stack_top = kernel_top;
    task.root = crate::fs::TERMINAL_ROOT;
    task.tgid = slot;
    task.bind_file_state(slot);
    task.parent = parent;
    task.io = io;
    task.memory_kib = (frames * (PAGE_SIZE / 1024)).min(u64::from(u32::MAX)) as u32;
    task.name = fixed_name(program);
    task.args[..args.len()].copy_from_slice(args);
    task.args_len = args.len() as u16;
    tasks[slot] = task;
    // SAFETY: Interrupts are masked and the slot is not running.
    unsafe { fpu_reset(slot) };
    Ok(slot)
}

/// Retires a terminal program: releases its files, endpoints, pipe ends, and
/// address space, then either keeps the status for its parent or frees the
/// slot when no parent will collect it.
fn retire_terminal_task(slot: usize, status: i64, faulted: bool) {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[slot];
    task.state = if faulted {
        TaskState::Faulted
    } else {
        TaskState::Done
    };
    task.exit_status = status;
    let _ = crate::fs::close_task_files(&mut task.caps);
    let _ = crate::ipc::close_task_endpoints(&mut task.caps, slot as u16);
    // The slot is reused by the next program, so its descriptor table must
    // not outlive this one.
    clear_file_state(slot);
    let io = task.io;
    task.io = crate::terminal::TaskIo::NONE;
    let pml4 = task.pml4;
    let parent = task.parent;

    // Exited children of this task can no longer be collected; running ones
    // are orphaned and freed when they exit.
    for (index, child) in tasks.iter_mut().enumerate() {
        if index == slot || child.kind != TaskKind::Terminal || child.parent != slot {
            continue;
        }
        if matches!(child.state, TaskState::Done | TaskState::Faulted) {
            *child = Task::new();
        } else {
            child.parent = 0;
        }
    }
    retire_address_space(pml4);
    crate::terminal::task_exited(slot, io, status);

    let parent_waits = parent != 0
        && parent < MAX_TASKS
        && tasks[parent].kind == TaskKind::Terminal
        && !matches!(
            tasks[parent].state,
            TaskState::Empty | TaskState::Done | TaskState::Faulted
        );
    if !parent_waits {
        // Nothing will collect the status. The slot's kernel stack is still
        // the active stack when the task retires itself, but a new task can
        // only be created by a later system call, after the switch.
        tasks[slot] = Task::new();
    }
}

fn terminal_task_live(task: &Task) -> bool {
    task.kind == TaskKind::Terminal
        && !matches!(
            task.state,
            TaskState::Empty | TaskState::Done | TaskState::Faulted
        )
}

/// Collects a child's exit status without blocking.
pub fn wait_child(pid: u64) -> i64 {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let Ok(pid) = usize::try_from(pid) else {
        return errno::ENOENT;
    };
    if pid == 0 || pid >= MAX_TASKS {
        return errno::ENOENT;
    }
    let child = &tasks[pid];
    if child.kind != TaskKind::Terminal || child.parent != current {
        return errno::ENOENT;
    }
    match child.state {
        TaskState::Done | TaskState::Faulted => {
            let status = child.exit_status;
            tasks[pid] = Task::new();
            status
        }
        _ => errno::EAGAIN,
    }
}

/// Outcome of a terminal `kill` request.
pub enum KillOutcome {
    /// Another task was terminated.
    Other,
    /// The caller named itself and must be retired by the dispatcher.
    Caller,
}

/// Terminates a live terminal program in the caller's session.
pub fn kill_terminal_task(pid: u64) -> Result<KillOutcome, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    let pid = usize::try_from(pid).map_err(|_| errno::ENOENT)?;
    if pid >= MAX_TASKS || tasks[pid].state == TaskState::Empty {
        return Err(errno::ENOENT);
    }
    if !terminal_task_live(&tasks[pid]) || !tasks[pid].io.session {
        // Kernel workers, services, applications, and exited programs are
        // outside the terminal's authority.
        return Err(errno::EACCES);
    }
    if pid == current {
        return Ok(KillOutcome::Caller);
    }
    retire_terminal_task(pid, 128 + 9, false);
    Ok(KillOutcome::Other)
}

/// Retires the calling terminal program with `status`.
pub fn exit_current_terminal_task(status: i64) -> *mut InterruptFrame {
    let current = CURRENT.load(Ordering::Relaxed);
    retire_terminal_task(current, status, false);
    schedule_next(current)
}

/// Terminates every live session program except `keep`, reporting `status`.
/// Returns how many were terminated.
pub fn kill_session(keep: usize, status: i64) -> usize {
    let _guard = timer::InterruptGuard::enter();
    let current = CURRENT.load(Ordering::Relaxed);
    let mut killed = 0;
    for slot in 1..MAX_TASKS {
        let tasks = unsafe { &*TASKS.0.get() };
        if slot == keep
            || slot == current
            || !terminal_task_live(&tasks[slot])
            || !tasks[slot].io.session
        {
            continue;
        }
        retire_terminal_task(slot, status, false);
        killed += 1;
    }
    killed
}

/// Returns `true` while a session program other than `leader` is running.
pub fn session_has_jobs(leader: usize) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks
        .iter()
        .enumerate()
        .any(|(slot, task)| slot != leader && terminal_task_live(task) && task.io.session)
}

/// Returns the current task's terminal I/O routing.
pub fn current_io() -> crate::terminal::TaskIo {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].io
}

/// Runs `f` on the current task's terminal I/O routing.
pub fn with_current_io<R>(f: impl FnOnce(&mut crate::terminal::TaskIo) -> R) -> R {
    let _guard = timer::InterruptGuard::enter();
    let tasks = unsafe { &mut *TASKS.0.get() };
    f(&mut tasks[CURRENT.load(Ordering::Relaxed)].io)
}

/// Copies the current task's argument block into `out`.
pub fn current_args(out: &mut [u8]) -> usize {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let length = usize::from(task.args_len).min(out.len());
    out[..length].copy_from_slice(&task.args[..length]);
    length
}

/// Fills `out` with one record per task and returns the count.
pub fn process_records(out: &mut [ProcessRecord]) -> usize {
    let _guard = timer::InterruptGuard::enter();
    let tasks = unsafe { &*TASKS.0.get() };
    let mut count = 0;
    for (pid, task) in tasks.iter().enumerate() {
        if task.state == TaskState::Empty || count == out.len() {
            continue;
        }
        let kind = match task.kind {
            TaskKind::Empty | TaskKind::Idle => ProcessKind::Idle,
            TaskKind::Worker => ProcessKind::Kernel,
            TaskKind::User => ProcessKind::Native,
            TaskKind::Service => ProcessKind::Service,
            TaskKind::Linux | TaskKind::Process => ProcessKind::Linux,
            TaskKind::Thread => ProcessKind::LinuxThread,
            TaskKind::Terminal => ProcessKind::Terminal,
        };
        let state = match task.state {
            TaskState::Ready | TaskState::Empty => ProcessState::Ready,
            TaskState::Running => ProcessState::Running,
            TaskState::Blocked => ProcessState::Blocked,
            TaskState::Done => ProcessState::Exited,
            TaskState::Faulted => ProcessState::Faulted,
        };
        let mut flags = 0;
        if task.io.session {
            flags |= PROCESS_FLAG_SESSION;
        }
        if !matches!(task.kind, TaskKind::Idle | TaskKind::Worker) {
            flags |= PROCESS_FLAG_USER;
        }
        let name = if task.name[0] == 0 {
            fixed_name(match task.kind {
                TaskKind::Idle | TaskKind::Empty => "kernel",
                TaskKind::Worker => "kworker",
                TaskKind::User => "aether-init",
                TaskKind::Service => "service",
                TaskKind::Linux | TaskKind::Process => "linux",
                TaskKind::Thread => "linux-thread",
                TaskKind::Terminal => "program",
            })
        } else {
            task.name
        };
        out[count] = ProcessRecord {
            pid: pid as u32,
            parent: task.parent as u32,
            state: state as u8,
            kind: kind as u8,
            flags,
            memory_kib: task.memory_kib,
            cpu_ticks: task.cpu_ticks,
            name,
        };
        count += 1;
    }
    count
}

/// Returns `(live tasks, task capacity, idle ticks)` for system summaries.
pub fn task_summary() -> (usize, usize, u64) {
    let tasks = unsafe { &*TASKS.0.get() };
    let live = tasks
        .iter()
        .filter(|task| {
            !matches!(
                task.state,
                TaskState::Empty | TaskState::Done | TaskState::Faulted
            )
        })
        .count();
    (live, MAX_TASKS, tasks[0].cpu_ticks)
}

const LINUX_EBADF: i64 = -9;
const LINUX_EFAULT: i64 = -14;
const LINUX_EACCES: i64 = -13;
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

/// Opens a Linux portal namespace path for the calling task.
///
/// The caller root is the kernel-owned task root. The namespace path is only a
/// name: the returned descriptor carries the exact backing path from the
/// matched grant, so a later read or write never resolves the guest string
/// again.
pub fn linux_portal_open(namespace: &str, flags: u64) -> Result<u64, i64> {
    use aether_abi::user::FileRights;
    let rights = match flags & 0x3 {
        0 => FileRights::READ,
        1 => FileRights::WRITE,
        2 => FileRights::READ | FileRights::WRITE,
        _ => return Err(LINUX_EINVAL),
    };
    let temp = namespace.ends_with(aether_services::portal_store::TEMP_SUFFIX);
    // A direct path names one existing file; only its staging sibling may be
    // created. O_TRUNC is allowed only with a write right.
    if flags & 0x40 != 0 && !temp {
        return Err(LINUX_EACCES);
    }
    let truncate = flags & 0x200 != 0;
    if (truncate || temp) && rights & FileRights::WRITE == 0 {
        return Err(LINUX_EACCES);
    }
    let root = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].root
    };
    crate::fs::portal_permission(root, rights)?;
    if temp {
        let (index, generation) =
            crate::portal::open_temp(root, namespace, truncate).map_err(portal_error_code)?;
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
        if task.abi != TaskAbi::Linux {
            return Err(LINUX_EBADF);
        }
        for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
            if slot.is_none() {
                *slot = Some(LinuxFile {
                    source: LinuxFileSource::PortalTemp {
                        index,
                        generation,
                        rights: rights as u8,
                    },
                    offset: 0,
                    writable: rights & FileRights::WRITE != 0,
                });
                return Ok(descriptor as u64);
            }
        }
        return Err(LINUX_EMFILE);
    }
    let grant = match crate::portal::check_namespace(root, namespace, rights) {
        Ok(grant) => grant,
        Err(error) => {
            #[cfg(feature = "gate4-portal-test")]
            if GATE4_REVOKED.load(Ordering::Acquire) != 0
                && error == crate::portal::PortalError::Revoked
            {
                if rights & FileRights::READ != 0 {
                    GATE4_DENIED_READS.fetch_add(1, Ordering::Release);
                }
                if rights & FileRights::WRITE != 0 {
                    GATE4_DENIED_WRITES.fetch_add(1, Ordering::Release);
                }
            }
            return Err(portal_error_code(error));
        }
    };
    let backing = grant.path();
    let length = backing.len();
    if length > PORTAL_PATH_BYTES {
        return Err(LINUX_ENAMETOOLONG);
    }
    crate::fs::portal_size(backing).map_err(portal_fs_error)?;
    if truncate {
        crate::fs::portal_truncate(backing).map_err(portal_fs_error)?;
    }
    let mut path = [0u8; PORTAL_PATH_BYTES];
    path[..length].copy_from_slice(backing.as_bytes());
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Portal {
                    path,
                    len: length as u16,
                    rights: rights as u8,
                },
                offset: 0,
                writable: rights & FileRights::WRITE != 0,
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Maps a portal policy failure to a Linux errno.
fn portal_error_code(error: crate::portal::PortalError) -> i64 {
    use crate::portal::PortalError;
    match error {
        PortalError::TempFull | PortalError::TempQuota => LINUX_ENOSPC,
        PortalError::NoTemp => LINUX_ENOENT,
        _ => LINUX_EACCES,
    }
}

/// Commits a staging namespace path to its exact granted file.
pub fn linux_portal_rename(from: &str, to: &str) -> Result<(), i64> {
    let root = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].root
    };
    let (grant, length) = crate::portal::commit_temp(root, from, to).map_err(portal_error_code)?;
    #[cfg(feature = "gate4-gui-test")]
    gate4_gui_emit_save(&grant);
    let _ = &grant;
    #[cfg(feature = "gate4-portal-test")]
    {
        GATE4_RENAMES.fetch_add(1, Ordering::Release);
        GATE4_RENAMED_BYTES.fetch_add(length, Ordering::Release);
    }
    #[cfg(not(feature = "gate4-portal-test"))]
    let _ = length;
    Ok(())
}

/// Emits the GUI save marker immediately after a validated XLSX commit.
///
/// The real XLSX fixture is a ZIP container. A commit only counts when the
/// kernel reads back more than 1 KiB and the local-file header magic. This is
/// deliberately independent of guest output and of the post-exit path.
#[cfg(feature = "gate4-gui-test")]
fn gate4_gui_emit_save(grant: &aether_services::portal_store::PortalGrant) {
    if grant.root() != crate::fs::DEMO_ROOT
        || grant.path() != crate::fs::GATE4_GUI_WORKBOOK
        || GATE4_GUI_SAVE_MARKER.load(Ordering::Acquire) != 0
    {
        return;
    }
    let mut data = [0u8; aether_fs::MAX_FILE_BYTES];
    let Ok(length) = crate::fs::read_path(crate::fs::GATE4_GUI_WORKBOOK, &mut data) else {
        serial_line!("AETHER_BOOT_FAILED reason=gate4-gui save readback");
        return;
    };
    if length <= 1024 || data[..4] != [b'P', b'K', 3, 4] {
        serial_line!("AETHER_BOOT_FAILED reason=gate4-gui committed bytes are not xlsx");
        return;
    }
    GATE4_GUI_SAVE_MARKER.store(1, Ordering::Release);
    serial_line!(
        "AETHER_GATE4_SAVE_OK file={} bytes={length} staging=sctmp+rename source=kernel",
        crate::fs::GATE4_GUI_WORKBOOK
    );
}

/// Discards a staging namespace path without touching the granted file.
pub fn linux_portal_unlink(namespace: &str) -> Result<(), i64> {
    let root = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].root
    };
    crate::portal::discard_temp(root, namespace).map_err(portal_error_code)
}

/// Maps a filesystem failure on a portal backing file to a Linux errno.
fn portal_fs_error(error: FsError) -> i64 {
    match error {
        FsError::Access | FsError::Escape => LINUX_EACCES,
        FsError::NotFound => LINUX_ENOENT,
        FsError::NameTooLong | FsError::PathTooLong => LINUX_ENAMETOOLONG,
        FsError::FileTooLarge | FsError::NoSpace => LINUX_ENOSPC,
        _ => -5, // EIO
    }
}

/// Revokes the Gate 4 workbook grant through the test-only control path.
///
/// This exists only in the `gate4-portal-test` build and only for the demo
/// application root. Revocation remains a kernel operation; the guest supplies
/// no grant state or marker text.
#[cfg(feature = "gate4-portal-test")]
pub fn gate4_revoke_portal() -> Result<(), i64> {
    let root = {
        let tasks = unsafe { &*TASKS.0.get() };
        tasks[CURRENT.load(Ordering::Relaxed)].root
    };
    if root != crate::fs::DEMO_ROOT {
        return Err(LINUX_EACCES);
    }
    crate::portal::revoke(root, crate::fs::GATE4_WORKBOOK).map_err(|_| LINUX_EACCES)?;
    GATE4_REVOKED.store(1, Ordering::Release);
    Ok(())
}

/// Emits kernel-owned Gate 4 portal evidence after the application exits.
#[cfg(feature = "gate4-portal-test")]
pub fn gate4_portal_post_exit() {
    let read_bytes = GATE4_PORTAL_READ_BYTES.load(Ordering::Acquire);
    if read_bytes == 0 {
        serial_line!("AETHER_BOOT_FAILED reason=gate4 portal read not observed");
        return;
    }
    serial_line!(
        "AETHER_GATE4_PORTAL_OK path={} read_bytes={read_bytes} source=kernel",
        crate::fs::GATE4_NAMESPACE
    );

    let mut saved = [0u8; aether_fs::MAX_FILE_BYTES];
    let staged_bytes = GATE4_TEMP_WRITE_BYTES.load(Ordering::Acquire);
    let committed_bytes = GATE4_RENAMED_BYTES.load(Ordering::Acquire);
    let renames = GATE4_RENAMES.load(Ordering::Acquire);
    match crate::fs::read_path(crate::fs::GATE4_WORKBOOK, &mut saved) {
        Ok(length)
            if renames == 1
                && committed_bytes == length
                && length == crate::fs::GATE4_SAVED_FIXTURE.len()
                && saved[..length] == *crate::fs::GATE4_SAVED_FIXTURE =>
        {
            serial_line!(
                "AETHER_GATE4_SAVE_OK file={} bytes={} staging=sctmp+rename temp_bytes={} staged_bytes={} renames={} source=kernel",
                crate::fs::GATE4_WORKBOOK,
                length,
                committed_bytes,
                staged_bytes,
                renames
            );
        }
        _ => {
            serial_line!(
                "AETHER_BOOT_FAILED reason=gate4 saved bytes not observed renames={renames} temp_bytes={committed_bytes} staged_bytes={staged_bytes}"
            );
            return;
        }
    }

    let denied_reads = GATE4_DENIED_READS.load(Ordering::Acquire);
    let denied_writes = GATE4_DENIED_WRITES.load(Ordering::Acquire);
    if GATE4_REVOKED.load(Ordering::Acquire) == 0 || denied_reads == 0 || denied_writes == 0 {
        serial_line!(
            "AETHER_BOOT_FAILED reason=gate4 revoke denials read={denied_reads} write={denied_writes}"
        );
        return;
    }
    serial_line!(
        "AETHER_GATE4_REVOKE_OK path={} read_denials={denied_reads} write_denials={denied_writes} source=kernel",
        crate::fs::GATE4_NAMESPACE
    );
}

/// Emits kernel-owned Gate 4 GUI evidence after the application exits.
///
/// The harness closes the GUI after the immediate save marker. This path then
/// revokes the GUI grant itself and proves that a fresh read, temp open, and
/// rename all fail before claiming revocation success.
#[cfg(feature = "gate4-gui-test")]
pub fn gate4_gui_post_exit() {
    use aether_abi::user::FileRights;
    if GATE4_GUI_POST_EXIT.swap(1, Ordering::AcqRel) != 0 {
        return;
    }
    let read_bytes = GATE4_GUI_READ_BYTES.load(Ordering::Acquire);
    if read_bytes == 0 {
        serial_line!("AETHER_BOOT_FAILED reason=gate4-gui portal read not observed");
        return;
    }
    serial_line!(
        "AETHER_GATE4_PORTAL_OK path={} read_bytes={read_bytes} source=kernel",
        crate::fs::GATE4_GUI_NAMESPACE
    );
    if GATE4_GUI_SAVE_MARKER.load(Ordering::Acquire) == 0 {
        serial_line!("AETHER_BOOT_FAILED reason=gate4-gui save marker missing");
        return;
    }
    if crate::portal::revoke(crate::fs::DEMO_ROOT, crate::fs::GATE4_GUI_WORKBOOK).is_err() {
        serial_line!("AETHER_BOOT_FAILED reason=gate4-gui revoke");
        return;
    }
    let read_denied = matches!(
        crate::portal::check_namespace(
            crate::fs::DEMO_ROOT,
            crate::fs::GATE4_GUI_NAMESPACE,
            FileRights::READ
        ),
        Err(crate::portal::PortalError::Revoked)
    );
    let write_denied = matches!(
        crate::portal::open_temp(
            crate::fs::DEMO_ROOT,
            crate::fs::GATE4_GUI_TEMP_NAMESPACE,
            true
        ),
        Err(crate::portal::PortalError::Revoked)
    );
    let rename_denied = matches!(
        crate::portal::commit_temp(
            crate::fs::DEMO_ROOT,
            crate::fs::GATE4_GUI_TEMP_NAMESPACE,
            crate::fs::GATE4_GUI_NAMESPACE
        ),
        Err(crate::portal::PortalError::Revoked)
    );
    if !read_denied || !write_denied || !rename_denied {
        serial_line!(
            "AETHER_BOOT_FAILED reason=gate4-gui revoke denials read={read_denied} write={write_denied} rename={rename_denied}"
        );
        return;
    }
    serial_line!(
        "AETHER_GATE4_REVOKE_OK path={} read_denials=1 write_denials=1 source=kernel",
        crate::fs::GATE4_GUI_NAMESPACE
    );
}

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
        "/etc/resolv.conf" => Some(PseudoFile::ResolvConf),
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
        PseudoFile::ProcMaps
        | PseudoFile::ProcStatus
        | PseudoFile::SyscallAudit
        | PseudoFile::ResolvConf => (0, 6, 0o100444),
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

/// Opens the synthetic display endpoint for the current Linux task.
///
/// The endpoint is a single descriptor per task; a second open returns
/// `EBUSY`. Policy is checked in the display service against the kernel task
/// grant before a descriptor is consumed.
pub fn linux_open_display() -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    if task
        .linux_fds
        .iter()
        .flatten()
        .any(|file| matches!(file.source, LinuxFileSource::Display))
    {
        return Err(-16); // EBUSY
    }
    let Some(descriptor) = (3..crate::linux::MAX_FDS).find(|slot| task.linux_fds[*slot].is_none())
    else {
        return Err(LINUX_EMFILE);
    };
    crate::display::open()?;
    task.linux_fds[descriptor] = Some(LinuxFile {
        source: LinuxFileSource::Display,
        offset: 0,
        writable: true,
    });
    Ok(descriptor as u64)
}

/// Creates a bounded anonymous shared-memory object and a descriptor for it.
pub fn linux_memfd_create() -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    if task
        .linux_fds
        .iter()
        .flatten()
        .filter(|file| matches!(file.source, LinuxFileSource::MemFd { .. }))
        .count()
        >= crate::memfd::MAX_MEMFDS
    {
        return Err(LINUX_EMFILE);
    }
    let Some(descriptor) = (3..crate::linux::MAX_FDS).find(|slot| task.linux_fds[*slot].is_none())
    else {
        return Err(LINUX_EMFILE);
    };
    let index = crate::memfd::create()?;
    task.linux_fds[descriptor] = Some(LinuxFile {
        source: LinuxFileSource::MemFd { index },
        offset: 0,
        writable: true,
    });
    Ok(descriptor as u64)
}

/// Returns `true` when `fd` names the current task's display endpoint.
#[must_use]
pub fn linux_display_fd(fd: u64) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)]
        .linux_fds
        .get(fd as usize)
        .and_then(Option::as_ref)
        .is_some_and(|file| matches!(file.source, LinuxFileSource::Display))
}

/// Resizes the memfd behind `fd`.
pub fn linux_ftruncate(fd: u64, length: i64) -> i64 {
    if !crate::memfd::valid_length(length) {
        return LINUX_EINVAL;
    }
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(source) = task
        .linux_fds
        .get(fd as usize)
        .and_then(Option::as_ref)
        .map(|file| file.source)
    else {
        return LINUX_EBADF;
    };
    let LinuxFileSource::MemFd { index } = source else {
        return LINUX_EINVAL;
    };
    crate::memfd::ftruncate(index, length as usize).map_or_else(|error| error, |()| 0)
}

/// Accepts `fsync`/`fdatasync` for an open descriptor.
///
/// Every writable byte lives in RAM-backed storage in this prototype, so there
/// is no medium to flush. An invalid descriptor still fails closed with
/// `EBADF` rather than silently succeeding.
pub fn linux_fsync(fd: u64) -> i64 {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    if task
        .linux_fds
        .get(fd as usize)
        .and_then(Option::as_ref)
        .is_some()
    {
        0
    } else {
        LINUX_EBADF
    }
}

/// Returns `true` for a path mapped to the per-application writable store.
#[must_use]
pub fn is_overlay_path(path: &str) -> bool {
    path.starts_with("/tmp/") || path.starts_with("/settings/") || path.starts_with("/logs/")
}

fn overlays() -> &'static mut [Option<LinuxOverlay>; OVERLAY_FILES] {
    // SAFETY: Access is serialized by the single-CPU scheduler and interrupt
    // masking, exactly like TASKS.
    unsafe { &mut *OVERLAYS.0.get() }
}

fn overlay_index(app: u8, path: &str) -> Option<usize> {
    let store = unsafe { &*OVERLAYS.0.get() };
    store.iter().position(|entry| {
        entry
            .as_ref()
            .is_some_and(|entry| entry.app == app && entry.path() == path)
    })
}

/// Returns the bytes an application currently holds in its overlay quota.
#[must_use]
pub fn linux_overlay_used(app: u8) -> usize {
    let store = unsafe { &*OVERLAYS.0.get() };
    store
        .iter()
        .flatten()
        .filter(|entry| entry.app == app)
        .map(|entry| entry.len)
        .sum()
}

/// Returns the persisted entry count and bytes for an application store.
#[must_use]
pub fn linux_overlay_stats(app: u8) -> (usize, usize) {
    let store = unsafe { &*OVERLAYS.0.get() };
    let mut entries = 0usize;
    let mut bytes = 0usize;
    for entry in store.iter().flatten().filter(|entry| entry.app == app) {
        entries += 1;
        bytes += entry.len;
    }
    (entries, bytes)
}

/// Erases every overlay entry for an application and returns the count.
///
/// This is the user-visible erasure operation behind `linux-erase`. Existing
/// descriptors to erased entries fail closed with `EBADF` instead of exposing
/// another application's data; the stored entries and their quota accounting
/// are removed.
pub fn linux_overlay_erase(app: u8) -> usize {
    let store = overlays();
    let mut erased = 0usize;
    for entry in store.iter_mut() {
        if entry.as_ref().is_some_and(|entry| entry.app == app) {
            *entry = None;
            erased += 1;
        }
    }
    erased
}

/// Opens a read-only file, preferring an existing overlay entry.
pub fn linux_open_read(path: &str) -> Result<u64, i64> {
    if pseudo_file(path).is_some() || pseudo_dir(path).is_some() {
        return linux_open_pseudo(path, false);
    }
    if is_overlay_path(path) {
        return linux_overlay_open(path, false, false, false);
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
    open: &[bool],
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
            if descriptor >= 3 && !open.get(descriptor).copied().unwrap_or(false) {
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
    // A snapshot, so the listing does not alias the descriptor it advances.
    let open: Vec<bool> = task.linux_fds.iter().map(Option::is_some).collect();
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
        pseudo_dir_entry(directory, slot.offset, &open)
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

/// Opens or creates a file in the application's writable overlay.
///
/// `/tmp`, `/settings`, and `/logs` are runtime path mappings, not host
/// authority. Entries are bounded per file and per application, the store is
/// shared by processes with the same application identity, and the store
/// outlives a task until the erasure path is used. It is RAM-backed, so it does
/// not survive a power cycle.
pub fn linux_overlay_open(
    path: &str,
    create: bool,
    truncate: bool,
    writable: bool,
) -> Result<u64, i64> {
    if !is_overlay_path(path) || !crate::linux::valid_path(path) {
        return Err(LINUX_EROFS);
    }
    if path.len() > OVERLAY_PATH {
        return Err(LINUX_ENAMETOOLONG);
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    let app = task.overlay_app;
    let mut created = false;
    let mut truncated = false;
    let mut overlay_index = overlay_index(app, path);
    if overlay_index.is_none() {
        if !create {
            return Err(LINUX_ENOENT);
        }
        let store = overlays();
        let index = store.iter().position(Option::is_none).ok_or(LINUX_ENOSPC)?;
        let mut entry = LinuxOverlay::EMPTY;
        entry.app = app;
        entry.path[..path.len()].copy_from_slice(path.as_bytes());
        entry.path_len = path.len();
        store[index] = Some(entry);
        overlay_index = Some(index);
        created = true;
    }
    let index = overlay_index.unwrap();
    if truncate && let Some(entry) = overlays()[index].as_mut() {
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

/// Creates a virtual directory in the writable overlay namespace.
///
/// The bounded overlay stores files, not directory inodes. A directory under
/// `/tmp`, `/settings`, or `/logs` is therefore accepted and later file opens
/// beneath it work; `getdents64` does not list it until a file is created
/// there. The root namespace is read-only, so any other path fails with
/// `EROFS` after existing directories report `EEXIST`.
pub fn linux_overlay_mkdir(path: &str) -> Result<(), i64> {
    if !crate::linux::valid_path(path) {
        return Err(LINUX_EINVAL);
    }
    let app = overlay_task_app()?;
    if crate::linux::is_dir(path)
        || pseudo_dir(path).is_some()
        || overlay_index(app, path).is_some()
    {
        return Err(LINUX_EEXIST);
    }
    if !is_overlay_path(path) && !matches!(path, "/tmp" | "/settings" | "/logs") {
        return Err(LINUX_EROFS);
    }
    if path.len() > OVERLAY_PATH {
        return Err(LINUX_ENAMETOOLONG);
    }
    Ok(())
}

/// Validates a path that a mutating overlay operation may touch.
///
/// Root paths are deliberately refused with `EROFS`: the imported tree is
/// immutable, and a missing or malformed path must not silently become a new
/// overlay entry.
fn overlay_operation_path(path: &str) -> Result<(), i64> {
    if !crate::linux::valid_path(path) {
        return Err(LINUX_EINVAL);
    }
    if !is_overlay_path(path) {
        return Err(LINUX_EROFS);
    }
    if path.len() > OVERLAY_PATH {
        return Err(LINUX_ENAMETOOLONG);
    }
    Ok(())
}

/// Returns the current Linux task's private overlay domain.
fn overlay_task_app() -> Result<u8, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    if task.abi != TaskAbi::Linux {
        return Err(LINUX_EBADF);
    }
    Ok(task.overlay_app)
}

/// Moves an overlay entry from `old` to `new`.
///
/// Both paths must be absolute, normalized overlay paths owned by the current
/// task's application. The source entry keeps its index, so descriptors opened
/// before the rename continue to read the same bytes. A replaced target entry
/// is dropped; descriptors that still reference it become stale and fail closed
/// with `EBADF`, matching the erasure path. This is the same honest limitation
/// as POSIX unlink: an open descriptor keeps the old inode, but this bounded
/// store has no separate link count, so a removed entry cannot be retained.
pub fn linux_overlay_rename(old: &str, new: &str) -> Result<(), i64> {
    overlay_operation_path(old)?;
    overlay_operation_path(new)?;
    let app = overlay_task_app()?;
    let Some(source) = overlay_index(app, old) else {
        return Err(LINUX_ENOENT);
    };
    if old == new {
        return Ok(());
    }
    let target = overlay_index(app, new);
    let store = overlays();
    if let Some(target) = target {
        store[target] = None;
    }
    let Some(entry) = store[source].as_mut() else {
        return Err(LINUX_ENOENT);
    };
    entry.path = [0; OVERLAY_PATH];
    entry.path[..new.len()].copy_from_slice(new.as_bytes());
    entry.path_len = new.len();
    if target.is_none() {
        // The event machinery only implements IN_CREATE/IN_OPEN/IN_MODIFY and
        // close events; there is no IN_MOVED_TO constant to fabricate.
        linux_inotify_emit(new, IN_CREATE, false);
    }
    Ok(())
}

/// Removes one entry from the current task's private overlay.
///
/// The read-only root is never touched. Existing descriptors to the removed
/// entry fail closed with `EBADF`; a link count or deferred deletion would need
/// storage this bounded prototype does not have. No inotify delete event is
/// emitted because the event machinery has no `IN_DELETE` constant.
pub fn linux_overlay_unlink(path: &str) -> Result<(), i64> {
    overlay_operation_path(path)?;
    let app = overlay_task_app()?;
    let Some(index) = overlay_index(app, path) else {
        return Err(LINUX_ENOENT);
    };
    overlays()[index] = None;
    Ok(())
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
    // Ends queued in `SCM_RIGHTS` messages still count as open.
    let (queued_readers, queued_writers) = crate::socket::in_flight_pipe_users(index);
    (readers + queued_readers, writers + queued_writers)
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

/// User-visible erasure entry point used by `linux-erase`.
pub fn erase_application_overlay(name: &str) -> Option<usize> {
    if name == "appimage" || name == "1" {
        Some(linux_overlay_erase(crate::linuxd::APP_OVERLAY_ID))
    } else if name == "test" || name == "0" {
        Some(linux_overlay_erase(0))
    } else {
        None
    }
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
            unsafe { (*PIPES.0.get())[index] = None };
            Err(LINUX_EMFILE)
        }
    }
}

/// Installs a socket slot in the lowest free descriptor above stdio. The
/// slot's existing reference becomes the descriptor's.
pub fn linux_install_socket(index: usize) -> Result<u64, i64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::Socket { index },
                offset: 0,
                writable: true,
            });
            return Ok(descriptor as u64);
        }
    }
    Err(LINUX_EMFILE)
}

/// Returns the socket slot behind `fd`: `EBADF` for a closed descriptor and
/// `ENOTSOCK` for any other kind.
pub fn linux_socket_index(fd: u64) -> Result<usize, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    match task.linux_fds.get(fd as usize).and_then(Option::as_ref) {
        Some(LinuxFile {
            source: LinuxFileSource::Socket { index },
            ..
        }) => Ok(*index),
        Some(_) => Err(crate::socket::ENOTSOCK),
        None => Err(LINUX_EBADF),
    }
}

/// A descriptor in flight in an `SCM_RIGHTS` message.
///
/// It holds the object reference a descriptor would hold, so the memfd,
/// socket, or pipe behind it stays alive while no task has it installed.
/// Dropping it drops that reference.
pub struct LinuxPassedFile(LinuxFile);

impl LinuxPassedFile {
    /// The pipe slot and direction, for pipe-user counting.
    #[must_use]
    pub fn pipe_end(&self) -> Option<(usize, bool)> {
        match self.0.source {
            LinuxFileSource::Pipe { index, write } => Some((index, write)),
            _ => None,
        }
    }

    /// The socket slot, for the in-flight cycle check.
    #[must_use]
    pub fn socket_index(&self) -> Option<usize> {
        match self.0.source {
            LinuxFileSource::Socket { index } => Some(index),
            _ => None,
        }
    }
}

impl Drop for LinuxPassedFile {
    fn drop(&mut self) {
        match self.0.source {
            LinuxFileSource::Pipe { index, .. } => {
                // `pipe_users` no longer sees this end once it is dropped.
                linux_release_pipe(index);
                linux_wake_pipe(index);
                linux_wake_pollers();
            }
            source => release_shared_object(source),
        }
    }
}

/// Takes a reference to the object behind `fd` for `SCM_RIGHTS`.
///
/// Descriptors whose state lives in the sending task (eventfd, epoll,
/// timerfd, inotify, the display endpoint) cannot be passed and fail with
/// `EINVAL`, so a receiver never indexes another task's tables.
pub fn linux_pass_file(fd: u64) -> Result<LinuxPassedFile, i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let file = task
        .linux_fds
        .get(fd as usize)
        .copied()
        .flatten()
        .ok_or(LINUX_EBADF)?;
    if matches!(
        file.source,
        LinuxFileSource::EventFd { .. }
            | LinuxFileSource::Epoll { .. }
            | LinuxFileSource::TimerFd { .. }
            | LinuxFileSource::Inotify
            | LinuxFileSource::Display
    ) {
        return Err(LINUX_EINVAL);
    }
    retain_shared_object(file.source);
    Ok(LinuxPassedFile(file))
}

/// Installs a passed descriptor in the lowest free slot above stdio. The
/// passed reference becomes the descriptor's; with no free slot the file is
/// dropped and `None` returned.
pub fn linux_receive_file(file: LinuxPassedFile) -> Option<u64> {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(descriptor) = task
        .linux_fds
        .iter()
        .enumerate()
        .skip(3)
        .find_map(|(descriptor, slot)| slot.is_none().then_some(descriptor))
    else {
        drop(file);
        return None;
    };
    let file = core::mem::ManuallyDrop::new(file);
    task.linux_fds[descriptor] = Some(file.0);
    Some(descriptor as u64)
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
    task.linux_timerfds[index] = Some(LinuxTimerFd {
        realtime: clockid == 0,
        ..LinuxTimerFd::EMPTY
    });
    for (descriptor, slot) in task.linux_fds.iter_mut().enumerate().skip(3) {
        if slot.is_none() {
            *slot = Some(LinuxFile {
                source: LinuxFileSource::TimerFd { index },
                offset: 0,
                writable: false,
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

/// Returns `true` if a timer descriptor was created on `CLOCK_REALTIME`.
pub fn linux_timerfd_is_realtime(fd: u64) -> Result<bool, i64> {
    let (current, index) = timerfd_slot(fd)?;
    let tasks = unsafe { &*TASKS.0.get() };
    Ok(tasks[current].linux_timerfds[index].is_some_and(|timer| timer.realtime))
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
        | LinuxFileSource::Portal { .. }
        | LinuxFileSource::PortalTemp { .. }
        | LinuxFileSource::Directory { .. } => {
            ready = events & (POLL_IN | POLL_OUT);
        }
        LinuxFileSource::MemFd { .. } => {
            ready = events & (POLL_IN | POLL_OUT);
        }
        LinuxFileSource::Console => {
            ready = events & (POLL_IN | POLL_OUT);
        }
        LinuxFileSource::Display => {
            if events & POLL_IN != 0 && crate::display::readable() {
                ready |= POLL_IN;
            }
        }
        LinuxFileSource::Socket { index } => {
            ready = crate::socket::poll(index, events);
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
            if events & POLL_IN != 0
                && inotify_state(task.tgid).is_some_and(|state| state.has_data())
            {
                ready |= POLL_IN;
            }
        }
        LinuxFileSource::Pseudo(_) | LinuxFileSource::PseudoDir(_) => {
            ready = events & POLL_IN;
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
        crate::random::fill(out);
        return Ok(out.len());
    }
    if kind == PseudoFile::SyscallAudit {
        return Ok(crate::linux_policy::audit_text(offset, out));
    }
    let text = match kind {
        PseudoFile::ProcExe => None,
        PseudoFile::ProcMaps => Some(proc_maps_text(vmas)),
        PseudoFile::ProcStatus => Some(proc_status_text(vmas, tgid, parent)),
        PseudoFile::ResolvConf => {
            let mut text = PseudoText {
                buffer: [0; 4096],
                length: 0,
            };
            text.length = crate::net::resolv_conf(&mut text.buffer);
            Some(text)
        }
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
                // SAFETY: overlay access is serialized like the task table.
                let store = unsafe { &*OVERLAYS.0.get() };
                let Some(entry) = store.get(index).and_then(Option::as_ref) else {
                    return Err(LINUX_EBADF);
                };
                let remaining = entry.len.saturating_sub(slot.offset);
                let length = remaining.min(out.len());
                out[..length].copy_from_slice(&entry.data[slot.offset..slot.offset + length]);
                slot.offset += length;
                Ok(length)
            }
            LinuxFileSource::Portal { path, len, rights } => {
                if rights & aether_abi::user::FileRights::READ as u8 == 0 {
                    return Err(LINUX_EBADF);
                }
                let path =
                    core::str::from_utf8(&path[..usize::from(len)]).map_err(|_| LINUX_ENOENT)?;
                let length =
                    crate::fs::portal_read_at(path, slot.offset, out).map_err(portal_fs_error)?;
                slot.offset = slot.offset.saturating_add(length);
                #[cfg(feature = "gate4-portal-test")]
                GATE4_PORTAL_READ_BYTES.fetch_add(length, Ordering::Release);
                #[cfg(feature = "gate4-gui-test")]
                if path == crate::fs::GATE4_GUI_WORKBOOK {
                    GATE4_GUI_READ_BYTES.fetch_add(length, Ordering::Release);
                }
                Ok(length)
            }
            LinuxFileSource::PortalTemp {
                index,
                generation,
                rights,
            } => {
                if rights & aether_abi::user::FileRights::READ as u8 == 0 {
                    return Err(LINUX_EBADF);
                }
                let length = crate::portal::read_temp(index, generation, slot.offset, out)
                    .map_err(portal_error_code)?;
                slot.offset = slot.offset.saturating_add(length);
                Ok(length)
            }
            LinuxFileSource::Directory { .. } => Err(LINUX_EISDIR),
            LinuxFileSource::MemFd { index } => {
                let count = crate::memfd::read(index, slot.offset, out)?;
                slot.offset = slot.offset.saturating_add(count);
                Ok(count)
            }
            // The syscall layer drains the endpoint before it borrows the task
            // table, because queueing a wakeup touches the scheduler too.
            LinuxFileSource::Display => Err(LINUX_EINVAL),
            LinuxFileSource::Socket { index } => crate::socket::read(index, out),
            LinuxFileSource::Console => Ok(0),
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
            LinuxFileSource::Pseudo(kind) => {
                let count =
                    pseudo_read(kind, slot.offset, out, &task.vmas, task.tgid, task.parent)?;
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

pub fn linux_write_file(fd: u64, data: &[u8]) -> Result<usize, i64> {
    let pipe = linux_pipe_end(fd);
    let eventfd = {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[CURRENT.load(Ordering::Relaxed)];
        task.linux_fds
            .get(fd as usize)
            .and_then(Option::as_ref)
            .and_then(|file| match file.source {
                LinuxFileSource::EventFd { index } => Some(index),
                _ => None,
            })
    };
    let result = {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[CURRENT.load(Ordering::Relaxed)];
        let Some(slot) = task.linux_fds.get_mut(fd as usize).and_then(Option::as_mut) else {
            return Err(LINUX_EBADF);
        };
        match slot.source {
            LinuxFileSource::Overlay(index) => {
                let app = task.overlay_app;
                let used = linux_overlay_used(app);
                // SAFETY: overlay access is serialized like the task table.
                let store = unsafe { &mut *OVERLAYS.0.get() };
                let Some(entry) = store.get_mut(index).and_then(Option::as_mut) else {
                    return Err(LINUX_EBADF);
                };
                let end = slot.offset.checked_add(data.len()).ok_or(LINUX_ENOSPC)?;
                if end > OVERLAY_BYTES {
                    return Err(LINUX_ENOSPC);
                }
                let retained = used.saturating_sub(entry.len);
                if retained + end > OVERLAY_APP_BYTES {
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
            LinuxFileSource::Portal { path, len, rights } => {
                if rights & aether_abi::user::FileRights::WRITE as u8 == 0 {
                    return Err(LINUX_EBADF);
                }
                let path =
                    core::str::from_utf8(&path[..usize::from(len)]).map_err(|_| LINUX_ENOENT)?;
                let length =
                    crate::fs::portal_write_at(path, slot.offset, data).map_err(portal_fs_error)?;
                slot.offset = slot.offset.saturating_add(length);
                #[cfg(feature = "gate4-portal-test")]
                GATE4_PORTAL_WRITE_BYTES.fetch_add(length, Ordering::Release);
                Ok(length)
            }
            LinuxFileSource::PortalTemp {
                index,
                generation,
                rights,
            } => {
                if rights & aether_abi::user::FileRights::WRITE as u8 == 0 {
                    return Err(LINUX_EBADF);
                }
                let length = crate::portal::write_temp(index, generation, slot.offset, data)
                    .map_err(portal_error_code)?;
                slot.offset = slot.offset.saturating_add(length);
                #[cfg(feature = "gate4-portal-test")]
                GATE4_TEMP_WRITE_BYTES.fetch_add(length, Ordering::Release);
                Ok(length)
            }
            LinuxFileSource::MemFd { index } => {
                let count = crate::memfd::write(index, slot.offset, data)?;
                slot.offset = slot.offset.saturating_add(count);
                Ok(count)
            }
            LinuxFileSource::Display => Err(LINUX_EINVAL),
            LinuxFileSource::Socket { index } => crate::socket::write(index, data),
            LinuxFileSource::Console => {
                crate::arch::x86_64::serial::write_bytes(data);
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
    // A successful eventfd write makes the descriptor readable. Poll and
    // epoll waiters must observe that transition; they re-evaluate readiness
    // and park again, so a spurious wake is harmless.
    if result.is_ok() && eventfd.is_some() {
        linux_wake_pollers();
        refresh_epoll_edges(fd);
    }
    result
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
    let app = task.overlay_app;
    let store = unsafe { &*OVERLAYS.0.get() };
    let exists = path == "/tmp"
        || pseudo_file(path).is_some()
        || pseudo_dir(path).is_some()
        || crate::linux::lookup(path).is_some()
        || crate::linux::is_dir(path)
        || store
            .iter()
            .flatten()
            .any(|entry| entry.app == app && entry.path() == path);
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
    #[cfg(feature = "gate4-gui-test")]
    trace_block("inotify");
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
            let store = unsafe { &*OVERLAYS.0.get() };
            let Some(entry) = store.get(index).and_then(Option::as_ref) else {
                return Err(LINUX_EBADF);
            };
            if offset > entry.len {
                return Ok(0);
            }
            let length = (entry.len - offset).min(out.len());
            out[..length].copy_from_slice(&entry.data[offset..offset + length]);
            Ok(length)
        }
        LinuxFileSource::Portal { path, len, .. } => {
            let path = core::str::from_utf8(&path[..usize::from(len)]).map_err(|_| LINUX_ENOENT)?;
            crate::fs::portal_read_at(path, offset, out).map_err(portal_fs_error)
        }
        LinuxFileSource::PortalTemp {
            index, generation, ..
        } => crate::portal::read_temp(index, generation, offset, out).map_err(portal_error_code),
        LinuxFileSource::MemFd { index } => crate::memfd::read(index, offset, out),
        LinuxFileSource::Display => Err(LINUX_EINVAL),
        LinuxFileSource::Console => Err(LINUX_ESPIPE),
        LinuxFileSource::Directory { .. } => Err(LINUX_EISDIR),
        LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. }
        | LinuxFileSource::Socket { .. } => Err(LINUX_ESPIPE),
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
    let current = CURRENT.load(Ordering::Relaxed);
    let pipe = {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let task = &mut tasks[current];
        let Some(slot) = task.linux_fds.get_mut(fd as usize) else {
            return LINUX_EBADF;
        };
        let source = slot.as_ref().map(|file| file.source);
        let writable = slot.as_ref().is_some_and(|file| file.writable);
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
                let tgid = task.tgid;
                if tgid < MAX_TASKS {
                    // SAFETY: Single-CPU syscall serialization guards the table.
                    unsafe { (*INOTIFY.0.get())[tgid] = None };
                }
            }
            Some(LinuxFileSource::Overlay(index)) => {
                let store = unsafe { &*OVERLAYS.0.get() };
                if let Some(entry) = store.get(index).and_then(Option::as_ref) {
                    let mut path = [0u8; OVERLAY_PATH];
                    path.copy_from_slice(&entry.path[..OVERLAY_PATH]);
                    let path_length = entry.path_len;
                    let mask = if writable {
                        IN_CLOSE_WRITE
                    } else {
                        IN_CLOSE_NOWRITE
                    };
                    if let Ok(path) = core::str::from_utf8(&path[..path_length]) {
                        linux_inotify_emit(path, mask, false);
                    }
                }
            }
            Some(source @ (LinuxFileSource::MemFd { .. } | LinuxFileSource::Socket { .. })) => {
                release_shared_object(source);
            }
            Some(LinuxFileSource::Display) => crate::display::close(),
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

/// Adds a reference to a reference-counted object behind a descriptor
/// (memfd or socket). Other kinds are not counted.
fn retain_shared_object(source: LinuxFileSource) {
    match source {
        LinuxFileSource::MemFd { index } => {
            let _ = crate::memfd::retain(index);
        }
        LinuxFileSource::Socket { index } => crate::socket::retain(index),
        _ => {}
    }
}

/// Drops the reference [`retain_shared_object`] took.
fn release_shared_object(source: LinuxFileSource) {
    match source {
        LinuxFileSource::MemFd { index } => {
            let _ = crate::memfd::release(index);
        }
        LinuxFileSource::Socket { index } => crate::socket::release(index),
        _ => {}
    }
}

/// Adds an object reference for every copied memfd or socket descriptor.
///
/// Used by `fork` after copying the parent's descriptor table.
fn retain_shared_fds(fds: &[Option<LinuxFile>]) {
    for file in fds.iter().flatten() {
        retain_shared_object(file.source);
    }
}

/// Drops every memfd and socket reference held by a retiring descriptor
/// table and clears those descriptors, so a second call for another thread
/// of the same process cannot release them twice.
fn release_shared_fds(fds: &mut [Option<LinuxFile>]) {
    for slot in fds.iter_mut() {
        if let Some(file) = slot
            && matches!(
                file.source,
                LinuxFileSource::MemFd { .. } | LinuxFileSource::Socket { .. }
            )
        {
            release_shared_object(file.source);
            *slot = None;
        }
    }
}

/// Releases endpoint-owned resources before a task's descriptors are cleared.
///
/// Called on exit, signal exit, and fault. It is idempotent: a stale memfd
/// index is ignored and `display::close` returns immediately when the endpoint
/// was never opened.
fn release_linux_task_resources(fds: &mut [Option<LinuxFile>]) {
    release_shared_fds(fds);
    if fds
        .iter()
        .flatten()
        .any(|file| matches!(file.source, LinuxFileSource::Display))
    {
        crate::display::close();
    }
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
        LinuxFileSource::Overlay(index) => unsafe { &*OVERLAYS.0.get() }
            .get(index)
            .and_then(Option::as_ref)
            .map_or(0, |entry| entry.len),
        LinuxFileSource::Portal { path, len, .. } => {
            core::str::from_utf8(&path[..usize::from(len)])
                .ok()
                .and_then(|path| crate::fs::portal_size(path).ok())
                .unwrap_or(0)
        }
        LinuxFileSource::PortalTemp {
            index, generation, ..
        } => crate::portal::temp_len(index, generation).unwrap_or(0),
        LinuxFileSource::MemFd { index } => crate::memfd::len(index).unwrap_or(0),
        LinuxFileSource::Display | LinuxFileSource::Console => 0,
        LinuxFileSource::Directory { .. }
        | LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. } => 0,
        LinuxFileSource::Socket { .. } => return LINUX_ESPIPE,
        LinuxFileSource::PseudoDir(_) => 0,
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
    if pseudo_file(path).is_some() || pseudo_dir(path).is_some() {
        return true;
    }
    if is_overlay_path(path) {
        let tasks = unsafe { &*TASKS.0.get() };
        let app = tasks[CURRENT.load(Ordering::Relaxed)].overlay_app;
        if overlay_index(app, path).is_some() {
            return true;
        }
    }
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
    if shared && shared_vma_usage(task.overlay_app) >= MAX_DOMAIN_SHARED_VMAS {
        return Err(VmaError::OutOfSpace);
    }
    // A table insert may allocate tree nodes, and an allocation failure
    // there would panic the kernel; refuse with ENOMEM while there is room.
    if !crate::memory::heap_has_headroom() {
        return Err(VmaError::OutOfSpace);
    }
    // The tables belong to the process, so this one update is visible to
    // every thread. Each step either succeeds or leaves its table unchanged;
    // a later failure undoes the earlier steps.
    let mut vmas = task.vmas;
    let mut anonymous_vmas = task.anonymous_vmas;
    let mut shared_vmas = task.shared_vmas;
    let start = if hint != 0 && hint.is_multiple_of(PAGE_SIZE) {
        match vmas.insert_at(hint, length, prot) {
            Ok(()) => hint,
            Err(VmaError::Overlap) if !fixed && !no_replace => vmas.insert(length, prot)?,
            Err(error) => return Err(error),
        }
    } else if fixed {
        return Err(VmaError::InvalidRange);
    } else {
        vmas.insert(length, prot)?
    };
    if anonymous && let Err(error) = anonymous_vmas.insert_at(start, length, prot) {
        let _ = vmas.remove(start, length);
        return Err(error);
    }
    if shared && let Err(error) = shared_vmas.insert_at(start, length, prot) {
        if anonymous {
            let _ = anonymous_vmas.remove(start, length);
        }
        let _ = vmas.remove(start, length);
        return Err(error);
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
    let task = &mut tasks[current];
    let removed = task.vmas.remove(address, length)?;
    let _ = task.anonymous_vmas.remove(address, length);
    let _ = task.shared_vmas.remove(address, length);
    Ok(removed)
}

pub fn linux_vma_insert_shared(
    length: u64,
    prot: u32,
    hint: u64,
    fixed: bool,
    no_replace: bool,
    anonymous: bool,
) -> Result<u64, VmaError> {
    linux_vma_insert(length, prot, hint, fixed, no_replace, anonymous, true)
}

/// Returns the current Linux task's bounded application domain id.
#[must_use]
pub fn linux_shared_domain() -> u8 {
    let tasks = unsafe { &*TASKS.0.get() };
    tasks[CURRENT.load(Ordering::Relaxed)].overlay_app
}

/// Counts shared VMA entries in one domain, once per address space.
///
/// Threads in one `CLONE_VM` group share a PML4 and therefore one table in
/// practice; counting it once avoids charging a thread group more than a
/// process. A `fork` child has a new PML4 and is charged separately, matching
/// its independent VMA table.
fn shared_vma_usage(domain: u8) -> usize {
    let tasks = unsafe { &*TASKS.0.get() };
    let mut seen = [0u64; MAX_TASKS];
    let mut seen_count = 0usize;
    let mut total = 0usize;
    for task in tasks.iter() {
        if task.state == TaskState::Empty
            || task.state == TaskState::Done
            || task.abi != TaskAbi::Linux
            || task.overlay_app != domain
        {
            continue;
        }
        if seen.iter().take(seen_count).any(|pml4| *pml4 == task.pml4) {
            continue;
        }
        if let Some(slot) = seen.get_mut(seen_count) {
            *slot = task.pml4;
            seen_count += 1;
        }
        total = total.saturating_add(task.shared_vmas.len());
    }
    total
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
    let task = &mut tasks[current];
    task.vmas.protect(address, length, prot)?;
    if task.anonymous_vmas.covers(address, length) {
        let _ = task.anonymous_vmas.protect(address, length, prot);
    }
    if task.shared_vmas.covers(address, length) {
        let _ = task.shared_vmas.protect(address, length, prot);
    }
    Ok(())
}

/// Returns metadata for a Linux path: `(size, inode, mode)`.
///
/// The root is a flat imported tree, so regular files have a stable inode
/// derived from their index and a synthetic mode.
pub fn linux_stat_path(path: &str) -> Option<(u64, u64, u32)> {
    if path.starts_with(aether_services::portal_store::PORTAL_PREFIX) {
        // The portal namespace is a grant-backed view, not an imported file.
        // Applications probe the path before opening it, so stat must resolve
        // it through the same exact-file grant as openat.
        use aether_abi::user::FileRights;
        let root = {
            let tasks = unsafe { &*TASKS.0.get() };
            tasks[CURRENT.load(Ordering::Relaxed)].root
        };
        let grant = crate::portal::check_namespace(root, path, FileRights::READ).ok()?;
        let size = crate::fs::portal_size(grant.path()).ok()?;
        return Some((size as u64, 3, 0o100644));
    }
    if let Some(kind) = pseudo_file(path) {
        return Some(pseudo_stat(kind));
    }
    if pseudo_dir(path).is_some() {
        return Some((0, 2, 0o040755));
    }
    if is_overlay_path(path) {
        let tasks = unsafe { &*TASKS.0.get() };
        let app = tasks[CURRENT.load(Ordering::Relaxed)].overlay_app;
        let index = overlay_index(app, path)?;
        // SAFETY: overlay access is serialized like the task table.
        let store = unsafe { &*OVERLAYS.0.get() };
        let entry = store.get(index).and_then(Option::as_ref)?;
        return Some((entry.len as u64, index as u64 + 1, 0o100644));
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
            let store = unsafe { &*OVERLAYS.0.get() };
            let entry = store
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(LINUX_EBADF)?;
            Ok((entry.len as u64, index as u64 + 1, 0o100644))
        }
        LinuxFileSource::Portal { path, len, .. } => {
            let path = core::str::from_utf8(&path[..usize::from(len)]).map_err(|_| LINUX_ENOENT)?;
            let size = crate::fs::portal_size(path).map_err(portal_fs_error)?;
            Ok((size as u64, 0, 0o100644))
        }
        LinuxFileSource::PortalTemp {
            index, generation, ..
        } => {
            let size = crate::portal::temp_len(index, generation).map_err(portal_error_code)?;
            Ok((size as u64, 0, 0o100644))
        }
        LinuxFileSource::MemFd { index } => {
            let length = crate::memfd::len(index).ok_or(LINUX_EBADF)?;
            Ok((length as u64, crate::memfd::file_key(index), 0o100666))
        }
        LinuxFileSource::Display => Ok((0, 7, 0o020600)),
        LinuxFileSource::Console => Ok((0, 8, 0o020620)),
        LinuxFileSource::Directory { .. } | LinuxFileSource::PseudoDir(_) => Ok((0, 2, 0o040755)),
        LinuxFileSource::Pseudo(kind) => Ok(pseudo_stat(kind)),
        LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. } => Ok((0, 0, 0o010600)),
        LinuxFileSource::Socket { index } => Ok((0, crate::socket::inode(index), 0o140777)),
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
            retain_shared_object(source.source);
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
    // Take the new reference first so `dup2` onto a descriptor that shares
    // the same object cannot drop it to zero in between.
    retain_shared_object(source.source);
    if let Some(previous) = slot.replace(source) {
        release_shared_object(previous.source);
    }
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
        LinuxFileSource::Overlay(index) => unsafe { &*OVERLAYS.0.get() }
            .get(index)
            .and_then(Option::as_ref)
            .map(|entry| entry.len as u64)
            .ok_or(LINUX_EBADF),
        LinuxFileSource::Portal { path, len, .. } => {
            let path = core::str::from_utf8(&path[..usize::from(len)]).map_err(|_| LINUX_ENOENT)?;
            crate::fs::portal_size(path)
                .map(|size| size as u64)
                .map_err(portal_fs_error)
        }
        LinuxFileSource::PortalTemp {
            index, generation, ..
        } => crate::portal::temp_len(index, generation)
            .map(|size| size as u64)
            .map_err(portal_error_code),
        LinuxFileSource::MemFd { index } => crate::memfd::len(index)
            .map(|length| length as u64)
            .ok_or(LINUX_EBADF),
        LinuxFileSource::Display | LinuxFileSource::Console => Ok(0),
        LinuxFileSource::Directory { .. }
        | LinuxFileSource::Pipe { .. }
        | LinuxFileSource::EventFd { .. }
        | LinuxFileSource::Epoll { .. }
        | LinuxFileSource::TimerFd { .. }
        | LinuxFileSource::Socket { .. } => Ok(0),
        LinuxFileSource::PseudoDir(_) => Ok(0),
        LinuxFileSource::Pseudo(PseudoFile::ProcExe) => Ok(LINUX_ELF.len() as u64),
        LinuxFileSource::Pseudo(_) => Ok(0),
        LinuxFileSource::Inotify => Ok(0),
    }
}

/// Returns a stable identity and length for a shared file-backed mapping.
///
/// The high 32 bits distinguish the read-only root from the writable overlay,
/// and the low bits are the inode index. Pipes, events, and pseudo-files have
/// no page-cache identity and are rejected with `EINVAL`.
pub fn linux_file_identity(fd: u64) -> Result<(u64, u64), i64> {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[CURRENT.load(Ordering::Relaxed)];
    let Some(slot) = task.linux_fds.get(fd as usize).and_then(Option::as_ref) else {
        return Err(LINUX_EBADF);
    };
    match slot.source {
        LinuxFileSource::Root(index) => {
            let file = crate::linux::file(index).ok_or(LINUX_EBADF)?;
            Ok((index as u64, file.data.len() as u64))
        }
        LinuxFileSource::Overlay(index) => {
            let store = unsafe { &*OVERLAYS.0.get() };
            let entry = store
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(LINUX_EBADF)?;
            Ok(((1u64 << 32) | index as u64, entry.len as u64))
        }
        LinuxFileSource::MemFd { index } => Ok((
            crate::memfd::file_key(index),
            crate::memfd::len(index).ok_or(LINUX_EBADF)? as u64,
        )),
        _ => Err(LINUX_EINVAL),
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

/// Releases the Linux process's mappings and heap pages, then empties its
/// mapping tables so the memory behind them is reclaimed with the process.
fn release_linux_mappings(index: usize) {
    let (pml4, mut vmas, mut anonymous_vmas, mut shared_vmas, brk) = {
        let tasks = unsafe { &*TASKS.0.get() };
        let task = &tasks[index];
        (
            task.pml4,
            task.vmas,
            task.anonymous_vmas,
            task.shared_vmas,
            task.brk,
        )
    };
    if let Some(pml4) = PhysFrame::from_address(pml4) {
        unmap_linux_process(pml4, &vmas, brk);
    }
    vmas.reset();
    anonymous_vmas.reset();
    shared_vmas.reset();
}

fn unmap_linux_process(pml4: PhysFrame, vmas: &VmaTable, brk: u64) {
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
        stop_sibling_threads(current);
        robust_futex_cleanup(current);
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
        release_linux_task_resources(&mut tasks[current].linux_fds);
        clear_file_state(tasks[current].tgid);
        serial_line!("linux: process exit pid={current} status={status}");
        linux_release_task_pipes();
        let pml4 = tasks[current].pml4;
        wake_waiting_parent(current);
        queue_child_signal(current);
        retire_address_space(pml4);
        return schedule_next(current);
    }
    if kind == TaskKind::Thread {
        robust_futex_cleanup(current);
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].futex_address = 0;
        tasks[current].futex_deadline = 0;
        tasks[current].state = TaskState::Done;
        // A thread shares the descriptor table of its process, so the table
        // and the descriptors in it stay open for the other threads.
        serial_line!("linux: thread exit tid={current} status={status}");
        linux_release_task_pipes();
        return schedule_next(current);
    }
    if (current != LINUX_TASK && current != LINUX_DYN_TASK && !is_app_task(current))
        || kind != TaskKind::Linux
    {
        serial_line!("AETHER_BOOT_FAILED reason=linux exit from wrong task");
        halt()
    }

    robust_futex_cleanup(current);
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
    release_linux_task_resources(&mut tasks[current].linux_fds);
    clear_file_state(tgid);
    tasks[current].vmas.reset();
    tasks[current].anonymous_vmas.reset();
    let result = if status == 0 {
        UserTestResult::Passed
    } else {
        UserTestResult::Failed
    };
    let app_slot = tasks[current].app_slot;
    if current == LINUX_DYN_TASK {
        LINUX_DYN_EXITS.fetch_add(1, Ordering::Release);
        LINUX_DYN_RESULT.store(result as usize, Ordering::Release);
    } else if is_app_task(current) {
        if current == APPIMAGE_TASK {
            APPIMAGE_RESULT.store(result as usize, Ordering::Release);
            // The slot is released just below, so report the identity now.
            if let Some((id, version, path)) = crate::linux::app_identity(app_slot) {
                serial_line!("AETHER_LINUX_APPIMAGE_OK id={id} version={version} path={path}");
            }
        }
        if app_slot < crate::linux::MAX_APPS {
            crate::display::close();
            crate::linux::release_app(app_slot);
            tasks[current].app_slot = crate::linux::MAX_APPS;
        }
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
    // A PROT_NONE range is a guard: any access is a fault, never a page-in.
    if prot & 7 == 0 {
        return false;
    }
    if error & WRITE != 0 && prot & aether_memory::vma::PROT_WRITE == 0
        || error & INSTRUCTION != 0 && prot & aether_memory::vma::PROT_EXEC == 0
    {
        return false;
    }
    let Some(pml4) = PhysFrame::from_address(task.pml4) else {
        return false;
    };
    // Never map over an existing leaf (a guard page, or a page another path
    // already installed): that would orphan its frame.
    if paging::user_leaf_present(pml4, page) {
        return false;
    }
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
    let (linux, thread, process, service, kind) = {
        let tasks = unsafe { &*TASKS.0.get() };
        (
            tasks[current].kind == TaskKind::Linux,
            tasks[current].kind == TaskKind::Thread,
            tasks[current].kind == TaskKind::Process,
            tasks[current].kind == TaskKind::Service,
            tasks[current].kind,
        )
    };
    if kind == TaskKind::Terminal {
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].saved_rsp = frame as u64;
        // #AC is SIGBUS (7); every other fault is reported as SIGSEGV (11).
        let signal = if vector == 17 { 7 } else { 11 };
        serial_line!("terminal: program fault pid={current} vector={vector} address={address:#x}");
        retire_terminal_task(current, 128 + signal, true);
        return schedule_next(current);
    }
    if kind != TaskKind::User && !linux && !thread && !process && !service {
        serial_line!("AETHER_BOOT_FAILED reason=user-fault on non-user task");
        halt()
    }
    if service {
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Faulted;
        tasks[current].saved_rsp = frame as u64;
        let _closed = crate::fs::close_task_files(&mut tasks[current].caps);
        LINUXD_FAULTS.fetch_add(1, Ordering::Release);
        LINUXD_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        serial_line!("linuxd: service fault pid={current} vector={vector}");
        return schedule_next(current);
    }
    if process {
        stop_sibling_threads(current);
        robust_futex_cleanup(current);
        release_linux_mappings(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Done;
        tasks[current].exit_status = 128 + vector as i64;
        release_linux_task_resources(&mut tasks[current].linux_fds);
        clear_file_state(tasks[current].tgid);
        serial_line!("linux: process fault pid={current} vector={vector}");
        linux_release_task_pipes();
        let pml4 = tasks[current].pml4;
        wake_waiting_parent(current);
        queue_child_signal(current);
        retire_address_space(pml4);
        return schedule_next(current);
    }
    if thread {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let tgid = tasks[current].tgid;
        if tgid < MAX_TASKS && tasks[tgid].kind == TaskKind::Process {
            // An unhandled fault kills the whole process, as a fatal signal
            // does on Linux; the siblings must not keep running against the
            // descriptors and mappings the group shares.
            serial_line!("linux: thread fault tid={current} vector={vector}");
            let signum = if vector == 17 { 7 } else { 11 };
            let pml4 = tasks[current].pml4;
            terminate_thread_group(current, signum);
            retire_address_space(pml4);
            return schedule_next(current);
        }
        robust_futex_cleanup(current);
        clear_child_tid(current);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[current].state = TaskState::Faulted;
        tasks[current].saved_rsp = frame as u64;
        // The descriptor table belongs to the thread group, which keeps
        // running; only the faulting thread retires (as on thread exit).
        serial_line!("linux: thread fault tid={current} vector={vector}");
        linux_release_task_pipes();
        return schedule_next(current);
    }

    if linux {
        // #PF and #GP are SIGSEGV; only #AC (alignment check) is SIGBUS.
        let signum = if vector == 17 { 7 } else { 11 };
        let code = if vector == 14 || vector == 17 {
            1
        } else {
            0x80
        };
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
        release_linux_task_resources(&mut tasks[current].linux_fds);
        clear_file_state(tasks[current].tgid);
        tasks[current].vmas.reset();
        tasks[current].anonymous_vmas.reset();
        if current == LINUX_DYN_TASK {
            LINUX_DYN_FAULTS.fetch_add(1, Ordering::Release);
            LINUX_DYN_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        } else if is_app_task(current) {
            if current == APPIMAGE_TASK {
                APPIMAGE_FAULTS.fetch_add(1, Ordering::Release);
                APPIMAGE_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
            }
            let app_slot = tasks[current].app_slot;
            if app_slot < crate::linux::MAX_APPS {
                crate::display::close();
                crate::linux::release_app(app_slot);
                tasks[current].app_slot = crate::linux::MAX_APPS;
            }
        } else {
            LINUX_FAULTS.fetch_add(1, Ordering::Release);
            LINUX_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        }
    } else {
        USER_FAULTS.fetch_add(1, Ordering::Release);
        USER_RESULT.store(UserTestResult::Faulted as usize, Ordering::Release);
        crate::apps::faulted(current, vector);
    }
    // Destroy owned endpoints and drop every alias the faulting task held,
    // including for Linux tasks, before the task table entry is reused.
    let (_endpoint_handles, _endpoints) =
        crate::ipc::close_task_endpoints(&mut tasks[current].caps, current as u16);
    linux_release_task_pipes();
    serial_line!("sched: user task {current} faulted vector={vector}");

    schedule_next(current)
}

/// Selects the save format for the per-task vector state and seeds every slot
/// with the architectural default.
///
/// A zeroed FXSAVE/XSAVE area is not a valid processor state: it sets MXCSR to
/// zero, which unmasks every SIMD floating-point exception, and clears the x87
/// control word. Restoring that into a task makes ordinary SSE arithmetic raise
/// `#XM` (vector 19). Capture a real default state with `fninit`, a masked
/// MXCSR, and `xsave64`/`fxsave64` instead.
pub fn init_fpu(xsave: bool) {
    FPU_XSAVE.store(xsave, Ordering::Relaxed);
    // SAFETY: Boot is single-threaded, interrupts are disabled, and no task
    // has executed vector code yet. The static areas are 64-byte aligned and
    // large enough for both save formats.
    unsafe {
        asm!("fninit", options(nostack, preserves_flags));
        let mxcsr: u32 = 0x1f80;
        asm!(
            "ldmxcsr [{mxcsr}]",
            mxcsr = in(reg) &mxcsr,
            options(nostack, preserves_flags, readonly)
        );
        for task in 0..=MAX_TASKS {
            let ptr = if task == MAX_TASKS {
                FPU_DEFAULT.0.get().cast::<u8>()
            } else {
                FPU_STATES.0.get().cast::<FpuState>().add(task).cast::<u8>()
            };
            if xsave {
                asm!(
                    "xsave64 [{ptr}]",
                    ptr = in(reg) ptr,
                    in("eax") 0b111u32,
                    in("edx") 0u32,
                    options(nostack, preserves_flags)
                );
            } else {
                asm!(
                    "fxsave64 [{ptr}]",
                    ptr = in(reg) ptr,
                    options(nostack, preserves_flags)
                );
            }
        }
    }
}

/// Resets a task slot's saved vector state to the boot default.
///
/// # Safety
///
/// Runs at CPL0 with interrupts masked, for a slot that is not running.
unsafe fn fpu_reset(task: usize) {
    // SAFETY: Both areas are static; the caller guarantees exclusivity.
    unsafe { (*FPU_STATES.0.get())[task] = *FPU_DEFAULT.0.get() };
}

/// Saves the live vector registers into a task's state area.
///
/// # Safety
///
/// Runs at CPL0 with interrupts masked, as part of a context switch.
unsafe fn fpu_save(task: usize) {
    let ptr = unsafe { FPU_STATES.0.get().cast::<FpuState>().add(task) }.cast::<u8>();
    // SAFETY: The area is 64-byte aligned and large enough for either format.
    // The caller serializes against user execution.
    unsafe {
        if FPU_XSAVE.load(Ordering::Relaxed) {
            asm!(
                "xsave64 [{ptr}]",
                ptr = in(reg) ptr,
                in("eax") 0b111u32,
                in("edx") 0u32,
                options(nostack, preserves_flags)
            );
        } else {
            asm!(
                "fxsave64 [{ptr}]",
                ptr = in(reg) ptr,
                options(nostack, preserves_flags)
            );
        }
    }
}

/// Restores a task's vector registers.
///
/// # Safety
///
/// Runs at CPL0 with interrupts masked, immediately before returning to the
/// task whose area was saved by [`fpu_save`].
unsafe fn fpu_restore(task: usize) {
    let ptr = unsafe { FPU_STATES.0.get().cast::<FpuState>().add(task) }.cast::<u8>();
    // SAFETY: Same area and serialization invariant as `fpu_save`.
    unsafe {
        if FPU_XSAVE.load(Ordering::Relaxed) {
            asm!(
                "xrstor64 [{ptr}]",
                ptr = in(reg) ptr,
                in("eax") 0b111u32,
                in("edx") 0u32,
                options(nostack, preserves_flags)
            );
        } else {
            asm!(
                "fxrstor64 [{ptr}]",
                ptr = in(reg) ptr,
                options(nostack, preserves_flags)
            );
        }
    }
}

/// Address spaces whose last task has retired, waiting for a CR3 switch.
struct PendingSpaces(UnsafeCell<[u64; MAX_PENDING_SPACES]>);

// SAFETY: Accessed only from scheduler paths at CPL0 with interrupts masked.
unsafe impl Sync for PendingSpaces {}

const MAX_PENDING_SPACES: usize = 16;
/// Address spaces and frames returned by retired Linux processes.
static RELEASED_SPACES: AtomicUsize = AtomicUsize::new(0);
static RELEASED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static PENDING_SPACES: PendingSpaces = PendingSpaces(UnsafeCell::new([0; MAX_PENDING_SPACES]));

/// Queues a Linux address space for teardown once no live task uses it.
///
/// The space may still be active in CR3 (the exiting task is running on it),
/// so the frames are released by [`schedule_next`] after the switch. Retired
/// tasks that still name the space have it cleared so nothing can reach the
/// recycled frames through them.
fn retire_address_space(pml4: u64) {
    if pml4 == 0 {
        return;
    }
    let tasks = unsafe { &mut *TASKS.0.get() };
    if tasks.iter().any(|task| {
        task.pml4 == pml4
            && !matches!(
                task.state,
                TaskState::Empty | TaskState::Done | TaskState::Faulted
            )
    }) {
        return;
    }
    for task in tasks.iter_mut().filter(|task| task.pml4 == pml4) {
        task.pml4 = 0;
    }
    // SAFETY: Scheduler paths run with interrupts masked on the single CPU.
    let pending = unsafe { &mut *PENDING_SPACES.0.get() };
    if pending.contains(&pml4) {
        return;
    }
    if let Some(slot) = pending.iter_mut().find(|slot| **slot == 0) {
        *slot = pml4;
    } else {
        serial_line!("linux: address-space teardown queue full; leaking {pml4:#x}");
    }
}

/// Frees queued address spaces that are no longer active in CR3.
fn drain_retired_address_spaces() {
    let active = paging::current_pml4().map_or(0, |frame| frame.start_address());
    // SAFETY: Scheduler paths run with interrupts masked on the single CPU.
    let pending = unsafe { &mut *PENDING_SPACES.0.get() };
    for slot in pending.iter_mut() {
        if *slot == 0 || *slot == active {
            continue;
        }
        if let Some(pml4) = PhysFrame::from_address(*slot) {
            let _ = crate::memory::with_frame_allocator(|allocator| {
                let before = allocator.stats().free_frames;
                // SAFETY: `retire_address_space` admitted the space only after
                // its last live task retired, and it is not the active CR3.
                unsafe { paging::destroy_user_address_space(allocator, pml4) };
                let freed = allocator.stats().free_frames.saturating_sub(before);
                RELEASED_SPACES.fetch_add(1, Ordering::Relaxed);
                RELEASED_FRAMES.fetch_add(freed as usize, Ordering::Relaxed);
            });
        }
        *slot = 0;
    }
}

/// Retires every other thread of the caller's thread group.
///
/// Called before a process tears down its mappings and descriptors, so no
/// sibling keeps running against released state (Linux `exit_group`).
fn stop_sibling_threads(current: usize) {
    let (tgid, pml4) = {
        let tasks = unsafe { &*TASKS.0.get() };
        (tasks[current].tgid, tasks[current].pml4)
    };
    for slot in 0..MAX_TASKS {
        let sibling = {
            let tasks = unsafe { &*TASKS.0.get() };
            let task = &tasks[slot];
            slot != current
                && task.kind == TaskKind::Thread
                && task.tgid == tgid
                && task.pml4 == pml4
                && !matches!(
                    task.state,
                    TaskState::Empty | TaskState::Done | TaskState::Faulted
                )
        };
        if !sibling {
            continue;
        }
        robust_futex_cleanup(slot);
        let tasks = unsafe { &mut *TASKS.0.get() };
        tasks[slot].futex_address = 0;
        tasks[slot].futex_deadline = 0;
        tasks[slot].state = TaskState::Done;
    }
}

fn schedule_next(current: usize) -> *mut InterruptFrame {
    // SAFETY: Context switches run at CPL0 with interrupts masked.
    unsafe { fpu_save(current) };
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
    drain_retired_address_spaces();
    // SAFETY: The selected task is about to resume on this CPU.
    unsafe { fpu_restore(next) };
    tasks[next].saved_rsp as *mut InterruptFrame
}

/// Yields the current task and returns the frame that should be resumed.
///
/// The syscall gate calls this with interrupts masked. If another task is
/// ready, it runs immediately; otherwise the idle task runs until the next
/// timer tick selects this task again.
pub fn yield_current(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    tasks[current].saved_rsp = frame as u64;
    if tasks[current].state == TaskState::Running {
        tasks[current].state = TaskState::Ready;
    }
    schedule_next(current)
}

/// Parks the current task as `Blocked` for `ticks` timer ticks and returns
/// the frame to resume once the wake deadline has elapsed.
///
/// The deadline goes to the same field the kernel uses for its other
/// deadline waits, so the timer path releases the task exactly when the
/// deadline elapses and never before. A zero duration still spans one tick,
/// because a parked task is only released on a live tick. RAX is restored to
/// zero before the first context switch, so the resumed frame reports zero
/// like it already does after `Yield` or a blocking `EndpointReceiveTimeout`.
pub fn sleep_current(frame: *mut InterruptFrame, ticks: u64) -> *mut InterruptFrame {
    // SAFETY: The syscall gate passes the live frame of the interrupted task
    // with interrupts masked, the same invariant `yield_current` relies on.
    unsafe { (*frame).rax = 0 };
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: The scheduler owns the current task's wait fields; this call
    // runs in the single-CPU, interrupt-masked syscall context.
    tasks[current].wait_deadline = sleep_wake_deadline(TICKS.load(Ordering::Relaxed), ticks);
    tasks[current].wait_kind = LinuxWaitKind::None;
    tasks[current].state = TaskState::Blocked;
    yield_current(frame)
}

/// Parks the current task for one tick, rewinding the interrupted system
/// call so it re-executes on wake.
///
/// A blocking endpoint receive uses this for every empty re-check: the timer
/// path releases the task exactly as it releases `Sleep`, and the re-run
/// receiver re-checks the queue against the absolute `deadline`, which it
/// keeps in the restart fields so it survives the parks. A task with no
/// deadline parks until a message arrives or the task is retired.
/// `linux_take_restart` gives the re-executed call the deadline back.
pub fn receive_park(
    frame: *mut InterruptFrame,
    syscall: u64,
    deadline: Option<u64>,
) -> *mut InterruptFrame {
    let tasks = unsafe { &mut *TASKS.0.get() };
    let current = CURRENT.load(Ordering::Relaxed);
    // SAFETY: The scheduler owns the current task's wait and restart fields;
    // this call runs in the single-CPU, interrupt-masked syscall context.
    let task = &mut tasks[current];
    task.wait_kind = LinuxWaitKind::None;
    task.wait_deadline = sleep_wake_deadline(TICKS.load(Ordering::Relaxed), 1);
    task.restart_syscall = syscall;
    task.restart_deadline = deadline.unwrap_or(0);
    task.restart_infinite = deadline.is_none();
    task.state = TaskState::Blocked;
    // SAFETY: Native calls always enter through the two-byte `int 0x80`, so
    // rewinding two bytes re-executes the interrupted receive on resume; the
    // same invariant `linux_park` relies on for the Linux `syscall` gate.
    unsafe { (*frame).rip = (*frame).rip.wrapping_sub(2) };
    task.restart_rip = unsafe { (*frame).rip };
    yield_current(frame)
}

/// Returns true when `frame` is about to re-execute the parked wait of task
/// `index`.
fn at_rewound_syscall(index: usize, frame: *mut InterruptFrame) -> bool {
    let tasks = unsafe { &*TASKS.0.get() };
    let task = &tasks[index];
    // SAFETY: the timer stub passes the live frame of the interrupted task.
    task.restart_syscall != u64::MAX
        && !frame.is_null()
        && unsafe { (*frame).rip } == task.restart_rip
}

/// Called from the timer interrupt before returning to a task.
pub fn on_tick(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    let now = TICKS.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    // Run the network stack before any task state is borrowed; it may wake
    // socket waiters.
    crate::net::poll();
    {
        let tasks = unsafe { &mut *TASKS.0.get() };
        let running = CURRENT.load(Ordering::Relaxed);
        tasks[running].cpu_ticks = tasks[running].cpu_ticks.wrapping_add(1);
    }
    // Advance the linuxd launch handshake before the interrupted task is
    // saved. The application task is only created once an approval message
    // has been received; no boot path creates it directly.
    crate::linuxd::service_tick();
    let current = CURRENT.load(Ordering::Relaxed);
    // Deliver a pending signal to the interrupted Linux task before it is
    // saved and possibly scheduled out. A task resumed at the rewound
    // `syscall` of a parked wait is left alone: the re-executed syscall
    // reports EINTR and delivers the signal on exit, as `linux_kill` defers
    // it. Delivering here would run the handler first, and the restarted
    // wait would then sleep to its deadline instead of returning EINTR.
    if !at_rewound_syscall(current, frame) {
        let _ = linux_deliver_signal(frame);
    }
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
        if task.state == TaskState::Blocked && deadline_elapsed(task.wait_deadline, now) {
            task.wait_kind = LinuxWaitKind::None;
            task.wait_deadline = 0;
            task.state = TaskState::Ready;
        }
    }

    #[cfg(feature = "linux-wait-dump")]
    if now.is_multiple_of(1000) {
        linux_wait_dump(now);
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
    // SAFETY: The timer path runs at CPL0 with interrupts masked.
    unsafe { fpu_save(current) };
    if tasks[current].state == TaskState::Running {
        tasks[current].state = TaskState::Ready;
    }

    // Prefer the idle task when input is waiting or a presented frame has not
    // been composited yet. Otherwise skip it so a software-rendered client
    // gets the whole CPU instead of sharing round-robin slices with an idle
    // painter. Without the frame check, a guest that renders or polls without
    // blocking can starve presentation and make input feel laggy.
    // Console input and terminal output count too: a terminal program that
    // never blocks (`yes`) must not freeze the screen it writes to, or the
    // console that could stop it.
    let input_pending = crate::input::pointer_pending()
        || crate::input::key_pending()
        || crate::arch::x86_64::serial::input_pending()
        || crate::terminal::paint_hint();
    let frame_pending = crate::desktop::frame_pending();
    let idle_ready = matches!(tasks[0].state, TaskState::Ready | TaskState::Running);
    let mut next = current;
    if current != 0 && (input_pending || frame_pending) && idle_ready {
        next = 0;
    } else {
        for step in 1..=MAX_TASKS {
            let index = (current + step) % MAX_TASKS;
            if index != 0 && tasks[index].state == TaskState::Ready {
                next = index;
                break;
            }
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

    // SAFETY: The selected task is about to resume on this CPU.
    unsafe { fpu_restore(next) };
    tasks[next].saved_rsp as *mut InterruptFrame
}

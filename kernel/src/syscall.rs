// SPDX-License-Identifier: Apache-2.0
//! Ring-3 system call dispatcher.
//!
//! Every pointer and handle is validated here before use. The current task's
//! PML4 is authoritative: a user address is accepted only when it lies in the
//! user window and every page in the range is present with the user bit (and
//! writable for output). The kernel never trusts a sender or process identity
//! supplied by the caller.

use crate::arch::x86_64::idt::InterruptFrame;
use crate::arch::x86_64::serial;
use crate::arch::x86_64::timer;
use crate::linux_policy;
use crate::memory::paging;
use crate::memory::shared;
use crate::scheduler;
use crate::serial_line;
use aether_abi::user::{MAX_SYSCALL_IO, Syscall, USER_LIMIT, USER_STACK_TOP, errno};
use aether_ipc::{IpcError, MAX_MESSAGE_BYTES};
use aether_memory::PAGE_SIZE;
use aether_memory::frame::{FrameAllocator, PhysFrame};
use core::sync::atomic::{AtomicUsize, Ordering};

static IPC_SENDS: AtomicUsize = AtomicUsize::new(0);
static IPC_RECEIVES: AtomicUsize = AtomicUsize::new(0);
static IPC_DENIED: AtomicUsize = AtomicUsize::new(0);
static IPC_BLOCKS: AtomicUsize = AtomicUsize::new(0);
static IPC_TIMEOUTS: AtomicUsize = AtomicUsize::new(0);
static IPC_CLOSES: AtomicUsize = AtomicUsize::new(0);

/// Returns the number of IPC messages user tasks sent.
#[must_use]
pub fn ipc_sends() -> usize {
    IPC_SENDS.load(Ordering::Acquire)
}

/// Returns the number of IPC messages user tasks received.
#[must_use]
pub fn ipc_receives() -> usize {
    IPC_RECEIVES.load(Ordering::Acquire)
}

/// Returns the number of IPC syscall attempts that were denied.
#[must_use]
pub fn ipc_denials() -> usize {
    IPC_DENIED.load(Ordering::Acquire)
}

/// Returns the number of times a blocking receive yielded the CPU.
#[must_use]
pub fn ipc_blocks() -> usize {
    IPC_BLOCKS.load(Ordering::Acquire)
}

/// Returns the number of blocking receives that reached their deadline.
#[must_use]
pub fn ipc_timeouts() -> usize {
    IPC_TIMEOUTS.load(Ordering::Acquire)
}

/// Returns the number of endpoint capabilities closed from user mode.
#[must_use]
pub fn ipc_closes() -> usize {
    IPC_CLOSES.load(Ordering::Acquire)
}

fn ipc_denied(error: IpcError) -> i64 {
    IPC_DENIED.fetch_add(1, Ordering::Release);
    match error {
        IpcError::Full | IpcError::Empty => errno::EAGAIN,
        IpcError::MessageTooLong => errno::EINVAL,
        IpcError::Capability(
            aether_ipc::CapError::InsufficientRights | aether_ipc::CapError::InvalidRights,
        ) => errno::EACCES,
        IpcError::Capability(aether_ipc::CapError::TableFull) => errno::ENOSPC,
        IpcError::Capability(_) => errno::EBADF,
        IpcError::EndpointGone | IpcError::WrongEndpoint => errno::EBADF,
    }
}

/// Returns the system calls a native service task may use.
fn service_syscall_allowed(call: Syscall) -> bool {
    matches!(
        call,
        Syscall::Yield
            | Syscall::Exit
            | Syscall::Write
            | Syscall::GetPid
            | Syscall::Sleep
            | Syscall::Uptime
            | Syscall::EndpointSend
            | Syscall::EndpointReceive
    )
}

/// Dispatches one system call and returns the frame to resume.
///
/// Returning a different frame lets `exit` schedule the next task without
/// returning to a dead user context.
pub fn dispatch(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    if scheduler::current_abi() != scheduler::TaskAbi::Aether {
        // A Linux task must not reach the native ABI through `int 0x80`.
        unsafe { (*frame).rax = errno::ENOSYS as u64 };
        return frame;
    }
    // Read fields individually. The ring-3 syscall gate supplies RSP/SS on the
    // stack, but a ring-0 `int 0x80` would not; copying the whole frame would
    // then read past the initialized part of the stack.
    let cs = unsafe { (*frame).cs };
    if cs & 3 != 3 {
        serial_line!("AETHER_BOOT_FAILED reason=syscall from ring 0");
        crate::arch::x86_64::halt()
    }

    let number = unsafe { (*frame).rax };
    let rdi = unsafe { (*frame).rdi };
    let rsi = unsafe { (*frame).rsi };
    let rdx = unsafe { (*frame).rdx };
    let r10 = unsafe { (*frame).r10 };
    let Some(call) = Syscall::from_raw(number) else {
        // SAFETY: The assembly stub passes a live frame on the current kernel
        // stack and does not move it for the duration of the call.
        unsafe { (*frame).rax = errno::ENOSYS as u64 };
        return frame;
    };

    // Native services have a narrower surface than native applications even
    // though both enter through `int 0x80`. `linuxd` may talk over the two
    // endpoints it was granted and write diagnostics; files, portal access,
    // endpoint creation, and window requests are denied.
    if scheduler::current_is_service() && !service_syscall_allowed(call) {
        // SAFETY: Same live-frame invariant as above.
        unsafe { (*frame).rax = errno::EACCES as u64 };
        return frame;
    }

    let Some(pml4) = scheduler::current_pml4() else {
        serial_line!("AETHER_BOOT_FAILED reason=syscall without pml4");
        crate::arch::x86_64::halt()
    };

    // `exit` does not return to the caller. Handle it before the generic
    // result write so the retired frame is never reused.
    if call == Syscall::Exit {
        return scheduler::user_exit(unsafe { (*frame).rdi });
    }

    // Yield and sleep must actually give another ready task a chance to run.
    // The frame is saved by the scheduler and resumed after the next tick (or
    // immediately if another task is ready), with RAX set to zero.
    if matches!(call, Syscall::Yield | Syscall::Sleep) {
        // SAFETY: Same frame invariant as above.
        unsafe { (*frame).rax = 0 };
        return scheduler::yield_current(frame);
    }

    let result = match call {
        Syscall::Write => write_console(pml4, rdi, rsi, rdx),
        Syscall::GetPid => scheduler::current_task() as i64,
        Syscall::Uptime => scheduler::ticks() as i64,
        Syscall::TaskCount => scheduler::task_count() as i64,
        Syscall::Open => crate::fs::sys_open(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::PortalOpen => {
            crate::fs::sys_portal_open(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL)
        }
        Syscall::PortalRequest => {
            crate::fs::sys_portal_request(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL)
        }
        Syscall::Read => crate::fs::sys_read(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::WriteFile => crate::fs::sys_write(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::Close => crate::fs::sys_close(rdi).unwrap_or(errno::EBADF),
        Syscall::List => crate::fs::sys_list(pml4, rdi, rsi, rdx, r10).unwrap_or(errno::EINVAL),
        Syscall::Stat => crate::fs::sys_stat(pml4, rdi, rsi).unwrap_or(errno::EINVAL),
        Syscall::EndpointCreate => endpoint_create(),
        Syscall::EndpointSend => endpoint_send(pml4, rdi, rsi, rdx, r10),
        Syscall::EndpointReceive => endpoint_receive(pml4, rdi, rsi, rdx, r10, None),
        Syscall::EndpointReceiveTimeout => {
            let fifth = unsafe { (*frame).r8 };
            endpoint_receive(pml4, rdi, rsi, rdx, r10, Some(fifth & u64::from(u32::MAX)))
        }
        Syscall::EndpointClose => endpoint_close(rdi),
        _ => errno::ENOSYS,
    };

    // SAFETY: Same frame invariant as above.
    unsafe { (*frame).rax = result as u64 };
    frame
}

/// Dispatches a Linux x86-64 `syscall` frame.
///
/// This is the compatibility layer's kernel entry, not a Linux kernel. Only
/// the calls implemented here can reach kernel state; everything else returns
/// `ENOSYS`. User pointers are validated with the same page-table walk as the
/// native ABI before any copy.
pub fn linux_dispatch(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    if scheduler::current_abi() != scheduler::TaskAbi::Linux {
        unsafe { (*frame).rax = LINUX_ENOSYS as u64 };
        return frame;
    }
    let number = unsafe { (*frame).rax };
    scheduler::linux_begin_syscall(number);
    #[cfg(feature = "gate4-gui-test")]
    scheduler::linux_set_last_syscall(number);
    let first = unsafe { (*frame).rdi };
    let second = unsafe { (*frame).rsi };
    let third = unsafe { (*frame).rdx };
    let Some(pml4) = scheduler::current_pml4() else {
        serial_line!("AETHER_BOOT_FAILED reason=linux syscall without pml4");
        crate::arch::x86_64::halt()
    };

    // Default deny: classify before executing. Unknown numbers never reach a
    // helper, and network/device groups fail with EACCES instead of falling
    // through to an implementation that may not exist.
    let class = linux_policy::classify(number);
    match class {
        linux_policy::Class::Unknown => {
            linux_policy::record_unknown();
            #[cfg(feature = "gate4-gui-test")]
            if scheduler::linux_is_appimage_task() {
                // The bounded trace names the calls the application needs and
                // the prototype does not implement yet.
                scheduler::linux_trace_enosys(number);
            }
            // SAFETY: The assembly stub passes a live frame on this task's
            // kernel stack for the duration of the dispatcher call.
            unsafe { (*frame).rax = LINUX_ENOSYS as u64 };
            return frame;
        }
        linux_policy::Class::Network | linux_policy::Class::Device => {
            linux_policy::record_denied(class);
            // SAFETY: Same live-frame invariant as above.
            unsafe { (*frame).rax = LINUX_EACCES as u64 };
            return frame;
        }
        _ => linux_policy::record(class),
    }

    let result = match number {
        0 => return linux_read(frame, pml4, first, second, third),
        1 => return linux_write(frame, pml4, first, second, third),
        3 => scheduler::linux_close(first),
        8 => scheduler::linux_lseek(first, second as i64, third),
        4 | 6 => linux_stat(pml4, first, second),
        7 => return linux_poll(frame, pml4, first, second, third),
        35 => return linux_nanosleep(frame, pml4, first, second),
        230 => {
            return linux_clock_nanosleep(frame, pml4, first, second, third, unsafe {
                (*frame).r10
            });
        }
        283 => scheduler::linux_timerfd_create(first, second)
            .map_or_else(|error| error, |fd| fd as i64),
        286 => linux_timerfd_settime(pml4, first, second, third, unsafe { (*frame).r10 }),
        287 => linux_timerfd_gettime(pml4, first, second),
        5 => linux_fstat(pml4, first, second),
        77 => scheduler::linux_ftruncate(first, second as i64),
        13 => linux_rt_sigaction(pml4, first, second, third),
        14 => linux_rt_sigprocmask(pml4, first, second, third),
        15 => return linux_sigreturn(frame),
        17 => linux_pread(pml4, first, second, third, unsafe { (*frame).r10 }),
        20 => linux_writev(pml4, first, second, third),
        21 => linux_access(pml4, first, second),
        24 => {
            unsafe { (*frame).rax = 0 };
            return scheduler::yield_current(frame);
        }
        26 => linux_msync(first, second, third),
        25 => linux_mremap(
            pml4,
            first,
            second,
            third,
            unsafe { (*frame).r10 },
            unsafe { (*frame).r8 },
        ),
        // epoll_wait and epoll_pwait share the first four arguments; the
        // signal mask of epoll_pwait is not needed because the prototype
        // serves one signal at a time.
        232 | 281 => {
            return linux_epoll_wait(frame, pml4, number, first, second, third, unsafe {
                (*frame).r10
            });
        }
        233 => linux_epoll_ctl(pml4, first, second, third, unsafe { (*frame).r10 }),
        28 => 0,
        32 => scheduler::linux_dup(first).map_or_else(|error| error, |fd| fd as i64),
        33 => scheduler::linux_dup2(first, second).map_or_else(|error| error, |fd| fd as i64),
        39 => scheduler::linux_getpid() as i64,
        110 => scheduler::linux_getppid(),
        109 => linux_setpgid(first, second),
        112 => scheduler::linux_setsid(),
        121 => linux_getpgid(first),
        124 => linux_getsid(first),
        57 => scheduler::linux_fork(frame).map_or_else(|error| error, |pid| pid as i64),
        59 => return linux_execve(frame, pml4, first, second, third),
        61 => return linux_wait4(frame, pml4, first, second, third),
        56 => linux_clone(
            frame,
            pml4,
            first,
            second,
            third,
            unsafe { (*frame).r10 },
            unsafe { (*frame).r8 },
        ),
        435 => linux_clone3(frame, pml4, first, second),
        62 => linux_kill(pml4, first, second),
        234 => linux_tgkill(pml4, first, second, third),
        72 => linux_fcntl(first, second, third),
        79 => linux_getcwd(pml4, first, second),
        82 => linux_rename(pml4, first, second),
        87 => linux_unlink(pml4, first),
        89 => linux_readlink(pml4, first, second, third, 0),
        204 => linux_sched_getaffinity(pml4, first, second, third),
        217 => linux_getdents(pml4, first, second, third),
        262 => linux_newfstatat(pml4, first, second, third),
        290 => scheduler::linux_eventfd(first).map_or_else(|error| error, |fd| fd as i64),
        291 => scheduler::linux_epoll_create().map_or_else(|error| error, |fd| fd as i64),
        293 => linux_pipe2(pml4, first, second),
        267 => linux_readlink(pml4, second, third, unsafe { (*frame).r10 }, first),
        302 => linux_prlimit(pml4, first, second, unsafe { (*frame).r10 }),
        294 => scheduler::linux_inotify_init(first).map_or_else(|error| error, |fd| fd as i64),
        254 => {
            let Some(path) = user_cstr(pml4, second, 256) else {
                return linux_result(frame, LINUX_EFAULT);
            };
            scheduler::linux_inotify_add(first, path, third as u32)
                .map_or_else(|error| error, i64::from)
        }
        255 => scheduler::linux_inotify_remove(first, second).map_or_else(|error| error, |_| 0),
        332 => linux_statx(
            pml4,
            first,
            second,
            third,
            unsafe { (*frame).r10 },
            unsafe { (*frame).r8 },
        ),
        9 => linux_mmap(
            pml4,
            first,
            second,
            third,
            unsafe { (*frame).r10 },
            unsafe { (*frame).r8 },
            unsafe { (*frame).r9 },
        ),
        10 => linux_mprotect(pml4, first, second, third),
        11 => linux_munmap(pml4, first, second),
        12 => scheduler::linux_brk(first) as i64,
        131 => linux_sigaltstack(pml4, first, second),
        186 => scheduler::current_task() as i64,
        218 => scheduler::linux_set_tid_address(first),
        63 => linux_uname(pml4, first),
        102 | 104 | 107 | 108 => 1000,
        158 => linux_arch_prctl(pml4, first, second),
        202 => return linux_futex(frame, pml4),
        273 => linux_set_robust_list(pml4, first, second),
        274 => linux_get_robust_list(pml4, first, second, third),
        334 => linux_rseq(pml4, first, second, third, unsafe { (*frame).r10 }),
        228 => linux_clock_gettime(pml4, second),
        318 => linux_getrandom(pml4, first, second),
        257 => linux_openat(pml4, first, second, third),
        319 => scheduler::linux_memfd_create().map_or_else(|error| error, |fd| fd as i64),
        60 | 231 => return scheduler::linux_exit(first),
        _ => LINUX_ENOSYS,
    };
    #[cfg(feature = "gate4-gui-test")]
    if scheduler::linux_is_appimage_task() && matches!(number, 257 | 82 | 87) {
        // The save transaction is only observable through the paths the
        // application chooses. Trace the bounded set of interesting path
        // calls so a direct write or a different staging suffix is visible.
        gate4_trace_path_call(number, pml4, first, second, third, result);
    }
    #[cfg(feature = "gate4-gui-test")]
    if scheduler::linux_is_appimage_task() && matches!(number, 283 | 286 | 287 | 319) {
        // The Gate 4 trace records the descriptor calls of the GUI so a
        // missing or failing call is visible instead of looking like an
        // application hang. The line count is bounded because the emulated
        // serial port is slow.
        scheduler::linux_trace_fdcall(number, first, result);
    }
    #[cfg(feature = "gate4-gui-test")]
    if scheduler::linux_is_appimage_task() {
        scheduler::linux_trace_syscall_result(number, result);
        if result < 0 {
            scheduler::linux_trace_error(number, result);
        }
    }
    // SAFETY: The assembly stub passes a live frame on this task's kernel
    // stack and keeps it valid until iretq.
    unsafe { (*frame).rax = result as u64 };
    scheduler::linux_deliver_signal(frame);
    if scheduler::linux_signal_pending_exit() {
        return scheduler::linux_signal_exit(frame);
    }
    frame
}

fn linux_result(frame: *mut InterruptFrame, result: i64) -> *mut InterruptFrame {
    unsafe { (*frame).rax = result as u64 };
    scheduler::linux_deliver_signal(frame);
    if scheduler::linux_signal_pending_exit() {
        return scheduler::linux_signal_exit(frame);
    }
    frame
}

const LINUX_EFAULT: i64 = -14;
const LINUX_EOPNOTSUPP: i64 = -95;
const LINUX_EINTR: i64 = -4;
const LINUX_EPIPE: i64 = -32;
const LINUX_EINVAL: i64 = -22;
const LINUX_ENOSYS: i64 = -38;
const LINUX_ESRCH: i64 = -3;
const LINUX_EPERM: i64 = -1;

const LINUX_ENOMEM: i64 = -12;
const LINUX_ENOENT: i64 = -2;
const LINUX_EACCES: i64 = -13;
const LINUX_EAGAIN: i64 = -11;
const LINUX_EBADF: i64 = -9;
const LINUX_EEXIST: i64 = -17;
const LINUX_E2BIG: i64 = -7;
const LINUX_ENAMETOOLONG: i64 = -36;
const LINUX_ERANGE: i64 = -34;

fn linux_openat(pml4: PhysFrame, directory: u64, pointer: u64, flags: u64) -> i64 {
    let Some(path) = user_cstr(pml4, pointer, 256) else {
        return LINUX_EFAULT;
    };
    if path.as_bytes().first() != Some(&b'/') {
        return LINUX_ENOENT;
    }
    #[cfg(feature = "gate4-portal-test")]
    if path == crate::fs::GATE4_REVOKE_PATH {
        return match scheduler::gate4_revoke_portal() {
            Ok(()) => scheduler::linux_open_pseudo("/dev/null", false)
                .map_or_else(|error| error, |fd| fd as i64),
            Err(error) => error,
        };
    }
    if path.starts_with(aether_services::portal_store::PORTAL_PREFIX) {
        return scheduler::linux_portal_open(path, flags)
            .map_or_else(|error| error, |fd| fd as i64);
    }
    // The display endpoint is a synthetic, policy-gated node. It is checked
    // before the generic `/dev` denial so no other device path can slip in.
    if path == "/dev/aether-display" {
        const O_CREAT: u64 = 0x40;
        const O_EXCL: u64 = 0x80;
        const O_TRUNC: u64 = 0x200;
        if flags & (O_CREAT | O_EXCL | O_TRUNC) != 0 {
            return LINUX_EINVAL;
        }
        return scheduler::linux_open_display().map_or_else(|error| error, |fd| fd as i64);
    }
    // Device paths are denied unless they are one of the four synthetic nodes.
    if let Some(rest) = path.strip_prefix("/dev/")
        && !matches!(rest, "null" | "zero" | "random" | "urandom")
    {
        return LINUX_EACCES;
    }
    if path == "/dev/null" && flags & 0x3 != 0 {
        return scheduler::linux_open_pseudo(path, true).map_or_else(|error| error, |fd| fd as i64);
    }
    // Writable paths live in the per-task overlay; the imported root stays
    // read-only.
    const WRITE_FLAGS: u64 = 0x1 | 0x2 | 0x40 | 0x200 | 0x400;
    if flags & WRITE_FLAGS != 0 {
        let create = flags & 0x40 != 0;
        let truncate = flags & 0x200 != 0;
        return scheduler::linux_overlay_open(path, create, truncate, true)
            .map_or_else(|error| error, |fd| fd as i64);
    }
    let _ = directory;
    scheduler::linux_open_read(path).map_or_else(|error| error, |fd| fd as i64)
}

/// Reports the application root, which is always `/` in this prototype.
///
/// Relative paths are rejected by the overlay and imported-root lookups, so
/// there is no per-process working directory to track. The buffer must hold the
/// two-byte `"/\0"` path; a smaller buffer fails with `ERANGE` like Linux.
fn linux_getcwd(pml4: PhysFrame, pointer: u64, size: u64) -> i64 {
    if size < 2 {
        return LINUX_ERANGE;
    }
    write_user_bytes(pml4, pointer, b"/\0").map_or(LINUX_EFAULT, |()| 2)
}

/// Renames one entry inside the current task's private overlay.
fn linux_rename(pml4: PhysFrame, old_pointer: u64, new_pointer: u64) -> i64 {
    let Some(old) = user_cstr(pml4, old_pointer, 256) else {
        return LINUX_EFAULT;
    };
    let Some(new) = user_cstr(pml4, new_pointer, 256) else {
        return LINUX_EFAULT;
    };
    if old.starts_with(aether_services::portal_store::PORTAL_PREFIX)
        || new.starts_with(aether_services::portal_store::PORTAL_PREFIX)
    {
        return scheduler::linux_portal_rename(old, new).map_or_else(|error| error, |()| 0);
    }
    scheduler::linux_overlay_rename(old, new).map_or_else(|error| error, |()| 0)
}

/// Removes one entry from the current task's private overlay.
fn linux_unlink(pml4: PhysFrame, pointer: u64) -> i64 {
    let Some(path) = user_cstr(pml4, pointer, 256) else {
        return LINUX_EFAULT;
    };
    if path.starts_with(aether_services::portal_store::PORTAL_PREFIX) {
        return scheduler::linux_portal_unlink(path).map_or_else(|error| error, |()| 0);
    }
    scheduler::linux_overlay_unlink(path).map_or_else(|error| error, |()| 0)
}

fn linux_read(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    descriptor: u64,
    pointer: u64,
    length: u64,
) -> *mut InterruptFrame {
    let _ = scheduler::linux_take_restart(0);
    // Clamp instead of rejecting: callers such as glibc may ask for a whole
    // buffer, and a short read is the normal Linux response to an I/O cap.
    let length = length.min(MAX_SYSCALL_IO as u64);
    if length == 0 {
        return linux_result(frame, 0);
    }
    // SAFETY: The range is validated once and the syscall path is
    // non-preemptible; the buffer remains mapped while the task is parked.
    let Some(buffer) = (unsafe { user_bytes_mut(pml4, pointer, length) }) else {
        return linux_result(frame, LINUX_EFAULT);
    };
    if scheduler::linux_display_fd(descriptor) {
        return match crate::display::read_events(buffer) {
            Ok(count) => linux_result(frame, count as i64),
            Err(LINUX_EAGAIN) => linux_result(frame, LINUX_EAGAIN),
            Err(error) => linux_result(frame, error),
        };
    }
    match scheduler::linux_read(descriptor, buffer) {
        Ok(count) => linux_result(frame, count as i64),
        Err(LINUX_EAGAIN) => {
            if let Some((index, false)) = scheduler::linux_pipe_end(descriptor) {
                // An empty pipe with no writer is end-of-file, not a wait.
                if scheduler::linux_pipe_writers(index) == 0 {
                    return linux_result(frame, 0);
                }
                if scheduler::linux_signal_pending() {
                    return linux_result(frame, LINUX_EINTR);
                }
                return scheduler::linux_park(
                    frame,
                    0,
                    scheduler::LinuxWait::PipeRead(index),
                    None,
                );
            }
            if scheduler::linux_inotify_blocks(descriptor) {
                return scheduler::linux_inotify_block(frame, pointer, length as usize);
            }
            linux_result(frame, LINUX_EAGAIN)
        }
        Err(error) => linux_result(frame, error),
    }
}

fn linux_pread(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64, offset: u64) -> i64 {
    // A short count is allowed; only one bounce-buffer-sized chunk is copied.
    let length = length.min(MAX_SYSCALL_IO as u64);
    // SAFETY: The range is validated and the syscall path is non-preemptible.
    let Some(buffer) = (unsafe { user_bytes_mut(pml4, pointer, length) }) else {
        return LINUX_EFAULT;
    };
    scheduler::linux_pread(descriptor, buffer, offset)
        .map_or_else(|error| error, |count| count as i64)
}

fn linux_access(pml4: PhysFrame, pointer: u64, mode: u64) -> i64 {
    let Some(path) = user_cstr(pml4, pointer, 256) else {
        return LINUX_EFAULT;
    };
    if !scheduler::linux_access(path) {
        return LINUX_ENOENT;
    }
    // Read-only root: no write or execute access.
    if mode & 0x3 != 0 {
        return LINUX_EACCES;
    }
    0
}

fn user_cstr<'a>(pml4: PhysFrame, pointer: u64, maximum: u64) -> Option<&'a str> {
    // A string may end near a page boundary while the caller's maximum scan
    // length crosses into the next, unmapped page. Validate one page at a time
    // and stop at the terminator instead of requiring the whole maximum range.
    let mut scanned = 0u64;
    while scanned < maximum {
        let page_offset = (pointer + scanned) & 0xfff;
        let chunk = (0x1000 - page_offset).min(maximum - scanned);
        let bytes = user_bytes(pml4, pointer + scanned, chunk, false)?;
        if let Some(position) = bytes.iter().position(|byte| *byte == 0) {
            let length = usize::try_from(scanned + position as u64).ok()?;
            // SAFETY: Every byte up to the terminator was validated page by
            // page above, and the range lies in the caller's user mapping.
            let bytes = unsafe { core::slice::from_raw_parts(pointer as *const u8, length) };
            return core::str::from_utf8(bytes).ok();
        }
        scanned += chunk;
    }
    None
}

/// Traces the path arguments of the Gate 4 GUI save transaction.
///
/// Only paths that name the workbook or a staging file are printed, so the
/// application's normal startup opens do not exhaust the bounded trace.
#[cfg(feature = "gate4-gui-test")]
fn gate4_trace_path_call(
    number: u64,
    pml4: PhysFrame,
    first: u64,
    second: u64,
    third: u64,
    result: i64,
) {
    const TRACE_LIMIT: usize = 48;
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    let interesting =
        |path: &str| path.contains(".xlsx") || path.contains("portal") || path.contains(".tmp");
    let (from, to) = match number {
        257 => (user_cstr(pml4, second, 256), None),
        82 => (user_cstr(pml4, first, 256), user_cstr(pml4, second, 256)),
        87 => (user_cstr(pml4, first, 256), None),
        _ => (None, None),
    };
    if !from.is_some_and(interesting) && !to.is_some_and(interesting) {
        return;
    }
    if TRACED.fetch_add(1, Ordering::Relaxed) >= TRACE_LIMIT {
        return;
    }
    match number {
        257 => serial_line!(
            "linux: openat path={} flags={:#x} result={}",
            from.unwrap_or("?"),
            third,
            result
        ),
        82 => serial_line!(
            "linux: rename from={} to={} result={}",
            from.unwrap_or("?"),
            to.unwrap_or("?"),
            result
        ),
        _ => serial_line!(
            "linux: unlink path={} result={}",
            from.unwrap_or("?"),
            result
        ),
    }
}

const LINUX_MAP_SHARED: u64 = 0x01;
const LINUX_MAP_PRIVATE: u64 = 0x02;
const LINUX_MAP_FIXED: u64 = 0x10;
const LINUX_MAP_ANONYMOUS: u64 = 0x20;
const LINUX_MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

fn linux_mmap(
    pml4: PhysFrame,
    address: u64,
    length: u64,
    protections: u64,
    flags: u64,
    fd: u64,
    offset: u64,
) -> i64 {
    if length == 0 || length > paging::LINUX_ARENA_PAGES * aether_memory::PAGE_SIZE {
        return LINUX_EINVAL;
    }
    let Some(length) = length.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    let anonymous = flags & LINUX_MAP_ANONYMOUS != 0;
    let shared = flags & LINUX_MAP_SHARED != 0;
    if flags & LINUX_MAP_PRIVATE != 0 && shared
        || flags & (LINUX_MAP_PRIVATE | LINUX_MAP_SHARED) == 0
    {
        return LINUX_EINVAL;
    }
    // Linux permits writable and executable mappings. LLVM's section
    // allocator asks for them directly on some paths, so rejecting the
    // combination breaks lavapipe shader compilation.
    if protections & !7 != 0 {
        return LINUX_EINVAL;
    }
    let mut file_identity = None;
    if anonymous {
        // The fd argument is ignored for MAP_ANONYMOUS. glibc passes -1 as a
        // 32-bit value, so do not require the full 64-bit all-ones pattern.
        if offset != 0 {
            return LINUX_EINVAL;
        }
    } else {
        if fd == u64::MAX || fd == u64::from(u32::MAX) || !offset.is_multiple_of(PAGE_SIZE) {
            return LINUX_EINVAL;
        }
        if shared {
            // A shared file mapping needs a stable page-cache identity. Pipes,
            // events, and pseudo-files do not have one.
            match scheduler::linux_file_identity(fd) {
                Ok(identity) => file_identity = Some(identity),
                Err(error) => return error,
            }
        } else if scheduler::linux_file_len(fd).is_err() {
            return LINUX_EBADF;
        }
    }
    let fixed = flags & LINUX_MAP_FIXED != 0;
    let no_replace = flags & LINUX_MAP_FIXED_NOREPLACE != 0;
    if fixed && !no_replace && address != 0 {
        // MAP_FIXED replaces an existing mapping. Drop the old ranges and
        // frames before installing the new one.
        if scheduler::linux_vma_remove(address, length).unwrap_or(false) {
            let _ = crate::memory::with_frame_allocator(|allocator| {
                paging::unmap_user_range(allocator, pml4, address, length)
            });
        }
    }
    let inserted = if shared {
        scheduler::linux_vma_insert_shared(
            length,
            protections as u32,
            address,
            fixed || no_replace,
            no_replace,
            anonymous,
        )
    } else {
        scheduler::linux_vma_insert(
            length,
            protections as u32,
            address,
            fixed || no_replace,
            no_replace,
            anonymous,
            false,
        )
    };
    let start = match inserted {
        Ok(start) => start,
        Err(aether_memory::vma::VmaError::Overlap) if no_replace => return LINUX_EEXIST,
        Err(error) => {
            return vma_errno(error);
        }
    };
    let Some(page_flags) = linux_mmap_page_flags(protections) else {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_EINVAL;
    };
    if shared {
        let domain = scheduler::linux_shared_domain();
        let mapped = if anonymous {
            crate::memory::with_frame_allocator(|allocator| {
                paging::map_shared_anonymous_range(
                    allocator, pml4, start, length, page_flags, domain,
                )
            })
        } else {
            let Some((key, _file_length)) = file_identity else {
                let _ = scheduler::linux_vma_remove(start, length);
                return LINUX_EINVAL;
            };
            // Linux permits a shared mapping to extend past the current
            // object size; pages beyond EOF are zero-filled on access (or
            // fault with SIGBUS on a real kernel).  Mesa/lavapipe maps a
            // 64 MiB memfd at a non-zero offset and grows/uses it afterwards,
            // so rejecting the tail here fails device creation.
            crate::memory::with_frame_allocator(|allocator| {
                map_shared_file_pages(
                    allocator, pml4, start, length, page_flags, fd, offset, key, domain,
                )
                .map_err(|_| paging::PagingError::OutOfMemory)
            })
        };
        let Some(mapped) = mapped else {
            let _ = scheduler::linux_vma_remove(start, length);
            return LINUX_ENOMEM;
        };
        if mapped.is_err() {
            let _ = scheduler::linux_vma_remove(start, length);
            return LINUX_ENOMEM;
        }
        return start as i64;
    }
    if anonymous {
        return start as i64;
    }
    // Private file-backed mappings remain eagerly populated with private
    // frames; MAP_PRIVATE must never alias the page cache.
    let writable = aether_memory::paging::PageFlags::PRESENT
        | aether_memory::paging::PageFlags::USER
        | aether_memory::paging::PageFlags::WRITABLE
        | aether_memory::paging::PageFlags::NO_EXECUTE;
    let Some(mapped) = crate::memory::with_frame_allocator(|allocator| {
        paging::map_allocated_user_range(allocator, pml4, start, length, writable)
    }) else {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_ENOMEM;
    };
    if mapped.is_err() {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_ENOMEM;
    }
    if let Err(error) = copy_file_range(pml4, fd, offset, start, length) {
        let _ = scheduler::linux_vma_remove(start, length);
        return error;
    }
    if paging::protect_user_range(pml4, start, length, page_flags).is_err() {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_EINVAL;
    }
    start as i64
}

/// Maps a file range into a shared page cache and the caller's address space.
///
/// Existing cache pages are retained; a miss allocates one frame, fills it
/// from the read-only source (zero beyond end-of-file), and registers it. The
/// registry keeps the frame alive until the last mapping is removed, so
/// unrelated processes mapping the same page alias one physical frame. Writes
/// update that frame but are never written back to the root; `msync` reports
/// `EOPNOTSUPP`.
#[allow(clippy::too_many_arguments)]
fn map_shared_file_pages(
    allocator: &mut FrameAllocator<'_>,
    pml4: PhysFrame,
    start: u64,
    length: u64,
    flags: aether_memory::paging::PageFlags,
    fd: u64,
    file_offset: u64,
    key: u64,
    domain: u8,
) -> Result<(), &'static str> {
    let pages = length / PAGE_SIZE;
    let mut page = 0u64;
    while page < pages {
        let Some(offset) = file_offset.checked_add(page.saturating_mul(PAGE_SIZE)) else {
            let _ = paging::unmap_user_range(allocator, pml4, start, page * PAGE_SIZE);
            return Err("offset");
        };
        let address = start + page * PAGE_SIZE;
        if let Some(frame) = shared::find_file(key, offset) {
            if paging::map_user_page(pml4, allocator, address, frame, flags).is_err() {
                let _ = shared::release(allocator, frame);
                let _ = paging::unmap_user_range(allocator, pml4, start, page * PAGE_SIZE);
                return Err("map");
            }
        } else {
            let Some(frame) = allocator.allocate() else {
                let _ = paging::unmap_user_range(allocator, pml4, start, page * PAGE_SIZE);
                return Err("memory");
            };
            // SAFETY: The frame was just allocated, is identity-mapped, and is
            // not reachable from any address space yet. The kernel path has
            // interrupts masked, so the slice cannot race with a user task.
            let destination = unsafe {
                core::slice::from_raw_parts_mut(
                    frame.start_address() as *mut u8,
                    PAGE_SIZE as usize,
                )
            };
            destination.fill(0);
            // A short read is normal at end-of-file; the tail stays zero.
            let _ = scheduler::linux_file_read_at(fd, offset, destination);
            if paging::map_user_page(pml4, allocator, address, frame, flags).is_err() {
                let _ = allocator.deallocate(frame);
                let _ = paging::unmap_user_range(allocator, pml4, start, page * PAGE_SIZE);
                return Err("map");
            }
            let registered = if crate::memfd::is_file_key(key) {
                shared::register_memfd(frame, domain, (key, offset))
            } else {
                shared::register(frame, domain, Some((key, offset)))
            };
            if registered.is_err() {
                let _ = paging::unmap_user_range(allocator, pml4, start, (page + 1) * PAGE_SIZE);
                return Err("quota");
            }
        }
        page += 1;
    }
    Ok(())
}

fn linux_mremap(
    pml4: PhysFrame,
    old_address: u64,
    old_size: u64,
    new_size: u64,
    flags: u64,
    new_address: u64,
) -> i64 {
    const MREMAP_MAYMOVE: u64 = 1;
    const MREMAP_FIXED: u64 = 2;
    if new_size == 0 || old_size == 0 || flags & !(MREMAP_MAYMOVE | MREMAP_FIXED) != 0 {
        return LINUX_EINVAL;
    }
    let Some(old_size) = old_size.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    let Some(new_size) = new_size.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    if !old_address.is_multiple_of(aether_memory::PAGE_SIZE)
        || !scheduler::linux_vma_covers(old_address, old_size)
    {
        return LINUX_EINVAL;
    }
    let Some(prot) = scheduler::linux_vma_prot(old_address) else {
        return LINUX_EINVAL;
    };
    if scheduler::linux_shared_vma(old_address) {
        // Moving or growing a shared mapping would need to preserve the same
        // physical frames across processes. The bounded slice does not do
        // that yet, so fail closed instead of silently unsharing it.
        return LINUX_EOPNOTSUPP;
    }
    if new_size <= old_size {
        let tail = old_size - new_size;
        if tail != 0 {
            let _ = scheduler::linux_vma_remove(old_address + new_size, tail);
            let _ = crate::memory::with_frame_allocator(|allocator| {
                paging::unmap_user_range(allocator, pml4, old_address + new_size, tail)
            });
        }
        return old_address as i64;
    }
    let delta = new_size - old_size;
    if flags & MREMAP_FIXED == 0 {
        // Try to grow in place first.
        if scheduler::linux_vma_insert(
            delta,
            prot,
            old_address + old_size,
            true,
            true,
            scheduler::linux_anonymous_vma_prot(old_address).is_some(),
            scheduler::linux_shared_vma(old_address),
        )
        .is_ok()
        {
            let writable = aether_memory::paging::PageFlags::PRESENT
                | aether_memory::paging::PageFlags::USER
                | aether_memory::paging::PageFlags::WRITABLE
                | aether_memory::paging::PageFlags::NO_EXECUTE;
            let Some(result) = crate::memory::with_frame_allocator(|allocator| {
                paging::map_allocated_user_range(
                    allocator,
                    pml4,
                    old_address + old_size,
                    delta,
                    writable,
                )
            }) else {
                return LINUX_ENOMEM;
            };
            if result.is_err() {
                return LINUX_ENOMEM;
            }
            let Some(page_flags) = linux_page_flags(prot as u64) else {
                return LINUX_EINVAL;
            };
            let _ = paging::protect_user_range(pml4, old_address + old_size, delta, page_flags);
            return old_address as i64;
        }
        if flags & MREMAP_MAYMOVE == 0 {
            return LINUX_ENOMEM;
        }
    }
    let fixed = flags & MREMAP_FIXED != 0;
    let target = if fixed { new_address } else { 0 };
    if fixed {
        if target == 0
            || target < old_address.saturating_add(old_size)
                && old_address < target.saturating_add(new_size)
        {
            return LINUX_EINVAL;
        }
        let _ = scheduler::linux_vma_remove(target, new_size);
        let _ = crate::memory::with_frame_allocator(|allocator| {
            paging::unmap_user_range(allocator, pml4, target, new_size)
        });
    }
    let Ok(new_start) = scheduler::linux_vma_insert(
        new_size,
        prot,
        target,
        fixed,
        false,
        scheduler::linux_anonymous_vma_prot(old_address).is_some(),
        scheduler::linux_shared_vma(old_address),
    ) else {
        return LINUX_ENOMEM;
    };
    let writable = aether_memory::paging::PageFlags::PRESENT
        | aether_memory::paging::PageFlags::USER
        | aether_memory::paging::PageFlags::WRITABLE
        | aether_memory::paging::PageFlags::NO_EXECUTE;
    let Some(mapped) = crate::memory::with_frame_allocator(|allocator| {
        paging::map_allocated_user_range(allocator, pml4, new_start, new_size, writable)
    }) else {
        let _ = scheduler::linux_vma_remove(new_start, new_size);
        return LINUX_ENOMEM;
    };
    if mapped.is_err() {
        let _ = scheduler::linux_vma_remove(new_start, new_size);
        return LINUX_ENOMEM;
    }
    // SAFETY: Both ranges are mapped in the current address space, do not
    // overlap, and the syscall path has interrupts masked.
    unsafe {
        core::ptr::copy_nonoverlapping(
            old_address as *const u8,
            new_start as *mut u8,
            old_size as usize,
        );
    }
    let Some(page_flags) = linux_page_flags(prot as u64) else {
        return LINUX_EINVAL;
    };
    if paging::protect_user_range(pml4, new_start, new_size, page_flags).is_err() {
        return LINUX_EINVAL;
    }
    let _ = scheduler::linux_vma_remove(old_address, old_size);
    let _ = crate::memory::with_frame_allocator(|allocator| {
        paging::unmap_user_range(allocator, pml4, old_address, old_size)
    });
    new_start as i64
}

/// Reports the bounded shared-file writeback policy.
///
/// Anonymous shared mappings have nothing to write back and report success.
/// A shared file mapping is read-shared: writes update the in-memory cache but
/// are never persisted to the read-only imported root, so `msync` fails closed
/// with `EOPNOTSUPP` instead of pretending the file was synchronized.
fn linux_msync(address: u64, length: u64, flags: u64) -> i64 {
    const MS_ASYNC: u64 = 1;
    const MS_INVALIDATE: u64 = 2;
    const MS_SYNC: u64 = 4;
    if flags & !(MS_ASYNC | MS_INVALIDATE | MS_SYNC) != 0
        || flags & (MS_ASYNC | MS_SYNC) == (MS_ASYNC | MS_SYNC)
        || !address.is_multiple_of(PAGE_SIZE)
    {
        return LINUX_EINVAL;
    }
    let Some(length) = length
        .checked_add(PAGE_SIZE - 1)
        .map(|v| v & !(PAGE_SIZE - 1))
    else {
        return LINUX_EINVAL;
    };
    if length == 0 {
        return 0;
    }
    if !scheduler::linux_vma_covers(address, length) {
        return LINUX_ENOMEM;
    }
    if scheduler::linux_shared_vma(address)
        && scheduler::linux_anonymous_vma_prot(address).is_none()
    {
        LINUX_EOPNOTSUPP
    } else {
        0
    }
}

fn linux_munmap(pml4: PhysFrame, address: u64, length: u64) -> i64 {
    if length == 0 {
        return 0;
    }
    let Some(length) = length.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    if !address.is_multiple_of(aether_memory::PAGE_SIZE) {
        return LINUX_EINVAL;
    }
    // Require a fully mapped range so clearing pages cannot reach the heap or
    // an untracked hole. Partial munmap is a later refinement.
    if !scheduler::linux_vma_covers(address, length) {
        return 0;
    }
    match scheduler::linux_vma_remove(address, length) {
        // Unmapping an unmapped range is not an error in Linux.
        Ok(false) | Err(_) => return 0,
        Ok(true) => {}
    }
    let Some(result) = crate::memory::with_frame_allocator(|allocator| {
        paging::unmap_user_range(allocator, pml4, address, length)
    }) else {
        return LINUX_ENOMEM;
    };
    if result.is_err() {
        return LINUX_EINVAL;
    }
    0
}

fn linux_mprotect(pml4: PhysFrame, address: u64, length: u64, protections: u64) -> i64 {
    if length == 0 {
        return 0;
    }
    // Linux accepts the advisory growth flags and the pkey bit in mprotect;
    // they change the VMA policy, not the page protection. The prototype
    // keeps the read/write/execute protection and drops the rest.
    const PROTECTION_MASK: u64 = 0x7;
    let protections = protections & PROTECTION_MASK;
    let Some(length) = length.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    let Some(flags) = linux_page_flags(protections) else {
        return LINUX_EINVAL;
    };
    if let Err(error) = scheduler::linux_vma_protect(address, length, protections as u32) {
        return vma_errno(error);
    }
    match paging::protect_user_range_if_present(pml4, address, length, flags) {
        Ok(()) => 0,
        Err(_) => LINUX_EINVAL,
    }
}

fn linux_stat(pml4: PhysFrame, path_pointer: u64, stat_pointer: u64) -> i64 {
    let Some(path) = user_cstr(pml4, path_pointer, 256) else {
        return LINUX_EFAULT;
    };
    let Some((size, inode, mode)) = scheduler::linux_stat_path(path) else {
        return LINUX_ENOENT;
    };
    write_stat(pml4, stat_pointer, size, inode, mode)
}

fn linux_writev(pml4: PhysFrame, descriptor: u64, iovec_pointer: u64, count: u64) -> i64 {
    if count == 0 {
        return 0;
    }
    if count > 16 {
        return LINUX_EINVAL;
    }
    let Some(iovecs) = user_bytes(pml4, iovec_pointer, count * 16, false) else {
        return LINUX_EFAULT;
    };
    let mut total = 0i64;
    for index in 0..count as usize {
        let offset = index * 16;
        let base = u64::from_ne_bytes(iovecs[offset..offset + 8].try_into().unwrap_or([0; 8]));
        let length =
            u64::from_ne_bytes(iovecs[offset + 8..offset + 16].try_into().unwrap_or([0; 8]));
        if length == 0 {
            continue;
        }
        let Some(bytes) = user_bytes(pml4, base, length, false) else {
            return LINUX_EFAULT;
        };
        if descriptor == 1 || descriptor == 2 {
            serial::write_bytes(bytes);
            total += length as i64;
        } else {
            let written = scheduler::linux_write_file(descriptor, bytes);
            match written {
                Ok(count) => total += count as i64,
                Err(error) => return error,
            }
        }
    }
    total
}

fn linux_fstat(pml4: PhysFrame, fd: u64, stat_pointer: u64) -> i64 {
    let Ok((size, inode, mode)) = scheduler::linux_stat_fd(fd) else {
        return LINUX_EBADF;
    };
    write_stat(pml4, stat_pointer, size, inode, mode)
}

fn linux_newfstatat(pml4: PhysFrame, directory: u64, path_pointer: u64, stat_pointer: u64) -> i64 {
    let Some(path) = user_cstr(pml4, path_pointer, 256) else {
        return LINUX_EFAULT;
    };
    if path.is_empty() {
        return linux_fstat(pml4, directory, stat_pointer);
    }
    linux_stat(pml4, path_pointer, stat_pointer)
}

fn write_stat(pml4: PhysFrame, pointer: u64, size: u64, inode: u64, mode: u32) -> i64 {
    let mut buffer = [0u8; 144];
    buffer[8..16].copy_from_slice(&inode.to_le_bytes());
    buffer[16..24].copy_from_slice(&1u64.to_le_bytes());
    buffer[24..28].copy_from_slice(&mode.to_le_bytes());
    buffer[28..32].copy_from_slice(&1000u32.to_le_bytes());
    buffer[32..36].copy_from_slice(&1000u32.to_le_bytes());
    buffer[48..56].copy_from_slice(&size.to_le_bytes());
    buffer[56..64].copy_from_slice(&4096i64.to_le_bytes());
    buffer[64..72].copy_from_slice(&size.div_ceil(512).to_le_bytes());
    write_user_bytes(pml4, pointer, &buffer).map_or(LINUX_EFAULT, |()| 0)
}

fn linux_statx(
    pml4: PhysFrame,
    directory: u64,
    path_pointer: u64,
    flags: u64,
    _mask: u64,
    statx_pointer: u64,
) -> i64 {
    // `AT_EMPTY_PATH` makes an empty path mean "the descriptor in dirfd".
    // glibc uses this form for fstat-like probes on an open file.
    const AT_EMPTY_PATH: u64 = 0x1000;
    let mut buffer = [0u8; 256];
    let mut use_fd = path_pointer == 0;
    let mut path = "";
    if !use_fd {
        let Some(value) = user_cstr(pml4, path_pointer, 256) else {
            return LINUX_EFAULT;
        };
        path = value;
        if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            use_fd = true;
        }
    }
    let (size, inode, mode, uid, gid) = if use_fd {
        let Ok((size, inode, mode)) = scheduler::linux_stat_fd(directory) else {
            return LINUX_EBADF;
        };
        (size, inode, mode, 1000u32, 1000u32)
    } else {
        let Some((size, inode, mode)) = scheduler::linux_stat_path(path) else {
            return LINUX_ENOENT;
        };
        (size, inode, mode, 1000u32, 1000u32)
    };
    buffer[0..4].copy_from_slice(&0x7ffu32.to_le_bytes());
    buffer[4..8].copy_from_slice(&4096u32.to_le_bytes());
    buffer[16..20].copy_from_slice(&1u32.to_le_bytes());
    buffer[20..24].copy_from_slice(&uid.to_le_bytes());
    buffer[24..28].copy_from_slice(&gid.to_le_bytes());
    buffer[28..30].copy_from_slice(&(mode as u16).to_le_bytes());
    buffer[32..40].copy_from_slice(&inode.to_le_bytes());
    buffer[40..48].copy_from_slice(&size.to_le_bytes());
    buffer[48..56].copy_from_slice(&size.div_ceil(512).to_le_bytes());
    write_user_bytes(pml4, statx_pointer, &buffer).map_or(LINUX_EFAULT, |()| 0)
}

fn linux_readlink(
    pml4: PhysFrame,
    path_pointer: u64,
    buffer_pointer: u64,
    buffer_length: u64,
    _directory: u64,
) -> i64 {
    let Some(path) = user_cstr(pml4, path_pointer, 256) else {
        return LINUX_EFAULT;
    };
    let target: &[u8] = match path {
        "/proc/self/exe" | "/proc/thread-self/exe" => b"/usr/bin/aether-linux",
        "/proc/self/cwd" | "/proc/thread-self/cwd" => b"/",
        _ => return LINUX_EINVAL,
    };
    if buffer_length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    let length = target.len().min(buffer_length as usize);
    write_user_bytes(pml4, buffer_pointer, &target[..length])
        .map_or(LINUX_EFAULT, |()| length as i64)
}

fn linux_epoll_ctl(
    pml4: PhysFrame,
    epoll_fd: u64,
    operation: u64,
    target_fd: u64,
    event_pointer: u64,
) -> i64 {
    let Some(raw) = user_bytes(pml4, event_pointer, 12, false) else {
        return LINUX_EFAULT;
    };
    let events = u32::from_ne_bytes(raw[..4].try_into().unwrap_or([0; 4]));
    let data = u64::from_ne_bytes(raw[4..12].try_into().unwrap_or([0; 8]));
    scheduler::linux_epoll_ctl(epoll_fd, operation, target_fd, events, data)
        .map_or_else(|error| error, |()| 0)
}

/// Converts a Linux millisecond timeout into an absolute tick deadline.
/// `None` means an infinite wait; a zero timeout yields the current tick so
/// the caller returns after one non-blocking check.
fn linux_poll_deadline(timeout: i64) -> Option<u64> {
    let now = scheduler::ticks();
    if timeout < 0 {
        None
    } else if timeout == 0 {
        Some(now)
    } else {
        let ticks = (timeout as u64).div_ceil(10).max(1);
        Some(now.saturating_add(ticks))
    }
}

fn linux_epoll_wait(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    number: u64,
    epoll_fd: u64,
    events_pointer: u64,
    max_events: u64,
    timeout: u64,
) -> *mut InterruptFrame {
    if max_events == 0 {
        return linux_result(frame, LINUX_EINVAL);
    }
    // Linux accepts any positive maxevents; the prototype keeps a fixed
    // 16-event scratch buffer, so clamp instead of failing the whole call.
    let max_events = max_events.min(16);
    let timeout = timeout as i64;
    let deadline = match scheduler::linux_take_restart(number) {
        Some(deadline) => deadline,
        None => linux_poll_deadline(timeout),
    };
    let mut events = [scheduler::LinuxEpollEvent { events: 0, data: 0 }; 16];
    let count = match scheduler::linux_epoll_wait(epoll_fd, &mut events[..max_events as usize]) {
        Ok(count) => count,
        Err(error) => return linux_result(frame, error),
    };
    if count != 0 {
        let mut buffer = [0u8; 16 * 12];
        for (index, event) in events[..count].iter().enumerate() {
            let offset = index * 12;
            buffer[offset..offset + 4].copy_from_slice(&event.events.to_ne_bytes());
            buffer[offset + 4..offset + 12].copy_from_slice(&event.data.to_ne_bytes());
        }
        return match write_user_bytes(pml4, events_pointer, &buffer[..count * 12]) {
            Some(()) => linux_result(frame, count as i64),
            None => linux_result(frame, LINUX_EFAULT),
        };
    }
    if timeout == 0 || deadline.is_some_and(|deadline| scheduler::ticks() >= deadline) {
        return linux_result(frame, 0);
    }
    if scheduler::linux_signal_pending() {
        return linux_result(frame, LINUX_EINTR);
    }
    scheduler::linux_park(frame, number, scheduler::LinuxWait::Poll, deadline)
}

fn linux_pipe2(pml4: PhysFrame, fds_pointer: u64, _flags: u64) -> i64 {
    let Ok((read, write)) = scheduler::linux_pipe() else {
        return LINUX_ENOMEM;
    };
    let mut buffer = [0u8; 8];
    buffer[..4].copy_from_slice(&(read as u32).to_le_bytes());
    buffer[4..].copy_from_slice(&(write as u32).to_le_bytes());
    write_user_bytes(pml4, fds_pointer, &buffer).map_or(LINUX_EFAULT, |()| 0)
}

fn linux_poll(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    fds_pointer: u64,
    count: u64,
    timeout: u64,
) -> *mut InterruptFrame {
    if count > 16 {
        return linux_result(frame, LINUX_EINVAL);
    }
    // poll's timeout is signed: negative waits forever, zero probes once.
    let timeout = timeout as i64;
    let deadline = match scheduler::linux_take_restart(7) {
        Some(deadline) => deadline,
        None => linux_poll_deadline(timeout),
    };
    let length = count * 8;
    // SAFETY: The pollfd array is validated as writable user memory on
    // every entry; the task is parked with the syscall rewound.
    let Some(fds) = (unsafe { user_bytes_mut(pml4, fds_pointer, length) }) else {
        return linux_result(frame, LINUX_EFAULT);
    };
    let mut ready = 0i64;
    for index in 0..count as usize {
        let offset = index * 8;
        let fd = i32::from_ne_bytes(fds[offset..offset + 4].try_into().unwrap_or([0; 4]));
        let events = i16::from_ne_bytes(fds[offset + 4..offset + 6].try_into().unwrap_or([0; 2]));
        let revents = if fd < 0 {
            0
        } else {
            scheduler::linux_poll(fd as u64, events)
        };
        fds[offset + 6..offset + 8].copy_from_slice(&revents.to_ne_bytes());
        if revents != 0 {
            ready += 1;
        }
    }
    if ready != 0 || (count == 0 && timeout == 0) {
        return linux_result(frame, ready);
    }
    if timeout == 0 || deadline.is_some_and(|deadline| scheduler::ticks() >= deadline) {
        return linux_result(frame, 0);
    }
    if scheduler::linux_signal_pending() {
        return linux_result(frame, LINUX_EINTR);
    }
    scheduler::linux_park(frame, 7, scheduler::LinuxWait::Poll, deadline)
}

fn linux_getdents(
    pml4: PhysFrame,
    descriptor: u64,
    buffer_pointer: u64,
    buffer_length: u64,
) -> i64 {
    if buffer_length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    // SAFETY: The range is validated as writable user memory and the syscall
    // path holds interrupts masked.
    let Some(buffer) = (unsafe { user_bytes_mut(pml4, buffer_pointer, buffer_length) }) else {
        return LINUX_EFAULT;
    };
    scheduler::linux_getdents(descriptor, buffer).map_or_else(|error| error, |count| count as i64)
}

fn linux_fcntl(fd: u64, command: u64, _argument: u64) -> i64 {
    if scheduler::linux_stat_fd(fd).is_err() {
        return LINUX_EBADF;
    }
    match command {
        0 | 1030 => scheduler::linux_dup(fd).map_or_else(|error| error, |new| new as i64),
        1..=4 => 0,
        _ => LINUX_EINVAL,
    }
}

fn linux_sched_getaffinity(pml4: PhysFrame, _pid: u64, length: u64, pointer: u64) -> i64 {
    if length < 8 {
        return LINUX_EINVAL;
    }
    write_user_bytes(pml4, pointer, &1u64.to_le_bytes()).map_or(LINUX_EFAULT, |()| 8)
}

fn linux_prlimit(pml4: PhysFrame, _resource: u64, _new_limit: u64, old_limit: u64) -> i64 {
    if old_limit == 0 {
        return 0;
    }
    let mut limits = [0u8; 16];
    limits[..8].copy_from_slice(&(8 * 1024 * 1024u64).to_le_bytes());
    limits[8..].copy_from_slice(&(8 * 1024 * 1024u64).to_le_bytes());
    write_user_bytes(pml4, old_limit, &limits).map_or(LINUX_EFAULT, |()| 0)
}

fn vma_errno(error: aether_memory::vma::VmaError) -> i64 {
    use aether_memory::vma::VmaError;
    match error {
        VmaError::InvalidRange
        | VmaError::OutsideWindow
        | VmaError::InvalidProtection
        | VmaError::Overlap => LINUX_EINVAL,
        VmaError::OutOfSpace | VmaError::NotFound => LINUX_ENOMEM,
    }
}

fn page_flags_from_protections(protections: u64) -> aether_memory::paging::PageFlags {
    let mut flags =
        aether_memory::paging::PageFlags::PRESENT | aether_memory::paging::PageFlags::USER;
    if protections & 2 != 0 {
        flags = flags | aether_memory::paging::PageFlags::WRITABLE;
    }
    if protections & 4 == 0 {
        flags = flags | aether_memory::paging::PageFlags::NO_EXECUTE;
    }
    flags
}

fn linux_page_flags(protections: u64) -> Option<aether_memory::paging::PageFlags> {
    // mprotect must not add write to an executable mapping: the LLVM JIT
    // flips a W+X mapping to read+execute, never the other way around.
    if protections & !7 != 0 || protections & 6 == 6 {
        return None;
    }
    Some(page_flags_from_protections(protections))
}

/// Page flags for the initial `mmap`.
///
/// An initial mapping may be writable+executable: Linux permits it and the
/// LLVM section allocator used by lavapipe asks for it directly. The stricter
/// [`linux_page_flags`] applies to later protection changes.
fn linux_mmap_page_flags(protections: u64) -> Option<aether_memory::paging::PageFlags> {
    if protections & !7 != 0 {
        return None;
    }
    Some(page_flags_from_protections(protections))
}

fn copy_file_range(
    pml4: PhysFrame,
    fd: u64,
    file_offset: u64,
    address: u64,
    length: u64,
) -> Result<(), i64> {
    let mut offset = 0u64;
    while offset < length {
        let chunk = (length - offset).min(MAX_SYSCALL_IO as u64);
        // SAFETY: The range is inside the Linux task's mapped arena and the
        // syscall path has interrupts masked.
        let Some(buffer) = (unsafe { user_bytes_mut(pml4, address + offset, chunk) }) else {
            return Err(LINUX_EFAULT);
        };
        let Some(position) = file_offset.checked_add(offset) else {
            return Err(LINUX_EINVAL);
        };
        let read = scheduler::linux_file_read_at(fd, position, buffer)?;
        if read < chunk as usize {
            // The file ended early; the remaining bytes stay zero-filled.
            break;
        }
        offset += chunk;
    }
    Ok(())
}

fn linux_arch_prctl(pml4: PhysFrame, code: u64, value: u64) -> i64 {
    match code {
        0x1002 => {
            scheduler::set_linux_fs_base(value);
            0
        }
        0x1003 => write_user_u64(pml4, value).map_or(LINUX_EFAULT, |()| 0),
        _ => LINUX_ENOSYS,
    }
}

fn linux_rt_sigaction(pml4: PhysFrame, signum: u64, action_pointer: u64, old_pointer: u64) -> i64 {
    if signum == 0 || signum > 64 || signum == 9 || signum == 19 {
        return LINUX_EINVAL;
    }
    let old = scheduler::linux_get_sigaction(signum as usize);
    if action_pointer != 0 {
        let Some(raw) = user_bytes(pml4, action_pointer, 32, false) else {
            return LINUX_EFAULT;
        };
        let action = scheduler::SigAction {
            handler: u64::from_ne_bytes(raw[0..8].try_into().unwrap_or([0; 8])),
            flags: u64::from_ne_bytes(raw[8..16].try_into().unwrap_or([0; 8])),
            restorer: u64::from_ne_bytes(raw[16..24].try_into().unwrap_or([0; 8])),
            mask: u64::from_ne_bytes(raw[24..32].try_into().unwrap_or([0; 8])),
        };
        // A user handler needs a restorer trampoline; SIG_DFL (0) and SIG_IGN
        // (1) do not. This keeps `rt_sigreturn` well-defined.
        if action.handler > 1
            && (action.restorer == 0
                || action.handler < aether_abi::user::USER_BASE
                || action.handler >= aether_abi::user::USER_LIMIT
                || action.restorer < aether_abi::user::USER_BASE
                || action.restorer >= aether_abi::user::USER_LIMIT)
        {
            return LINUX_EINVAL;
        }
        scheduler::linux_set_sigaction(signum as usize, action);
    }
    if old_pointer != 0 {
        let mut raw = [0u8; 32];
        if let Some(action) = old {
            raw[0..8].copy_from_slice(&action.handler.to_ne_bytes());
            raw[8..16].copy_from_slice(&action.flags.to_ne_bytes());
            raw[16..24].copy_from_slice(&action.restorer.to_ne_bytes());
            raw[24..32].copy_from_slice(&action.mask.to_ne_bytes());
        }
        if write_user_bytes(pml4, old_pointer, &raw).is_none() {
            return LINUX_EFAULT;
        }
    }
    0
}

fn linux_rt_sigprocmask(pml4: PhysFrame, how: u64, set_pointer: u64, old_pointer: u64) -> i64 {
    let old = scheduler::linux_sigmask();
    if set_pointer != 0 {
        let Some(raw) = user_bytes(pml4, set_pointer, 8, false) else {
            return LINUX_EFAULT;
        };
        let set = u64::from_ne_bytes(raw.try_into().unwrap_or([0; 8]));
        let updated = match how {
            0 => old | set,
            1 => old & !set,
            2 => set,
            _ => return LINUX_EINVAL,
        };
        scheduler::linux_set_sigmask(updated);
    }
    if old_pointer != 0 && write_user_bytes(pml4, old_pointer, &old.to_ne_bytes()).is_none() {
        return LINUX_EFAULT;
    }
    0
}

fn linux_sigaltstack(pml4: PhysFrame, stack_pointer: u64, old_pointer: u64) -> i64 {
    const SS_ONSTACK: u32 = 1;
    const SS_DISABLE: u32 = 2;
    const SS_AUTODISARM: u32 = 1 << 31;
    if stack_pointer != 0 {
        let Some(raw) = user_bytes(pml4, stack_pointer, 24, false) else {
            return LINUX_EFAULT;
        };
        let pointer = u64::from_ne_bytes(raw[0..8].try_into().unwrap_or([0; 8]));
        let flags = u32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0; 4]));
        let size = u64::from_ne_bytes(raw[16..24].try_into().unwrap_or([0; 8]));
        if flags & SS_ONSTACK != 0 || flags & !(SS_DISABLE | SS_AUTODISARM) != 0 {
            return LINUX_EINVAL;
        }
        let (_, active) = scheduler::linux_altstack_state();
        if active {
            // Linux refuses to replace a stack that is currently in use.
            return LINUX_EPERM;
        }
        if flags & SS_DISABLE != 0 {
            scheduler::linux_set_altstack(0, 0);
        } else {
            const MIN_ALTSTACK: u64 = 512;
            if pointer < aether_abi::user::USER_BASE || size < MIN_ALTSTACK {
                return LINUX_ENOMEM;
            }
            let Some(end) = pointer.checked_add(size) else {
                return LINUX_ENOMEM;
            };
            if end > aether_abi::user::USER_LIMIT {
                return LINUX_ENOMEM;
            }
            if !linux_user_range_valid(pml4, pointer, size.min(4096), true) {
                return LINUX_ENOMEM;
            }
            scheduler::linux_set_altstack(pointer, size);
        }
    }
    if old_pointer != 0 {
        let mut raw = [0u8; 24];
        let (stack, active) = scheduler::linux_altstack_state();
        if let Some((pointer, size)) = stack {
            raw[0..8].copy_from_slice(&pointer.to_ne_bytes());
            let flags = if active { SS_ONSTACK } else { 0 };
            raw[8..12].copy_from_slice(&flags.to_ne_bytes());
            raw[16..24].copy_from_slice(&size.to_ne_bytes());
        } else {
            raw[8..12].copy_from_slice(&SS_DISABLE.to_ne_bytes());
        }
        if write_user_bytes(pml4, old_pointer, &raw).is_none() {
            return LINUX_EFAULT;
        }
    }
    0
}

fn linux_kill(pml4: PhysFrame, pid: u64, signal: u64) -> i64 {
    let current = scheduler::linux_getpid() as u64;
    let _ = pml4;
    if pid == 0 {
        return scheduler::linux_send_signal_group(
            scheduler::linux_getpgid(0).unwrap_or(current as usize),
            signal,
        );
    }
    let pid = pid as i64;
    if pid == -1 {
        // Broadcast kill crosses process/application boundaries; unsupported.
        return LINUX_ENOSYS;
    }
    if pid < -1 {
        let Some(group) = pid.checked_abs() else {
            return LINUX_EINVAL;
        };
        return scheduler::linux_send_signal_group(group as usize, signal);
    }
    scheduler::linux_send_signal_to(pid as usize, None, signal)
}

fn linux_setpgid(pid: u64, pgid: u64) -> i64 {
    const MAX_PID: u64 = 1 << 20;
    if pid > MAX_PID {
        return LINUX_ESRCH;
    }
    // A negative pgid arrives as a huge unsigned value; Linux rejects it.
    if pgid > MAX_PID {
        return LINUX_EINVAL;
    }
    scheduler::linux_setpgid(pid as usize, pgid as usize)
}

fn linux_getpgid(pid: u64) -> i64 {
    if pid > 1 << 20 {
        return LINUX_ESRCH;
    }
    scheduler::linux_getpgid(pid as usize).map_or_else(|error| error, |pgid| pgid as i64)
}

fn linux_getsid(pid: u64) -> i64 {
    if pid > 1 << 20 {
        return LINUX_ESRCH;
    }
    scheduler::linux_getsid(pid as usize)
}

fn linux_tgkill(pml4: PhysFrame, tgid: u64, tid: u64, signal: u64) -> i64 {
    let _ = pml4;
    if tgid == 0 || tid == 0 || tgid > usize::MAX as u64 || tid > usize::MAX as u64 {
        return LINUX_EINVAL;
    }
    scheduler::linux_send_signal_to(tgid as usize, Some(tid as usize), signal)
}

fn linux_sigreturn(frame: *mut InterruptFrame) -> *mut InterruptFrame {
    if scheduler::linux_sigreturn(frame) {
        if let Some((pid, status_pointer)) = scheduler::linux_take_wait_restart() {
            // `SA_RESTART` for wait4: resume the interrupted blocking wait
            // after the handler returned, reaping an already-exited child.
            return scheduler::linux_wait4_block(frame, pid, status_pointer);
        }
        return frame;
    }
    // SAFETY: The frame is live on the current kernel stack.
    unsafe { (*frame).rax = LINUX_EINVAL as u64 };
    frame
}

fn linux_execve(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    path_pointer: u64,
    argv_pointer: u64,
    envp_pointer: u64,
) -> *mut InterruptFrame {
    let Some(user_path) = user_cstr(pml4, path_pointer, 256) else {
        unsafe { (*frame).rax = LINUX_EFAULT as u64 };
        return frame;
    };
    let mut path_storage = [0u8; 256];
    if user_path.is_empty() || user_path.len() >= path_storage.len() {
        unsafe { (*frame).rax = LINUX_ENAMETOOLONG as u64 };
        return frame;
    }
    path_storage[..user_path.len()].copy_from_slice(user_path.as_bytes());
    let path = core::str::from_utf8(&path_storage[..user_path.len()]).unwrap_or("");
    let Some(bytes) = crate::linux::file_data(path) else {
        unsafe { (*frame).rax = LINUX_ENOENT as u64 };
        return frame;
    };

    let mut argument_storage = [[0u8; 256]; 16];
    let mut argument_lengths = [0usize; 16];
    let mut argument_count = 1usize;
    let mut environment_storage = [[0u8; 256]; 16];
    let mut environment_lengths = [0usize; 16];
    let mut environment_count = 0usize;
    if argv_pointer != 0 {
        match read_exec_vector(
            pml4,
            argv_pointer,
            &mut argument_storage,
            &mut argument_lengths,
        ) {
            Some(count) => argument_count = count,
            None => {
                unsafe { (*frame).rax = LINUX_EFAULT as u64 };
                return frame;
            }
        }
        if argument_count == 0 {
            argument_storage[0][..path.len()].copy_from_slice(path.as_bytes());
            argument_lengths[0] = path.len();
            argument_count = 1;
        }
    } else {
        argument_storage[0][..path.len()].copy_from_slice(path.as_bytes());
        argument_lengths[0] = path.len();
    }
    if envp_pointer != 0 {
        match read_exec_vector(
            pml4,
            envp_pointer,
            &mut environment_storage,
            &mut environment_lengths,
        ) {
            Some(count) => environment_count = count,
            None => {
                unsafe { (*frame).rax = LINUX_EFAULT as u64 };
                return frame;
            }
        }
    }
    let mut arguments = [""; 16];
    let mut environment = [""; 16];
    for index in 0..argument_count {
        let Ok(value) = core::str::from_utf8(&argument_storage[index][..argument_lengths[index]])
        else {
            unsafe { (*frame).rax = LINUX_EFAULT as u64 };
            return frame;
        };
        arguments[index] = value;
    }
    for index in 0..environment_count {
        let Ok(value) =
            core::str::from_utf8(&environment_storage[index][..environment_lengths[index]])
        else {
            unsafe { (*frame).rax = LINUX_EFAULT as u64 };
            return frame;
        };
        environment[index] = value;
    }

    // A PT_INTERP image uses the Linux ELF loader and the interpreter named by
    // that image, resolved only from the authenticated read-only root.
    if let Ok(parsed) = aether_linux_elf::parse(bytes)
        && let Some(interpreter_path) = parsed.interpreter()
    {
        let Some(interpreter) = crate::linux::file_data(interpreter_path) else {
            unsafe { (*frame).rax = LINUX_ENOENT as u64 };
            return frame;
        };
        let loaded = crate::memory::with_frame_allocator(|allocator| {
            crate::linux_loader::load(
                allocator,
                bytes,
                interpreter,
                path,
                &arguments[..argument_count],
                &environment[..environment_count],
            )
        });
        let Some(Ok(loaded)) = loaded else {
            unsafe { (*frame).rax = LINUX_ENOMEM as u64 };
            return frame;
        };
        paging::activate(loaded.pml4);
        unsafe {
            (*frame).rax = 0;
            (*frame).rip = loaded.entry;
            (*frame).rsp = loaded.stack_pointer;
        }
        scheduler::linux_replace_image(loaded.pml4);
        return frame;
    }

    let Ok(image) = aether_boot_common::elf::parse(bytes) else {
        unsafe { (*frame).rax = LINUX_EINVAL as u64 };
        return frame;
    };
    let Some(new_pml4) = crate::memory::with_frame_allocator(|allocator| {
        paging::create_user_address_space(allocator, &image, bytes)
    }) else {
        unsafe { (*frame).rax = LINUX_ENOMEM as u64 };
        return frame;
    };
    let Ok(new_pml4) = new_pml4 else {
        unsafe { (*frame).rax = LINUX_ENOMEM as u64 };
        return frame;
    };

    let stack_page = USER_STACK_TOP - aether_memory::PAGE_SIZE;
    let Some((page, stack_pointer)) = build_static_exec_stack(
        stack_page,
        &arguments[..argument_count],
        &environment[..environment_count],
    ) else {
        let _ = crate::memory::with_frame_allocator(|allocator| {
            // SAFETY: The new space was never activated or given to a task.
            unsafe { paging::destroy_user_address_space(allocator, new_pml4) };
        });
        unsafe { (*frame).rax = LINUX_E2BIG as u64 };
        return frame;
    };

    paging::activate(new_pml4);
    // SAFETY: The new address space maps the stack page writable, and the
    // syscall path has interrupts masked.
    unsafe {
        core::ptr::copy_nonoverlapping(page.as_ptr(), stack_page as *mut u8, page.len());
        (*frame).rax = 0;
        (*frame).rip = image.entry();
        (*frame).rsp = stack_pointer;
    }
    scheduler::linux_replace_image(new_pml4);
    frame
}

fn build_static_exec_stack(
    stack_page: u64,
    arguments: &[&str],
    environment: &[&str],
) -> Option<([u8; aether_memory::PAGE_SIZE as usize], u64)> {
    if arguments.is_empty() || arguments.len() > 16 || environment.len() > 16 {
        return None;
    }
    let mut page = [0u8; aether_memory::PAGE_SIZE as usize];
    let mut string_cursor = 0usize;
    let mut put_string = |value: &str| -> Option<u64> {
        let end = string_cursor.checked_add(value.len())?.checked_add(1)?;
        if end > 1024 {
            return None;
        }
        let pointer = stack_page.checked_add(string_cursor as u64)?;
        page[string_cursor..string_cursor + value.len()].copy_from_slice(value.as_bytes());
        page[string_cursor + value.len()] = 0;
        string_cursor = end;
        Some(pointer)
    };
    let mut argv = [0u64; 16];
    for (slot, value) in argv.iter_mut().zip(arguments) {
        *slot = put_string(value)?;
    }
    let mut envp = [0u64; 16];
    for (slot, value) in envp.iter_mut().zip(environment) {
        *slot = put_string(value)?;
    }

    let mut offset = 1024usize;
    let mut push = |value: u64| -> Option<()> {
        let end = offset.checked_add(8)?;
        let slot = page.get_mut(offset..end)?;
        slot.copy_from_slice(&value.to_le_bytes());
        offset = end;
        Some(())
    };
    push(arguments.len() as u64)?;
    for value in argv.iter().take(arguments.len()) {
        push(*value)?;
    }
    push(0)?;
    for value in envp.iter().take(environment.len()) {
        push(*value)?;
    }
    push(0)?;
    push(0)?; // AT_NULL
    push(0)?;
    Some((page, stack_page.checked_add(1024)?))
}

/// Copies a bounded, NULL-terminated Linux string vector from user memory.
/// The returned strings borrow only validated user ranges and are consumed
/// before replacing the caller's address space.
fn read_exec_vector(
    pml4: PhysFrame,
    pointer: u64,
    output: &mut [[u8; 256]],
    lengths: &mut [usize],
) -> Option<usize> {
    if output.len() != lengths.len() {
        return None;
    }
    for index in 0..output.len() {
        let slot = pointer.checked_add((index as u64).checked_mul(8)?)?;
        let bytes = user_bytes(pml4, slot, 8, false)?;
        let value = u64::from_ne_bytes(bytes.try_into().ok()?);
        if value == 0 {
            return Some(index);
        }
        let string = user_cstr(pml4, value, 256)?;
        if string.len() >= output[index].len() {
            return None;
        }
        output[index][..string.len()].copy_from_slice(string.as_bytes());
        lengths[index] = string.len();
    }
    // Reaching the fixed vector limit without a terminator is treated as an
    // invalid argument list rather than reading unbounded user memory.
    None
}

fn linux_wait4(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    pid: u64,
    status_pointer: u64,
    options: u64,
) -> *mut InterruptFrame {
    const WNOHANG: u64 = 1;
    let pid = pid as i64;
    if pid < -1 {
        unsafe { (*frame).rax = LINUX_EINVAL as u64 };
        return frame;
    }
    if options & !WNOHANG != 0 {
        // WUNTRACED/WCONTINUED and other stop/continue reporting are not
        // implemented; fail explicitly rather than pretending to wait.
        unsafe { (*frame).rax = LINUX_ENOSYS as u64 };
        return frame;
    }
    if status_pointer != 0 && !linux_user_range_valid(pml4, status_pointer, 4, true) {
        unsafe { (*frame).rax = LINUX_EFAULT as u64 };
        return frame;
    }
    let result = scheduler::linux_wait4(pid);
    if matches!(result, Ok(None)) && options & WNOHANG == 0 {
        return scheduler::linux_wait4_block(frame, pid, status_pointer);
    }
    let value = match result {
        Ok(Some((child, status))) => {
            if status_pointer != 0
                && write_user_bytes(pml4, status_pointer, &(status as u32).to_ne_bytes()).is_none()
            {
                unsafe { (*frame).rax = LINUX_EFAULT as u64 };
                return frame;
            }
            child as i64
        }
        Ok(None) => 0,
        Err(error) => error,
    };
    unsafe { (*frame).rax = value as u64 };
    frame
}

fn linux_clone(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    flags: u64,
    child_stack: u64,
    parent_tid: u64,
    child_tid: u64,
    tls: u64,
) -> i64 {
    const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
    const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
    const CLONE_CHILD_SETTID: u64 = 0x0100_0000;
    if flags & CLONE_PARENT_SETTID != 0
        && (parent_tid == 0 || !linux_user_range_valid(pml4, parent_tid, 4, true))
    {
        return LINUX_EFAULT;
    }
    if flags & (CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID) != 0
        && (child_tid == 0 || !linux_user_range_valid(pml4, child_tid, 4, true))
    {
        return LINUX_EFAULT;
    }
    let tid = match scheduler::linux_clone(frame, flags, child_stack, child_tid, tls) {
        Ok(tid) => tid,
        Err(error) => return error,
    };
    if flags & CLONE_PARENT_SETTID != 0 {
        let _ = write_user_u32(pml4, parent_tid, tid as u32);
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        let _ = write_user_u32(pml4, child_tid, tid as u32);
    }
    tid as i64
}

fn linux_clone3(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    arguments_pointer: u64,
    size: u64,
) -> i64 {
    const CLONE_ARGS_SIZE: u64 = 88;
    const CLONE_ARGS_MIN: u64 = 64;
    const CLONE_VM: u64 = 0x0000_0100;
    const CLONE_FS: u64 = 0x0000_0200;
    const CLONE_FILES: u64 = 0x0000_0400;
    const CLONE_SIGHAND: u64 = 0x0000_0800;
    const CLONE_THREAD: u64 = 0x0001_0000;
    const CLONE_SETTLS: u64 = 0x0008_0000;
    const CLONE_PARENT_SETTID: u64 = 0x0010_0000;
    const CLONE_CHILD_CLEARTID: u64 = 0x0020_0000;
    const CLONE_CHILD_SETTID: u64 = 0x0100_0000;
    const SUPPORTED: u64 = CLONE_VM
        | CLONE_FS
        | CLONE_FILES
        | CLONE_SIGHAND
        | CLONE_THREAD
        | CLONE_SETTLS
        | CLONE_PARENT_SETTID
        | CLONE_CHILD_CLEARTID
        | CLONE_CHILD_SETTID;

    if size < CLONE_ARGS_MIN {
        return LINUX_EINVAL;
    }
    let read = size.min(CLONE_ARGS_SIZE);
    let Some(raw) = user_bytes(pml4, arguments_pointer, read, false) else {
        return LINUX_EFAULT;
    };
    let field = |offset: usize| -> u64 {
        raw.get(offset..offset + 8)
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(0, u64::from_ne_bytes)
    };
    let flags = field(0);
    let pidfd = field(8);
    let child_tid = field(16);
    let parent_tid = field(24);
    let exit_signal = field(32);
    let stack = field(40);
    let stack_size = field(48);
    let tls = field(56);
    let set_tid = field(64);
    let set_tid_size = field(72);
    let cgroup = field(80);

    // Only the thread-creating subset of clone3 is mapped to the bounded
    // clone path. pidfds, set_tid, cgroups, and non-thread process creation
    // fail explicitly instead of silently changing meaning.
    if pidfd != 0
        || set_tid != 0
        || set_tid_size != 0
        || cgroup != 0
        || exit_signal != 0
        || flags & !SUPPORTED != 0
        || flags & (CLONE_VM | CLONE_THREAD) != (CLONE_VM | CLONE_THREAD)
    {
        return LINUX_ENOSYS;
    }
    if stack == 0 {
        return LINUX_EINVAL;
    }
    let child_stack = if stack_size == 0 {
        stack & !15
    } else {
        let Some(top) = stack.checked_add(stack_size) else {
            return LINUX_EINVAL;
        };
        top & !15
    };
    if !(aether_abi::user::USER_BASE..aether_abi::user::USER_LIMIT).contains(&child_stack) {
        return LINUX_EINVAL;
    }
    linux_clone(frame, pml4, flags, child_stack, parent_tid, child_tid, tls)
}

fn linux_set_robust_list(pml4: PhysFrame, head: u64, length: u64) -> i64 {
    if head == 0 && length == 0 {
        return scheduler::linux_set_robust_list(0, 0);
    }
    if length != 24 || head == 0 || !head.is_multiple_of(8) {
        return LINUX_EINVAL;
    }
    if !linux_user_range_valid(pml4, head, 24, false) {
        return LINUX_EFAULT;
    }
    scheduler::linux_set_robust_list(head, length)
}

fn linux_get_robust_list(pml4: PhysFrame, pid: u64, head_pointer: u64, length_pointer: u64) -> i64 {
    if pid > 1 << 20 {
        return LINUX_ESRCH;
    }
    if head_pointer == 0
        || length_pointer == 0
        || !linux_user_range_valid(pml4, head_pointer, 8, true)
        || !linux_user_range_valid(pml4, length_pointer, 8, true)
    {
        return LINUX_EFAULT;
    }
    let (head, length) = match scheduler::linux_get_robust_list(pid as usize) {
        Ok(value) => value,
        Err(error) => return error,
    };
    if write_user_bytes(pml4, head_pointer, &head.to_ne_bytes()).is_none()
        || write_user_bytes(pml4, length_pointer, &length.to_ne_bytes()).is_none()
    {
        return LINUX_EFAULT;
    }
    0
}

fn linux_rseq(pml4: PhysFrame, address: u64, length: u64, flags: u64, signature: u64) -> i64 {
    if flags != 0 || signature != 0 {
        return LINUX_EINVAL;
    }
    if address == 0 {
        if length != 0 {
            return LINUX_EINVAL;
        }
        scheduler::linux_set_rseq(0, 0);
        return 0;
    }
    // struct rseq is 32 bytes; a bounded area keeps the registration from
    // covering an arbitrary user range. Alignment is required by Linux.
    if !(32..=4096).contains(&length) || !address.is_multiple_of(32) {
        return LINUX_EINVAL;
    }
    if !linux_user_range_valid(pml4, address, length, true) {
        return LINUX_EFAULT;
    }
    // The bounded stub reports CPU 0 and never sends preemption aborts.
    if write_user_bytes(pml4, address, &[0u8; 8]).is_none() {
        return LINUX_EFAULT;
    }
    scheduler::linux_set_rseq(address, length);
    0
}

fn linux_futex(frame: *mut InterruptFrame, pml4: PhysFrame) -> *mut InterruptFrame {
    let uaddr = unsafe { (*frame).rdi };
    let operation = unsafe { (*frame).rsi };
    let value = unsafe { (*frame).rdx } as u32;
    // `arg4` doubles as the relative timeout for WAIT and as the wake count of
    // `UADDR2` for WAKE_OP / requeue (Linux documents this aliasing).
    let arg4 = unsafe { (*frame).r10 };
    let uaddr2 = unsafe { (*frame).r8 };
    let arg5 = unsafe { (*frame).r9 } as u32;
    // FUTEX_PRIVATE_FLAG (0x80) and FUTEX_CLOCK_REALTIME (0x100) are flags;
    // the command is the low byte. Shared futexes behave like private ones in
    // this single-CPU model: all waiters share one table keyed by address.
    let command = operation & 0x7f;
    let realtime = operation & 0x100 != 0;
    match command {
        0 | 9 => {
            // FUTEX_WAIT / FUTEX_WAIT_BITSET. Linux rejects a mismatched word
            // with EAGAIN; bitsets are ignored because every wake here wakes
            // any waiter of the address.
            if !address_valid(pml4, uaddr) {
                return linux_result(frame, LINUX_EFAULT);
            }
            // SAFETY: The four-byte user word was validated above.
            let observed = unsafe { core::ptr::read(uaddr as *const u32) } as u64;
            if observed != u64::from(value) {
                return linux_result(frame, LINUX_EAGAIN);
            }
            let mut relative = None;
            if arg4 != 0 {
                let Some((seconds, nanoseconds)) = read_timespec(pml4, arg4) else {
                    return linux_result(frame, LINUX_EINVAL);
                };
                // CLOCK_REALTIME is served by the same single monotonic clock,
                // a bounded simplification documented for this prototype.
                let _ = realtime;
                let Some(ticks) = timespec_ticks(seconds, nanoseconds) else {
                    return linux_result(frame, LINUX_EINVAL);
                };
                if command == 9 {
                    // WAIT_BITSET takes an absolute monotonic deadline; the
                    // shared futex bookkeeping stores a remaining duration.
                    if scheduler::ticks() >= ticks {
                        return linux_result(frame, LINUX_ETIMEDOUT);
                    }
                    relative = Some(ticks - scheduler::ticks());
                } else if ticks == 0 {
                    // Linux reports ETIMEDOUT immediately for a relative
                    // zero-length wait.
                    return linux_result(frame, LINUX_ETIMEDOUT);
                } else {
                    relative = Some(ticks);
                }
            };
            scheduler::futex_trace(command, uaddr, value, arg4, 0, 1);
            scheduler::linux_futex_wait(frame, uaddr, relative)
        }
        1 | 10 => {
            // FUTEX_WAKE / FUTEX_WAKE_BITSET.
            if !address_valid(pml4, uaddr) {
                return linux_result(frame, LINUX_EFAULT);
            }
            let woken = scheduler::linux_futex_wake(uaddr, u64::from(value));
            if woken == 0 {
                scheduler::futex_trace(command, uaddr, value, arg4, 0, -2);
            } else {
                scheduler::futex_trace(command, uaddr, value, arg4, 0, woken);
            }
            linux_result(frame, woken)
        }
        4 => {
            // FUTEX_CMP_REQUEUE: wake `value` waiters at `uaddr`, then move up
            // to `arg4` further waiters to `uaddr2` when `*uaddr == arg5`.
            if !address_valid(pml4, uaddr) || !address_valid(pml4, uaddr2) {
                return linux_result(frame, LINUX_EFAULT);
            }
            // SAFETY: The four-byte user word was validated above.
            let observed = unsafe { core::ptr::read(uaddr as *const u32) } as u64;
            if observed != u64::from(arg5) {
                return linux_result(frame, LINUX_EAGAIN);
            }
            let result = scheduler::linux_futex_requeue(uaddr, uaddr2, u64::from(value), arg4);
            scheduler::futex_trace(command, uaddr, value, arg4, uaddr2, result);
            linux_result(frame, result)
        }
        5 => {
            // FUTEX_WAKE_OP: wake `value` waiters at `uaddr` and conditionally
            // `arg4` waiters at `uaddr2`, per the packed algorithm in `arg5`.
            // `arg5` encodes command|compare|operand|comparand exactly as the
            // Linux `FUTEX_OP` macro does: 3-bit command, 4-bit comparison,
            // 12-bit operand, 12-bit comparand.
            if !address_valid(pml4, uaddr) || !address_valid(pml4, uaddr2) {
                return linux_result(frame, LINUX_EFAULT);
            }
            // SAFETY: The four-byte user word was validated above.
            let old = unsafe { core::ptr::read(uaddr2 as *const u32) };
            let wake_op_cmd = (arg5 >> 28) & 0xf;
            let compare = (arg5 >> 24) & 0xf;
            let operand = (arg5 >> 12) & 0xfff;
            let comparand = arg5 & 0xfff;
            let new_word = match wake_op_cmd {
                0 => operand,                   // FUTEX_OP_SET
                1 => old.wrapping_add(operand), // FUTEX_OP_ADD
                2 => old | operand,             // FUTEX_OP_OR
                3 => !old & operand,            // FUTEX_OP_ANDN
                4 => old ^ operand,             // FUTEX_OP_XOR
                _ => {
                    scheduler::futex_trace(command, uaddr, value, arg4, uaddr2, -22);
                    return linux_result(frame, LINUX_ENOSYS);
                }
            };
            let matched = match compare {
                0 => old == comparand, // FUTEX_OP_CMP_EQ
                1 => old != comparand, // FUTEX_OP_CMP_NE
                2 => old < comparand,  // FUTEX_OP_CMP_LT
                3 => old <= comparand, // FUTEX_OP_CMP_LE
                4 => old > comparand,  // FUTEX_OP_CMP_GT
                5 => old >= comparand, // FUTEX_OP_CMP_GE
                _ => false,
            };
            let store_ok = if new_word != old {
                write_user_u32(pml4, uaddr2, new_word).is_some()
            } else {
                true
            };
            if !store_ok {
                return linux_result(frame, LINUX_EFAULT);
            }
            // Linux always wakes `uaddr`; only the `uaddr2` wake depends on
            // the comparison.
            let first_woken = scheduler::linux_futex_wake(uaddr, u64::from(value));
            let second_woken = if matched {
                scheduler::linux_futex_wake(uaddr2, arg4)
            } else {
                0
            };
            scheduler::futex_trace(
                command,
                uaddr,
                value,
                arg4,
                uaddr2,
                first_woken + second_woken,
            );
            linux_result(frame, first_woken + second_woken)
        }
        _ => {
            // PI operations (6/7/11/12) and all others stay unsupported; the
            // bounded trace shows whether a real thread ever needs them.
            scheduler::futex_trace(command, uaddr, value, arg4, uaddr2, -38);
            linux_result(frame, LINUX_ENOSYS)
        }
    }
}

/// Validates a four-byte futex word as writable user memory.
fn address_valid(pml4: PhysFrame, address: u64) -> bool {
    address.is_multiple_of(4) && linux_user_range_valid(pml4, address, 4, true)
}

const LINUX_ETIMEDOUT: i64 = -110;

fn write_user_u32(pml4: PhysFrame, pointer: u64, value: u32) -> Option<()> {
    write_user_bytes(pml4, pointer, &value.to_le_bytes())
}

fn linux_clock_gettime(pml4: PhysFrame, pointer: u64) -> i64 {
    let ticks = scheduler::ticks();
    let seconds = (ticks / 100) as i64;
    let nanos = ((ticks % 100) * 10_000_000) as i64;
    let mut buffer = [0u8; 16];
    buffer[..8].copy_from_slice(&seconds.to_le_bytes());
    buffer[8..].copy_from_slice(&nanos.to_le_bytes());
    write_user_bytes(pml4, pointer, &buffer).map_or(LINUX_EFAULT, |()| 0)
}

const NANOS_PER_TICK: u64 = 10_000_000;

/// Reads and validates a `timespec`. Negative values and out-of-range
/// nanoseconds are rejected rather than normalized.
fn read_timespec(pml4: PhysFrame, pointer: u64) -> Option<(u64, u64)> {
    let raw = user_bytes(pml4, pointer, 16, false)?;
    let seconds = i64::from_ne_bytes(raw[..8].try_into().ok()?);
    let nanoseconds = i64::from_ne_bytes(raw[8..].try_into().ok()?);
    if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
        return None;
    }
    Some((seconds as u64, nanoseconds as u64))
}

fn timespec_ticks(seconds: u64, nanoseconds: u64) -> Option<u64> {
    seconds
        .checked_mul(100)?
        .checked_add(nanoseconds.div_ceil(NANOS_PER_TICK))
}

fn write_timespec(pml4: PhysFrame, pointer: u64, ticks: u64) -> Option<()> {
    let seconds = ticks / 100;
    let nanos = (ticks % 100) * NANOS_PER_TICK;
    let mut buffer = [0u8; 16];
    buffer[..8].copy_from_slice(&(seconds as i64).to_le_bytes());
    buffer[8..].copy_from_slice(&(nanos as i64).to_le_bytes());
    write_user_bytes(pml4, pointer, &buffer)
}

fn write_itimerspec(pml4: PhysFrame, pointer: u64, interval: u64, remaining: u64) -> Option<()> {
    let mut buffer = [0u8; 32];
    let interval_seconds = interval / 100;
    let interval_nanos = (interval % 100) * NANOS_PER_TICK;
    let remaining_seconds = remaining / 100;
    let remaining_nanos = (remaining % 100) * NANOS_PER_TICK;
    buffer[..8].copy_from_slice(&(interval_seconds as i64).to_le_bytes());
    buffer[8..16].copy_from_slice(&(interval_nanos as i64).to_le_bytes());
    buffer[16..24].copy_from_slice(&(remaining_seconds as i64).to_le_bytes());
    buffer[24..].copy_from_slice(&(remaining_nanos as i64).to_le_bytes());
    write_user_bytes(pml4, pointer, &buffer)
}

fn linux_nanosleep(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    request_pointer: u64,
    remaining_pointer: u64,
) -> *mut InterruptFrame {
    linux_sleep_common(frame, pml4, 35, 0, request_pointer, remaining_pointer)
}

fn linux_clock_nanosleep(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    clockid: u64,
    flags: u64,
    request_pointer: u64,
    remaining_pointer: u64,
) -> *mut InterruptFrame {
    if clockid > 1 || flags & !1 != 0 {
        return linux_result(frame, LINUX_EINVAL);
    }
    linux_sleep_common(
        frame,
        pml4,
        230,
        flags & 1,
        request_pointer,
        remaining_pointer,
    )
}

/// Shared relative/absolute sleep. `TIMER_ABSTIME` selects an absolute
/// monotonic deadline. A deliverable pending signal returns `EINTR`; the
/// relative `nanosleep` form also reports the remaining time.
fn linux_sleep_common(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    syscall: u64,
    flags: u64,
    request_pointer: u64,
    remaining_pointer: u64,
) -> *mut InterruptFrame {
    let absolute = flags & 1 != 0;
    let deadline = match scheduler::linux_take_restart(syscall) {
        Some(Some(deadline)) => deadline,
        Some(None) => scheduler::ticks(),
        None => {
            let Some((seconds, nanoseconds)) = read_timespec(pml4, request_pointer) else {
                return linux_result(frame, LINUX_EINVAL);
            };
            let Some(ticks) = timespec_ticks(seconds, nanoseconds) else {
                return linux_result(frame, LINUX_EINVAL);
            };
            if absolute {
                ticks
            } else {
                scheduler::ticks().saturating_add(ticks)
            }
        }
    };
    if remaining_pointer != 0 && !linux_user_range_valid(pml4, remaining_pointer, 16, true) {
        return linux_result(frame, LINUX_EFAULT);
    }
    let now = scheduler::ticks();
    if scheduler::linux_signal_pending() {
        if remaining_pointer != 0 && syscall == 35 {
            let _ = write_timespec(pml4, remaining_pointer, deadline.saturating_sub(now));
        }
        return linux_result(frame, LINUX_EINTR);
    }
    if now >= deadline {
        return linux_result(frame, 0);
    }
    scheduler::linux_park(frame, syscall, scheduler::LinuxWait::Sleep, Some(deadline))
}

fn linux_timerfd_settime(
    pml4: PhysFrame,
    fd: u64,
    flags: u64,
    new_pointer: u64,
    old_pointer: u64,
) -> i64 {
    if flags & !1 != 0 {
        return LINUX_EINVAL;
    }
    let Some(raw) = user_bytes(pml4, new_pointer, 32, false) else {
        return LINUX_EFAULT;
    };
    let read = |offset: usize| -> Option<(u64, u64)> {
        let seconds = i64::from_ne_bytes(raw[offset..offset + 8].try_into().ok()?);
        let nanoseconds = i64::from_ne_bytes(raw[offset + 8..offset + 16].try_into().ok()?);
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return None;
        }
        Some((seconds as u64, nanoseconds as u64))
    };
    let Some((interval_seconds, interval_nanos)) = read(0) else {
        return LINUX_EINVAL;
    };
    let Some((value_seconds, value_nanos)) = read(16) else {
        return LINUX_EINVAL;
    };
    let Some(interval) = timespec_ticks(interval_seconds, interval_nanos) else {
        return LINUX_EINVAL;
    };
    let Some(value) = timespec_ticks(value_seconds, value_nanos) else {
        return LINUX_EINVAL;
    };
    if old_pointer != 0 && !linux_user_range_valid(pml4, old_pointer, 32, true) {
        return LINUX_EFAULT;
    }
    let deadline = if value_seconds == 0 && value_nanos == 0 {
        None
    } else if flags & 1 != 0 {
        Some(value)
    } else {
        Some(scheduler::ticks().saturating_add(value.max(1)))
    };
    match scheduler::linux_timerfd_settime(fd, deadline, interval) {
        Ok((remaining, previous_interval)) => {
            if old_pointer != 0
                && write_itimerspec(pml4, old_pointer, previous_interval, remaining).is_none()
            {
                return LINUX_EFAULT;
            }
            0
        }
        Err(error) => error,
    }
}

fn linux_timerfd_gettime(pml4: PhysFrame, fd: u64, current_pointer: u64) -> i64 {
    if !linux_user_range_valid(pml4, current_pointer, 32, true) {
        return LINUX_EFAULT;
    }
    match scheduler::linux_timerfd_gettime(fd) {
        Ok((remaining, interval)) => write_itimerspec(pml4, current_pointer, interval, remaining)
            .map_or(LINUX_EFAULT, |()| 0),
        Err(error) => error,
    }
}

fn linux_getrandom(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    if length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    let mut buffer = [0u8; MAX_SYSCALL_IO];
    crate::random::fill(&mut buffer[..length as usize]);
    write_user_bytes(pml4, pointer, &buffer[..length as usize])
        .map_or(LINUX_EFAULT, |()| length as i64)
}

fn linux_uname(pml4: PhysFrame, pointer: u64) -> i64 {
    let mut buffer = [0u8; 390];
    let fields = [
        (0, b"Linux".as_slice()),
        (65, b"aether".as_slice()),
        (130, b"0.1.0-aether".as_slice()),
        (195, b"#1".as_slice()),
        (260, b"x86_64".as_slice()),
        (325, b"".as_slice()),
    ];
    for (offset, value) in fields {
        buffer[offset..offset + value.len()].copy_from_slice(value);
    }
    write_user_bytes(pml4, pointer, &buffer).map_or(LINUX_EFAULT, |()| 0)
}

fn write_user_bytes(pml4: PhysFrame, pointer: u64, bytes: &[u8]) -> Option<()> {
    // SAFETY: `user_bytes_mut` validates the range and the syscall path runs
    // with interrupts masked, so no concurrent user access can alias it.
    let destination = unsafe { user_bytes_mut(pml4, pointer, bytes.len() as u64)? };
    destination.copy_from_slice(bytes);
    Some(())
}

fn write_user_u64(pml4: PhysFrame, pointer: u64) -> Option<()> {
    write_user_bytes(pml4, pointer, &scheduler::linux_fs_base().to_le_bytes())
}

fn linux_write(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    descriptor: u64,
    pointer: u64,
    length: u64,
) -> *mut InterruptFrame {
    let _ = scheduler::linux_take_restart(1);
    // Short write: never copy more than one bounce buffer, even when the
    // caller's iovec or buffer is larger.
    let length = length.min(MAX_SYSCALL_IO as u64);
    let Some(bytes) = user_bytes(pml4, pointer, length, false) else {
        return linux_result(frame, LINUX_EFAULT);
    };
    if descriptor == 1 || descriptor == 2 {
        serial::write_bytes(bytes);
        return linux_result(frame, length as i64);
    }
    if scheduler::linux_display_fd(descriptor) {
        return linux_result(frame, crate::display::write_request(pml4, bytes));
    }
    match scheduler::linux_write_file(descriptor, bytes) {
        Ok(count) => linux_result(frame, count as i64),
        Err(LINUX_EAGAIN) => {
            let Some((index, true)) = scheduler::linux_pipe_end(descriptor) else {
                return linux_result(frame, LINUX_EAGAIN);
            };
            if scheduler::linux_pipe_readers(index) == 0 {
                return linux_result(frame, LINUX_EPIPE);
            }
            if scheduler::linux_signal_pending() {
                return linux_result(frame, LINUX_EINTR);
            }
            scheduler::linux_park(frame, 1, scheduler::LinuxWait::PipeWrite(index), None)
        }
        Err(error) => linux_result(frame, error),
    }
}

fn write_console(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64) -> i64 {
    if descriptor != 1 {
        return errno::EBADF;
    }
    if length as usize > MAX_SYSCALL_IO {
        return errno::EINVAL;
    }
    let Some(bytes) = user_bytes(pml4, pointer, length, false) else {
        return errno::EINVAL;
    };
    serial::write_bytes(bytes);
    length as i64
}

/// Decodes a capability handle passed through a register.
///
/// Handle 0 is reserved (generations start at 1), and bytes above the
/// 24-bit encoding are rejected so a caller cannot smuggle extra bits.
fn decode_ipc_handle(value: u64) -> Option<aether_ipc::CapHandle> {
    if value == 0 || value > 0x00ff_ffff {
        return None;
    }
    let index = (value & 0xff) as u8;
    let generation = (value >> 8) as u16;
    Some(aether_ipc::CapHandle { index, generation })
}

/// Returns `true` when two byte ranges overlap.
fn ranges_overlap(a: u64, a_len: u64, b: u64, b_len: u64) -> bool {
    let a_end = a.saturating_add(a_len);
    let b_end = b.saturating_add(b_len);
    a < b_end && b < a_end
}

/// `EndpointSend`: sends a message through the caller's endpoint capability.
///
/// The sender identity is the kernel's current task id. The payload and the
/// optional transferred handle are validated before the queue is touched, so
/// a malformed message never reaches any endpoint's queue.
fn endpoint_send(pml4: PhysFrame, handle: u64, pointer: u64, length: u64, transfer: u64) -> i64 {
    let Some(cap) = decode_ipc_handle(handle) else {
        return ipc_denied_usage();
    };
    if length as usize > MAX_MESSAGE_BYTES {
        return ipc_denied_usage();
    }
    let Some(payload) = user_bytes(pml4, pointer, length, false) else {
        return ipc_denied_usage();
    };
    let transfer = if transfer == 0 {
        None
    } else {
        match decode_ipc_handle(transfer) {
            Some(handle) => Some(handle),
            None => {
                return ipc_denied_usage();
            }
        }
    };
    // Derive the sender identity inside the same guard that borrows the
    // caller's table, so the two can never describe different tasks even if
    // the gate is ever made preemptible.
    let result = scheduler::with_current_ipc(|current, caps| {
        crate::ipc::send(caps, cap, current as u16, payload, transfer)
    });
    match result {
        Ok(()) => {
            IPC_SENDS.fetch_add(1, Ordering::Release);
            0
        }
        Err(error) => ipc_denied(error),
    }
}

/// `EndpointCreate`: creates a bounded endpoint for the caller.
///
/// The endpoint pool is fixed at boot (8 objects), so repeated creation
/// fails with `ENOSPC` instead of growing kernel memory. This is the same
/// bound the kernel's own boot self-tests exercise.
fn endpoint_create() -> i64 {
    let full =
        aether_ipc::Rights::SEND | aether_ipc::Rights::RECEIVE | aether_ipc::Rights::TRANSFER;
    let result = scheduler::with_current_ipc(|_current, caps| {
        // Refuse before allocating the endpoint when the caller cannot name
        // it. Otherwise a full table would consume a pool slot with no
        // capability to use or revoke it.
        if !caps.has_free_slot() {
            return Err(errno::ENOSPC);
        }
        match crate::ipc::create_endpoint(_current as u16) {
            None => Err(errno::ENOSPC),
            Some(object) => caps
                .grant(object, aether_ipc::ObjectType::Endpoint, full)
                .map(|handle| (u64::from(handle.generation) << 8) | u64::from(handle.index))
                .map_err(|_| errno::ENOSPC),
        }
    });
    match result {
        Ok(handle) => handle as i64,
        Err(code) => {
            IPC_DENIED.fetch_add(1, Ordering::Release);
            code
        }
    }
}

fn ipc_denied_usage() -> i64 {
    IPC_DENIED.fetch_add(1, Ordering::Release);
    errno::EINVAL
}

/// `EndpointReceive`: receives a message through the caller's endpoint.
///
/// The buffer must be able to hold a maximum-size message so a receive is
/// never partial, and the transferred-handle field must be a writable user
/// range disjoint from the payload buffer. The transferred capability, if
/// any, is installed in the caller's table by the endpoint before the queue
/// slot is released, so it is never consumed without the table entry
/// staying visible to the caller.
///
/// With `timeout` set, the call is a blocking receive: an empty queue yields
/// the CPU and retries on the next timer wakeup until a message arrives or the
/// tick deadline passes. Interrupts are re-enabled only around the wait, and
/// the syscall gate is restored to its masked state before returning.
fn endpoint_receive(
    pml4: PhysFrame,
    handle: u64,
    pointer: u64,
    length: u64,
    out: u64,
    timeout: Option<u64>,
) -> i64 {
    let Some(cap) = decode_ipc_handle(handle) else {
        return ipc_denied_usage();
    };
    if (length as usize) < MAX_MESSAGE_BYTES {
        return ipc_denied_usage();
    }
    if out == 0 || out >= USER_LIMIT || !linux_user_range_valid(pml4, out, 4, true) {
        return ipc_denied_usage();
    }
    if ranges_overlap(pointer, length, out, 4) {
        return ipc_denied_usage();
    }

    let deadline =
        timeout.and_then(|ticks| (ticks != 0).then(|| scheduler::ticks().saturating_add(ticks)));

    loop {
        match receive_once(pml4, cap, pointer, out) {
            Ok(received_len) => {
                IPC_RECEIVES.fetch_add(1, Ordering::Release);
                return received_len as i64;
            }
            Err(IpcError::Empty) if timeout.is_some() => {
                // A finite timeout has expired, and an unbounded wait (0) is
                // only cancelled by a message or task retirement.
                if deadline.is_some_and(|deadline| scheduler::ticks() >= deadline) {
                    IPC_TIMEOUTS.fetch_add(1, Ordering::Release);
                    return errno::ETIMEDOUT;
                }
                IPC_BLOCKS.fetch_add(1, Ordering::Release);
                // Let the timer and every other task run. The gate entered
                // with interrupts masked; restore that state before return so
                // the syscall epilogue is not preempted.
                timer::enable_interrupts();
                timer::halt_until_interrupt();
                timer::disable_interrupts();
            }
            Err(error) => return ipc_denied(error),
        }
    }
}

/// One validated receive attempt for [`endpoint_receive`].
fn receive_once(
    pml4: PhysFrame,
    cap: aether_ipc::CapHandle,
    pointer: u64,
    out: u64,
) -> Result<usize, IpcError> {
    let Some(buffer) = (unsafe {
        // SAFETY: The range was validated by the caller, including the user
        // bit and write permission.
        user_bytes_mut(pml4, pointer, MAX_MESSAGE_BYTES as u64)
    }) else {
        return Err(IpcError::MessageTooLong);
    };
    let mut received_len = 0usize;
    scheduler::with_current_ipc(|_current, caps| match crate::ipc::receive(caps, cap) {
        Ok(message) => {
            let payload = message.payload();
            let transferred = match message.transferred {
                Some(transferred) => {
                    (u32::from(transferred.generation) << 8) | u32::from(transferred.index)
                }
                None => 0,
            };
            buffer[..payload.len()].copy_from_slice(payload);
            for byte in buffer[payload.len()..].iter_mut() {
                *byte = 0;
            }
            // SAFETY: `out` was validated as a four-byte writable user range
            // and is disjoint from the payload buffer; the byte-wise copy
            // avoids alignment requirements, and interrupts are masked so no
            // context switch can occur inside the store.
            unsafe {
                let bytes = transferred.to_ne_bytes();
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), out as *mut u8, bytes.len());
            }
            received_len = payload.len();
            Ok(())
        }
        Err(error) => Err(error),
    })?;
    Ok(received_len)
}

/// `EndpointClose`: revokes the caller's aliases and destroys an owned object.
fn endpoint_close(handle: u64) -> i64 {
    let Some(cap) = decode_ipc_handle(handle) else {
        return ipc_denied_usage();
    };
    let result =
        scheduler::with_current_ipc(|current, caps| crate::ipc::close(caps, cap, current as u16));
    match result {
        Ok(()) => {
            IPC_CLOSES.fetch_add(1, Ordering::Release);
            0
        }
        Err(error) => ipc_denied(error),
    }
}

/// Returns a validated read-only user slice.
///
/// # Safety
///
/// The returned slice aliases user memory. The caller must not hold it across a
/// context switch and must not use it after the task's address space changes.
pub fn user_bytes<'a>(pml4: PhysFrame, pointer: u64, length: u64, write: bool) -> Option<&'a [u8]> {
    if length as usize > MAX_SYSCALL_IO {
        return None;
    }
    if pointer >= USER_LIMIT {
        return None;
    }
    if !linux_user_range_valid(pml4, pointer, length, write) {
        return None;
    }
    if length == 0 {
        return Some(&[]);
    }
    // SAFETY: The range lies inside the caller's user window and every page
    // was validated as present (and writable when required). The kernel runs
    // with interrupts masked by the syscall gate, so the mapping cannot change
    // underneath the copy.
    Some(unsafe { core::slice::from_raw_parts(pointer as *const u8, length as usize) })
}

/// Returns a validated mutable user slice with the same lifetime caveats as
/// [`user_bytes`].
///
/// # Safety
///
/// The caller must ensure the task does not concurrently access the range.
pub unsafe fn user_bytes_mut<'a>(
    pml4: PhysFrame,
    pointer: u64,
    length: u64,
) -> Option<&'a mut [u8]> {
    if length as usize > MAX_SYSCALL_IO {
        return None;
    }
    if pointer >= USER_LIMIT {
        return None;
    }
    if !linux_user_range_valid(pml4, pointer, length, true) {
        return None;
    }
    if length == 0 {
        return Some(&mut []);
    }
    // SAFETY: The range is inside the user window and every page is present
    // and writable. The caller supplies the aliasing guarantee.
    Some(unsafe { core::slice::from_raw_parts_mut(pointer as *mut u8, length as usize) })
}

fn linux_user_range_valid(pml4: PhysFrame, pointer: u64, length: u64, write: bool) -> bool {
    paging::user_range_valid(pml4, pointer, length, write)
        || scheduler::linux_page_in_user_range(pml4, pointer, length, write)
}

/// Copies a bounded user string after validating its range.
pub fn user_str<'a>(pml4: PhysFrame, pointer: u64, length: u64) -> Option<&'a str> {
    const MAX_PATH: u64 = 256;
    if length == 0 || length > MAX_PATH {
        return None;
    }
    let bytes = user_bytes(pml4, pointer, length, false)?;
    core::str::from_utf8(bytes).ok()
}

// SPDX-License-Identifier: Apache-2.0
//! Linux seccomp: `seccomp(2)`, the seccomp and privilege `prctl` options,
//! and the filter check at every Linux system call entry.
//!
//! A process can only narrow its own system calls, never widen them.
//! Filters stack, are inherited by threads, `fork`, and `execve`, and can
//! never be removed, so Firefox's content-process sandbox runs inside the
//! per-application policy the kernel already applies. The classic BPF
//! programs are checked and run by [`aether_seccomp`].
//!
//! Differences from Linux:
//!
//! - `SECCOMP_RET_KILL_THREAD` ends the whole process, like
//!   `SECCOMP_RET_KILL_PROCESS`. A process with one thread silently gone is
//!   rarely safe to keep running.
//! - There is no tracer and no user-space notifier, so `SECCOMP_RET_TRACE`
//!   and `SECCOMP_RET_USER_NOTIF` fail the call with `ENOSYS`, and
//!   `SECCOMP_FILTER_FLAG_NEW_LISTENER` is refused.
//! - Installing a filter always requires `PR_SET_NO_NEW_PRIVS`; no task has
//!   `CAP_SYS_ADMIN`.

use crate::arch::x86_64::idt::InterruptFrame;
use crate::linux_gap;
use crate::scheduler;
use crate::syscall::copy_from_user;
use aether_memory::frame::PhysFrame;
use aether_seccomp::{Action, Data, Instruction};
use alloc::rc::Rc;
use alloc::vec::Vec;

const EFAULT: i64 = -14;
const EACCES: i64 = -13;
const ENOMEM: i64 = -12;
const EINVAL: i64 = -22;
const ENOSYS: i64 = -38;
const EOPNOTSUPP: i64 = -95;
/// Linux `MAX_ERRNO`: larger `SECCOMP_RET_ERRNO` data is clamped.
const MAX_ERRNO: u16 = 4095;

const SECCOMP_SET_MODE_STRICT: u64 = 0;
const SECCOMP_SET_MODE_FILTER: u64 = 1;
const SECCOMP_GET_ACTION_AVAIL: u64 = 2;
const FLAG_TSYNC: u64 = 1;
const FLAG_LOG: u64 = 2;
const FLAG_SPEC_ALLOW: u64 = 4;
const FLAG_TSYNC_ESRCH: u64 = 16;

const PR_GET_DUMPABLE: u64 = 3;
const PR_SET_DUMPABLE: u64 = 4;
const PR_SET_NAME: u64 = 15;
const PR_GET_NAME: u64 = 16;
const PR_GET_SECCOMP: u64 = 21;
const PR_SET_SECCOMP: u64 = 22;
const PR_SET_NO_NEW_PRIVS: u64 = 38;
const PR_GET_NO_NEW_PRIVS: u64 = 39;

/// Linux `MAX_INSNS_PER_PATH`: instructions a call may run across the whole
/// chain, counting a 4-instruction penalty per filter.
const MAX_PATH_INSTRUCTIONS: usize = 32_768;
const FILTER_PENALTY: usize = 4;

/// The system calls strict mode allows: read, write, rt_sigreturn, exit.
const STRICT_CALLS: [u64; 4] = [0, 1, 15, 60];
const SIGKILL: usize = 9;
const SIGSYS: usize = 31;

/// One installed filter and the chain it was stacked on.
pub struct Filter {
    program: Vec<Instruction>,
    previous: Option<Rc<Filter>>,
    /// Instructions along this chain, with the per-filter penalty.
    path: usize,
}

impl Filter {
    /// Runs every filter in the chain, newest first, and returns the
    /// result with the highest precedence.
    fn evaluate(&self, data: &[u8; aether_seccomp::DATA_BYTES]) -> u32 {
        let mut result = aether_seccomp::RET_ALLOW;
        let mut node = Some(self);
        while let Some(filter) = node {
            result = aether_seccomp::combine(result, aether_seccomp::run(&filter.program, data));
            node = filter.previous.as_deref();
        }
        result
    }

    /// Whether `ancestor` is this filter or one it was stacked on.
    pub fn descends_from(&self, ancestor: &Rc<Filter>) -> bool {
        let mut node = Some(self);
        while let Some(filter) = node {
            if core::ptr::eq(filter, Rc::as_ptr(ancestor)) {
                return true;
            }
            node = filter.previous.as_deref();
        }
        false
    }
}

/// Applies the caller's seccomp state to the system call in `frame`.
/// Returns `None` when the call may run, or the frame to resume with when
/// seccomp decided it.
pub fn check(frame: *mut InterruptFrame, number: u64) -> Option<*mut InterruptFrame> {
    let result = scheduler::linux_with_seccomp(|filter, strict| {
        if strict {
            return Some(if STRICT_CALLS.contains(&number) {
                aether_seccomp::RET_ALLOW
            } else {
                aether_seccomp::RET_KILL_PROCESS
            });
        }
        let filter = filter?;
        // SAFETY: The syscall entry passes a live frame for this task.
        let data = unsafe {
            Data {
                nr: number as i32,
                arch: aether_seccomp::AUDIT_ARCH_X86_64,
                instruction_pointer: (*frame).rip,
                args: [
                    (*frame).rdi,
                    (*frame).rsi,
                    (*frame).rdx,
                    (*frame).r10,
                    (*frame).r8,
                    (*frame).r9,
                ],
            }
        };
        Some(filter.evaluate(&data.to_bytes()))
    })?;
    let strict = scheduler::linux_with_seccomp(|_, strict| strict);
    match Action::from_result(result) {
        Action::Allow | Action::Log => None,
        Action::Errno(errno) => {
            let errno = i64::from(errno.min(MAX_ERRNO));
            Some(crate::syscall::linux_result(frame, -errno))
        }
        Action::Trap(data) => Some(scheduler::linux_seccomp_trap(frame, number, data)),
        Action::Trace(_) | Action::UserNotif => Some(crate::syscall::linux_result(frame, ENOSYS)),
        Action::KillProcess | Action::KillThread => Some(scheduler::linux_seccomp_kill(
            frame,
            if strict { SIGKILL } else { SIGSYS },
        )),
    }
}

/// `seccomp(operation, flags, args)`.
pub fn sys_seccomp(pml4: PhysFrame, operation: u64, flags: u64, args: u64) -> i64 {
    match operation {
        SECCOMP_SET_MODE_STRICT => {
            if flags != 0 || args != 0 {
                return EINVAL;
            }
            scheduler::linux_seccomp_set_strict().map_or_else(|error| error, |()| 0)
        }
        SECCOMP_SET_MODE_FILTER => set_filter(pml4, flags, args),
        SECCOMP_GET_ACTION_AVAIL => {
            if flags != 0 {
                return EINVAL;
            }
            let mut raw = [0u8; 4];
            if copy_from_user(pml4, args, &mut raw).is_none() {
                return EFAULT;
            }
            match u32::from_ne_bytes(raw) {
                aether_seccomp::RET_KILL_PROCESS
                | aether_seccomp::RET_KILL_THREAD
                | aether_seccomp::RET_TRAP
                | aether_seccomp::RET_ERRNO
                | aether_seccomp::RET_LOG
                | aether_seccomp::RET_ALLOW => 0,
                _ => EOPNOTSUPP,
            }
        }
        // SECCOMP_GET_NOTIF_SIZES and anything newer: no notifier exists.
        _ => EINVAL,
    }
}

fn set_filter(pml4: PhysFrame, flags: u64, args: u64) -> i64 {
    if flags & !(FLAG_TSYNC | FLAG_LOG | FLAG_SPEC_ALLOW | FLAG_TSYNC_ESRCH) != 0 {
        return EINVAL;
    }
    if scheduler::linux_with_seccomp(|_, strict| strict) {
        return EINVAL;
    }
    if !scheduler::linux_no_new_privs() {
        return EACCES;
    }
    // struct sock_fprog { unsigned short len; struct sock_filter *filter; }
    let mut header = [0u8; 16];
    if copy_from_user(pml4, args, &mut header).is_none() {
        return EFAULT;
    }
    let len = usize::from(u16::from_ne_bytes([header[0], header[1]]));
    let pointer = u64::from_ne_bytes(header[8..16].try_into().unwrap_or([0; 8]));
    if len == 0 || len > aether_seccomp::MAX_INSTRUCTIONS {
        return EINVAL;
    }
    if !crate::memory::heap_has_headroom() {
        return ENOMEM;
    }
    let mut raw = Vec::new();
    if raw.try_reserve_exact(len * 8).is_err() {
        return ENOMEM;
    }
    raw.resize(len * 8, 0);
    if copy_from_user(pml4, pointer, &mut raw).is_none() {
        return EFAULT;
    }
    let mut program = Vec::new();
    if program.try_reserve_exact(len).is_err() {
        return ENOMEM;
    }
    program.extend(
        raw.as_chunks::<8>()
            .0
            .iter()
            .map(|bytes| Instruction::from_bytes(*bytes)),
    );
    if aether_seccomp::validate(&program).is_err() {
        return EINVAL;
    }
    let previous = scheduler::linux_seccomp_filter();
    let path = previous.as_ref().map_or(0, |filter| filter.path) + len + FILTER_PENALTY;
    if path > MAX_PATH_INSTRUCTIONS {
        return ENOMEM;
    }
    let filter = Rc::new(Filter {
        program,
        previous,
        path,
    });
    let sync = flags & FLAG_TSYNC != 0;
    let esrch = flags & FLAG_TSYNC_ESRCH != 0;
    match scheduler::linux_seccomp_install(filter, sync, esrch) {
        Ok(()) => 0,
        // A positive result names the thread that blocked TSYNC.
        Err(result) => result,
    }
}

/// The `prctl` options this layer implements; any other option fails with
/// `EINVAL`.
pub fn sys_prctl(pml4: PhysFrame, option: u64, args: [u64; 4]) -> i64 {
    match option {
        PR_SET_NO_NEW_PRIVS => {
            if args != [1, 0, 0, 0] {
                return EINVAL;
            }
            scheduler::linux_set_no_new_privs();
            0
        }
        PR_GET_NO_NEW_PRIVS => {
            if args != [0; 4] {
                return EINVAL;
            }
            i64::from(scheduler::linux_no_new_privs())
        }
        PR_GET_SECCOMP => scheduler::linux_with_seccomp(|filter, strict| {
            if strict {
                1
            } else if filter.is_some() {
                2
            } else {
                0
            }
        }),
        PR_SET_SECCOMP => match args[0] {
            1 => sys_seccomp(pml4, SECCOMP_SET_MODE_STRICT, 0, 0),
            2 => sys_seccomp(pml4, SECCOMP_SET_MODE_FILTER, 0, args[1]),
            _ => EINVAL,
        },
        PR_GET_DUMPABLE => i64::from(scheduler::linux_dumpable()),
        PR_SET_DUMPABLE => match args[0] {
            0 | 1 => {
                scheduler::linux_set_dumpable(args[0] == 1);
                0
            }
            _ => EINVAL,
        },
        PR_SET_NAME => {
            let mut name = [0u8; 16];
            let mut len = 0;
            while len < 15 {
                let mut byte = [0u8; 1];
                if copy_from_user(pml4, args[0] + len as u64, &mut byte).is_none() {
                    return EFAULT;
                }
                if byte[0] == 0 {
                    break;
                }
                name[len] = byte[0];
                len += 1;
            }
            scheduler::linux_set_task_name(&name[..len]);
            0
        }
        PR_GET_NAME => {
            let name = scheduler::linux_task_name();
            crate::syscall::copy_to_user(pml4, args[0], &name).map_or(EFAULT, |()| 0)
        }
        _ => {
            linux_gap::record(
                157,
                linux_gap::Tag::PrctlOption,
                option as u32,
                args[0] as u32,
            );
            EINVAL
        }
    }
}

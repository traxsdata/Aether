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
use crate::memory::paging;
use crate::scheduler;
use crate::serial_line;
use aether_abi::user::{MAX_SYSCALL_IO, Syscall, USER_LIMIT, USER_STACK_TOP, errno};
use aether_ipc::{IpcError, MAX_MESSAGE_BYTES};
use aether_memory::frame::PhysFrame;
use core::sync::atomic::{AtomicUsize, Ordering};

static IPC_SENDS: AtomicUsize = AtomicUsize::new(0);
static IPC_RECEIVES: AtomicUsize = AtomicUsize::new(0);
static IPC_DENIED: AtomicUsize = AtomicUsize::new(0);

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
        Syscall::Read => crate::fs::sys_read(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::WriteFile => crate::fs::sys_write(pml4, rdi, rsi, rdx).unwrap_or(errno::EINVAL),
        Syscall::Close => crate::fs::sys_close(rdi).unwrap_or(errno::EBADF),
        Syscall::List => crate::fs::sys_list(pml4, rdi, rsi, rdx, r10).unwrap_or(errno::EINVAL),
        Syscall::Stat => crate::fs::sys_stat(pml4, rdi, rsi).unwrap_or(errno::EINVAL),
        Syscall::EndpointCreate => endpoint_create(),
        Syscall::EndpointSend => endpoint_send(pml4, rdi, rsi, rdx, r10),
        Syscall::EndpointReceive => endpoint_receive(pml4, rdi, rsi, rdx, r10),
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
    let first = unsafe { (*frame).rdi };
    let second = unsafe { (*frame).rsi };
    let third = unsafe { (*frame).rdx };
    let Some(pml4) = scheduler::current_pml4() else {
        serial_line!("AETHER_BOOT_FAILED reason=linux syscall without pml4");
        crate::arch::x86_64::halt()
    };

    let result = match number {
        0 => linux_read(pml4, first, second, third),
        1 => linux_write(pml4, first, second, third),
        3 => scheduler::linux_close(first),
        8 => scheduler::linux_lseek(first, second as i64, third),
        4 | 6 => linux_stat(pml4, first, second),
        7 => linux_poll(pml4, first, second, third),
        5 => linux_fstat(pml4, first, second),
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
        25 => linux_mremap(
            pml4,
            first,
            second,
            third,
            unsafe { (*frame).r10 },
            unsafe { (*frame).r8 },
        ),
        232 => linux_epoll_wait(pml4, first, second, third, unsafe { (*frame).r10 }),
        233 => linux_epoll_ctl(pml4, first, second, third, unsafe { (*frame).r10 }),
        28 => 0,
        32 => scheduler::linux_dup(first).map_or_else(|error| error, |fd| fd as i64),
        33 => scheduler::linux_dup2(first, second).map_or_else(|error| error, |fd| fd as i64),
        39 => scheduler::linux_getpid() as i64,
        109 => linux_setpgid(first, second),
        112 => scheduler::linux_setsid(),
        121 => linux_getpgid(first),
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
        62 => linux_kill(pml4, first, second),
        234 => linux_tgkill(pml4, first, second, third),
        72 => linux_fcntl(first, second, third),
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
        63 => linux_uname(pml4, first),
        102 | 104 | 107 | 108 => 1000,
        158 => linux_arch_prctl(pml4, first, second),
        202 => return linux_futex(frame, pml4, first, second, third),
        218 | 273 | 334 => 0,
        228 => linux_clock_gettime(pml4, second),
        318 => linux_getrandom(pml4, first, second),
        257 => linux_openat(pml4, first, second, third),
        60 | 231 => return scheduler::linux_exit(first),
        _ => LINUX_ENOSYS,
    };
    // SAFETY: The assembly stub passes a live frame on this task's kernel
    // stack and keeps it valid until iretq.
    unsafe { (*frame).rax = result as u64 };
    scheduler::linux_deliver_signal(frame);
    frame
}

fn linux_result(frame: *mut InterruptFrame, result: i64) -> *mut InterruptFrame {
    unsafe { (*frame).rax = result as u64 };
    scheduler::linux_deliver_signal(frame);
    frame
}

const LINUX_EFAULT: i64 = -14;
const LINUX_EINVAL: i64 = -22;
const LINUX_ENOSYS: i64 = -38;
const LINUX_ESRCH: i64 = -3;

const LINUX_ENOMEM: i64 = -12;
const LINUX_ENOENT: i64 = -2;
const LINUX_EACCES: i64 = -13;
const LINUX_EAGAIN: i64 = -11;
const LINUX_EBADF: i64 = -9;
const LINUX_EEXIST: i64 = -17;
const LINUX_E2BIG: i64 = -7;
const LINUX_ENAMETOOLONG: i64 = -36;

fn linux_openat(pml4: PhysFrame, directory: u64, pointer: u64, flags: u64) -> i64 {
    let Some(path) = user_cstr(pml4, pointer, 256) else {
        return LINUX_EFAULT;
    };
    if path.as_bytes().first() != Some(&b'/') {
        return LINUX_ENOENT;
    }
    // Writable paths live in the per-task overlay; the imported root stays
    // read-only.
    const WRITE_FLAGS: u64 = 0x1 | 0x2 | 0x40 | 0x200 | 0x400;
    if flags & WRITE_FLAGS != 0 {
        let create = flags & 0x40 != 0;
        let truncate = flags & 0x200 != 0;
        return scheduler::linux_overlay_open(path, create, truncate)
            .map_or_else(|error| error, |fd| fd as i64);
    }
    let _ = directory;
    scheduler::linux_open_read(path).map_or_else(|error| error, |fd| fd as i64)
}

fn linux_read(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64) -> i64 {
    if length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    // SAFETY: The range is validated and the syscall path is non-preemptible.
    let Some(buffer) = (unsafe { user_bytes_mut(pml4, pointer, length) }) else {
        return LINUX_EFAULT;
    };
    scheduler::linux_read(descriptor, buffer).map_or_else(|error| error, |count| count as i64)
}

fn linux_pread(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64, offset: u64) -> i64 {
    if length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
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
        || shared && !anonymous
    {
        return LINUX_ENOSYS;
    }
    if protections & !7 != 0 || protections & 6 == 6 {
        return LINUX_EINVAL;
    }
    if anonymous {
        // The fd argument is ignored for MAP_ANONYMOUS. glibc passes -1 as a
        // 32-bit value, so do not require the full 64-bit all-ones pattern.
        if offset != 0 {
            return LINUX_EINVAL;
        }
    } else {
        if fd == u64::MAX
            || fd == u64::from(u32::MAX)
            || !offset.is_multiple_of(aether_memory::PAGE_SIZE)
        {
            return LINUX_EINVAL;
        }
        if scheduler::linux_file_len(fd).is_err() {
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
    if anonymous {
        return start as i64;
    }
    // File-backed mappings remain eagerly populated while the kernel copies data.
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
    if !anonymous && let Err(error) = copy_file_range(pml4, fd, offset, start, length) {
        let _ = scheduler::linux_vma_remove(start, length);
        return error;
    }
    let Some(page_flags) = linux_page_flags(protections) else {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_EINVAL;
    };
    if paging::protect_user_range(pml4, start, length, page_flags).is_err() {
        let _ = scheduler::linux_vma_remove(start, length);
        return LINUX_EINVAL;
    }
    start as i64
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
    let Some(length) = length.checked_add(0xfff).map(|value| value & !0xfff) else {
        return LINUX_EINVAL;
    };
    let Some(flags) = linux_page_flags(protections) else {
        return LINUX_EINVAL;
    };
    if let Err(error) = scheduler::linux_vma_protect(address, length, protections as u32) {
        return vma_errno(error);
    }
    match paging::protect_user_range(pml4, address, length, flags) {
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
    _flags: u64,
    _mask: u64,
    statx_pointer: u64,
) -> i64 {
    let mut buffer = [0u8; 256];
    let (size, inode, mode, uid, gid) = if path_pointer == 0 {
        let Ok((size, inode, mode)) = scheduler::linux_stat_fd(directory) else {
            return LINUX_EBADF;
        };
        (size, inode, mode, 1000u32, 1000u32)
    } else {
        let Some(path) = user_cstr(pml4, path_pointer, 256) else {
            return LINUX_EFAULT;
        };
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

fn linux_epoll_wait(
    pml4: PhysFrame,
    epoll_fd: u64,
    events_pointer: u64,
    max_events: u64,
    timeout: u64,
) -> i64 {
    // Only timeout-zero probing is supported; blocking waits need a scheduler
    // wait queue to handle readiness, deadlines, and signal interruption.
    if timeout != 0 {
        return -95;
    }
    if max_events == 0 || max_events > 16 {
        return LINUX_EINVAL;
    }
    let mut events = [scheduler::LinuxEpollEvent { events: 0, data: 0 }; 16];
    let count = match scheduler::linux_epoll_wait(epoll_fd, &mut events[..max_events as usize]) {
        Ok(count) => count,
        Err(error) => return error,
    };
    let mut buffer = [0u8; 16 * 12];
    for (index, event) in events[..count].iter().enumerate() {
        let offset = index * 12;
        buffer[offset..offset + 4].copy_from_slice(&event.events.to_ne_bytes());
        buffer[offset + 4..offset + 12].copy_from_slice(&event.data.to_ne_bytes());
    }
    write_user_bytes(pml4, events_pointer, &buffer[..count * 12])
        .map_or(LINUX_EFAULT, |()| count as i64)
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

fn linux_poll(pml4: PhysFrame, fds_pointer: u64, count: u64, timeout: u64) -> i64 {
    // poll's timeout is signed: nonzero includes both finite and infinite waits.
    if timeout != 0 {
        return -95;
    }
    if count > 16 {
        return LINUX_EINVAL;
    }
    let length = count * 8;
    // SAFETY: The pollfd array is validated as writable user memory.
    let Some(fds) = (unsafe { user_bytes_mut(pml4, fds_pointer, length) }) else {
        return LINUX_EFAULT;
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
    ready
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

fn linux_page_flags(protections: u64) -> Option<aether_memory::paging::PageFlags> {
    if protections & !7 != 0 || protections & 6 == 6 {
        return None;
    }
    let mut flags =
        aether_memory::paging::PageFlags::PRESENT | aether_memory::paging::PageFlags::USER;
    if protections & 2 != 0 {
        flags = flags | aether_memory::paging::PageFlags::WRITABLE;
    }
    if protections & 4 == 0 {
        flags = flags | aether_memory::paging::PageFlags::NO_EXECUTE;
    }
    Some(flags)
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
    if stack_pointer != 0 {
        let Some(raw) = user_bytes(pml4, stack_pointer, 24, false) else {
            return LINUX_EFAULT;
        };
        let pointer = u64::from_ne_bytes(raw[0..8].try_into().unwrap_or([0; 8]));
        let flags = u32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0; 4]));
        let size = u64::from_ne_bytes(raw[16..24].try_into().unwrap_or([0; 8]));
        if flags & 2 != 0 {
            scheduler::linux_set_altstack(0, 0);
        } else {
            scheduler::linux_set_altstack(pointer, size);
        }
    }
    if old_pointer != 0 {
        let mut raw = [0u8; 24];
        if let Some((pointer, size)) = scheduler::linux_altstack() {
            if pointer != 0 {
                raw[0..8].copy_from_slice(&pointer.to_ne_bytes());
                raw[16..24].copy_from_slice(&size.to_ne_bytes());
            } else {
                raw[8..12].copy_from_slice(&2u32.to_ne_bytes());
            }
        } else {
            raw[8..12].copy_from_slice(&2u32.to_ne_bytes());
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
    if pid > usize::MAX as u64 || pgid > usize::MAX as u64 {
        return LINUX_EINVAL;
    }
    scheduler::linux_setpgid(pid as usize, pgid as usize)
}

fn linux_getpgid(pid: u64) -> i64 {
    if pid > usize::MAX as u64 {
        return LINUX_ESRCH;
    }
    scheduler::linux_getpgid(pid as usize).map_or_else(|error| error, |pgid| pgid as i64)
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
        unsafe { (*frame).rax = LINUX_EINVAL as u64 };
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

fn linux_futex(
    frame: *mut InterruptFrame,
    pml4: PhysFrame,
    address: u64,
    operation: u64,
    value: u64,
) -> *mut InterruptFrame {
    match operation & 0x7f {
        0 => {
            if !address.is_multiple_of(4) || !linux_user_range_valid(pml4, address, 4, true) {
                // SAFETY: The frame is live on the current kernel stack.
                unsafe { (*frame).rax = LINUX_EFAULT as u64 };
                return frame;
            }
            // SAFETY: The four-byte user word was validated above.
            let observed = unsafe { core::ptr::read(address as *const u32) } as u64;
            if observed != value {
                unsafe { (*frame).rax = LINUX_EAGAIN as u64 };
                return frame;
            }
            let timeout_pointer = unsafe { (*frame).r10 };
            let timeout_ticks = if timeout_pointer == 0 {
                None
            } else {
                if !linux_user_range_valid(pml4, timeout_pointer, 16, false) {
                    unsafe { (*frame).rax = LINUX_EFAULT as u64 };
                    return frame;
                }
                // SAFETY: The complete timespec was validated as readable.
                let raw = unsafe { core::slice::from_raw_parts(timeout_pointer as *const u8, 16) };
                let seconds = i64::from_ne_bytes(raw[..8].try_into().unwrap_or([0; 8]));
                let nanoseconds = i64::from_ne_bytes(raw[8..].try_into().unwrap_or([0; 8]));
                if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
                    unsafe { (*frame).rax = LINUX_EINVAL as u64 };
                    return frame;
                }
                let Some(ticks) = (seconds as u64)
                    .checked_mul(100)
                    .and_then(|whole| whole.checked_add((nanoseconds as u64).div_ceil(10_000_000)))
                else {
                    unsafe { (*frame).rax = LINUX_EINVAL as u64 };
                    return frame;
                };
                if ticks == 0 {
                    unsafe { (*frame).rax = (-110i64) as u64 };
                    return frame;
                }
                if scheduler::ticks().checked_add(ticks).is_none() {
                    unsafe { (*frame).rax = LINUX_EINVAL as u64 };
                    return frame;
                }
                Some(ticks)
            };
            scheduler::linux_futex_wait(frame, address, timeout_ticks)
        }
        1 => {
            let woken = scheduler::linux_futex_wake(address, value);
            unsafe { (*frame).rax = woken as u64 };
            frame
        }
        _ => {
            unsafe { (*frame).rax = LINUX_ENOSYS as u64 };
            frame
        }
    }
}

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

fn linux_getrandom(pml4: PhysFrame, pointer: u64, length: u64) -> i64 {
    if length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    let mut state = scheduler::ticks()
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(scheduler::current_task() as u64)
        | 1;
    let mut buffer = [0u8; MAX_SYSCALL_IO];
    for byte in buffer.iter_mut().take(length as usize) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
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

fn linux_write(pml4: PhysFrame, descriptor: u64, pointer: u64, length: u64) -> i64 {
    if length as usize > MAX_SYSCALL_IO {
        return LINUX_EINVAL;
    }
    let Some(bytes) = user_bytes(pml4, pointer, length, false) else {
        return LINUX_EFAULT;
    };
    if descriptor == 1 || descriptor == 2 {
        serial::write_bytes(bytes);
        return length as i64;
    }
    scheduler::linux_write_file(descriptor, bytes).map_or_else(|error| error, |count| count as i64)
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
/// The endpoint pool is fixed at boot (4 objects), so repeated creation
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
        match crate::ipc::create_endpoint() {
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
fn endpoint_receive(pml4: PhysFrame, handle: u64, pointer: u64, length: u64, out: u64) -> i64 {
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
    let Some(buffer) = (unsafe {
        // SAFETY: The kernel checks the whole user range, including the user
        // bit and write permission, before returning the slice.
        user_bytes_mut(pml4, pointer, MAX_MESSAGE_BYTES as u64)
    }) else {
        return ipc_denied_usage();
    };
    let mut received_len = 0usize;
    let result =
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
        });
    match result {
        Ok(()) => {
            IPC_RECEIVES.fetch_add(1, Ordering::Release);
            received_len as i64
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

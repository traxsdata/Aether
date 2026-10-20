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
use aether_abi::user::{MAX_SYSCALL_IO, Syscall, USER_LIMIT, errno};
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
        17 => linux_pread(pml4, first, second, third, unsafe { (*frame).r10 }),
        21 => linux_access(pml4, first, second),
        9 => linux_mmap(first, second, third, unsafe { (*frame).r10 }, unsafe {
            (*frame).r8
        }),
        10 => linux_mprotect(pml4, first, second, third),
        11 => 0,
        12 => scheduler::linux_brk(first) as i64,
        39 | 186 => scheduler::current_task() as i64,
        63 => linux_uname(pml4, first),
        102 | 104 | 107 | 108 => 1000,
        158 => linux_arch_prctl(pml4, first, second),
        202 => linux_futex(first, second),
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
    frame
}

const LINUX_EFAULT: i64 = -14;
const LINUX_EINVAL: i64 = -22;
const LINUX_ENOSYS: i64 = -38;

const LINUX_ENOMEM: i64 = -12;
const LINUX_EPERM: i64 = -1;
const LINUX_ENOENT: i64 = -2;
const LINUX_EACCES: i64 = -13;

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
    let bytes = user_bytes(pml4, pointer, maximum, false)?;
    let end = bytes.iter().position(|byte| *byte == 0)?;
    core::str::from_utf8(&bytes[..end]).ok()
}

fn linux_mmap(address: u64, length: u64, protections: u64, flags: u64, fd: u64) -> i64 {
    // Anonymous private memory only. File mappings need a filesystem service
    // and a VMA table, which the layer does not have yet.
    if flags & 0x20 == 0 || flags & 0x10 != 0 || fd != u64::MAX {
        return LINUX_ENOSYS;
    }
    if protections & 4 != 0 {
        // The boot arena is non-executable; do not silently weaken W^X.
        return LINUX_EPERM;
    }
    let _ = address;
    scheduler::linux_mmap(length).map_or(LINUX_ENOMEM, |address| address as i64)
}

fn linux_mprotect(pml4: PhysFrame, address: u64, length: u64, protections: u64) -> i64 {
    let executable = protections & 4 != 0;
    let writable = protections & 2 != 0;
    if executable && writable {
        return LINUX_EINVAL;
    }
    let mut flags =
        aether_memory::paging::PageFlags::PRESENT | aether_memory::paging::PageFlags::USER;
    if writable {
        flags = flags | aether_memory::paging::PageFlags::WRITABLE;
    }
    if !executable {
        flags = flags | aether_memory::paging::PageFlags::NO_EXECUTE;
    }
    match paging::protect_user_range(pml4, address, length, flags) {
        Ok(()) => 0,
        Err(_) => LINUX_EINVAL,
    }
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

fn linux_futex(_address: u64, operation: u64) -> i64 {
    match operation & 0x7f {
        // WAIT and WAKE are single-task no-ops for now. A real implementation
        // needs a wait queue and a timeout service.
        0 | 1 => 0,
        _ => LINUX_ENOSYS,
    }
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
    if out == 0 || out >= USER_LIMIT || !paging::user_range_valid(pml4, out, 4, true) {
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
    if pointer >= USER_LIMIT || !paging::user_range_valid(pml4, pointer, length, write) {
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
    if pointer >= USER_LIMIT || !paging::user_range_valid(pml4, pointer, length, true) {
        return None;
    }
    if length == 0 {
        return Some(&mut []);
    }
    // SAFETY: The range is inside the user window and every page is present
    // and writable. The caller supplies the aliasing guarantee.
    Some(unsafe { core::slice::from_raw_parts_mut(pointer as *mut u8, length as usize) })
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

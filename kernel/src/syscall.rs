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

/// Dispatches one system call and returns the frame to resume.
///
/// Returning a different frame lets `exit` schedule the next task without
/// returning to a dead user context.
pub fn dispatch(frame: *mut InterruptFrame) -> *mut InterruptFrame {
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
    if out == 0 || out >= USER_LIMIT || !paging::user_range_valid(pml4, out, 4, true) {
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

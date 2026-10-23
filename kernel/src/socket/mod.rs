// SPDX-License-Identifier: Apache-2.0
//! Linux socket descriptors.
//!
//! Every Linux socket, local or Internet, is one slot in a global table. A
//! descriptor names the slot with `LinuxFileSource::Socket { index }`, so
//! `dup`, `fork`, and threads share one socket the way Linux shares one open
//! file description. The slot carries a reference count and the
//! `O_NONBLOCK` flag; the protocol state lives in the slot's [`SocketKind`].
//!
//! This module decodes the socket system calls (41 to 55, `accept4`,
//! `recvmmsg`, `sendmmsg`) once and hands each family flat kernel buffers:
//!
//! - `AF_UNIX` goes to [`unix`], which owns local socket pairs and
//!   `SCM_RIGHTS`.
//! - `AF_INET` and `AF_INET6` go to [`inet`], which requires the network
//!   policy described there.
//! - Every other family, `AF_NETLINK` included, fails with `EAFNOSUPPORT`.
//!
//! A blocking call parks the task with the poll wait kind and is restarted
//! from the top; anything that changes socket readiness calls
//! `scheduler::linux_wake_pollers`. All access is serialized because the Linux
//! syscall entry runs with interrupts masked on the single CPU.

// The per-family interface is wider than the stubs use until the Internet
// and local families land; keep it whole instead of trimming it per change.
#![allow(dead_code)]

pub mod inet;
pub mod unix;

use crate::arch::x86_64::idt::InterruptFrame;
use crate::scheduler;
use crate::syscall::{copy_from_user, copy_to_user, linux_result, user_range_writable};
use aether_abi::user::MAX_SYSCALL_IO;
use aether_memory::frame::PhysFrame;
use core::cell::UnsafeCell;

pub const EPERM: i64 = -1;
pub const EINTR: i64 = -4;
pub const EBADF: i64 = -9;
pub const EAGAIN: i64 = -11;
pub const ENOMEM: i64 = -12;
pub const EACCES: i64 = -13;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const EMFILE: i64 = -24;
pub const ENOSYS: i64 = -38;
pub const ENOTSOCK: i64 = -88;
pub const EDESTADDRREQ: i64 = -89;
pub const EMSGSIZE: i64 = -90;
pub const EPROTOTYPE: i64 = -91;
pub const ENOPROTOOPT: i64 = -92;
pub const EPROTONOSUPPORT: i64 = -93;
pub const EOPNOTSUPP: i64 = -95;
pub const EAFNOSUPPORT: i64 = -97;
pub const EADDRINUSE: i64 = -98;
pub const EADDRNOTAVAIL: i64 = -99;
pub const ENETUNREACH: i64 = -101;
pub const ECONNABORTED: i64 = -103;
pub const ECONNRESET: i64 = -104;
pub const EISCONN: i64 = -106;
pub const ENOTCONN: i64 = -107;
pub const ETIMEDOUT: i64 = -110;
pub const ECONNREFUSED: i64 = -111;
pub const EALREADY: i64 = -114;
pub const EINPROGRESS: i64 = -115;
pub const EPIPE: i64 = -32;

pub const AF_UNIX: u16 = 1;
pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;

pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
pub const SOCK_SEQPACKET: u32 = 5;
const SOCK_TYPE_MASK: u64 = 0xf;
const SOCK_NONBLOCK: u64 = 0x800;
const SOCK_CLOEXEC: u64 = 0x8_0000;

pub const MSG_PEEK: u32 = 0x2;
pub const MSG_TRUNC: u32 = 0x20;
pub const MSG_DONTWAIT: u32 = 0x40;
pub const MSG_NOSIGNAL: u32 = 0x4000;
pub const MSG_CTRUNC: u32 = 0x8;

pub const SOL_SOCKET: u32 = 1;
pub const SO_TYPE: u32 = 3;
pub const SO_ERROR: u32 = 4;
pub const SO_DOMAIN: u32 = 39;

pub const POLL_IN: i16 = 1;
pub const POLL_OUT: i16 = 4;
pub const POLL_ERR: i16 = 8;
pub const POLL_HUP: i16 = 0x10;
pub const POLL_NVAL: i16 = 0x20;
pub const POLL_RDHUP: i16 = 0x2000;

/// Global socket slots.
#[cfg(feature = "gate4-gui-test")]
pub const MAX_SOCKETS: usize = 64;
#[cfg(not(feature = "gate4-gui-test"))]
pub const MAX_SOCKETS: usize = 16;
/// Largest `sockaddr` the kernel copies in or out.
pub const MAX_ADDRESS: usize = 128;
/// Largest ancillary-data block the kernel copies in or out.
pub const MAX_CONTROL: usize = 256;
/// Maximum `iovec` entries in one `sendmsg`/`recvmsg`.
const MAX_IOVECS: usize = 16;

/// A raw `sockaddr` as user space passed it, or as a family returns it.
#[derive(Clone, Copy)]
pub struct SockAddr {
    pub bytes: [u8; MAX_ADDRESS],
    pub len: usize,
}

impl SockAddr {
    pub const EMPTY: Self = Self {
        bytes: [0; MAX_ADDRESS],
        len: 0,
    };

    /// The address family, or `None` for an address shorter than its tag.
    #[must_use]
    pub fn family(&self) -> Option<u16> {
        (self.len >= 2).then(|| u16::from_ne_bytes([self.bytes[0], self.bytes[1]]))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Builds an address from bytes a family produced.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut address = Self::EMPTY;
        let len = bytes.len().min(MAX_ADDRESS);
        address.bytes[..len].copy_from_slice(&bytes[..len]);
        address.len = len;
        address
    }
}

/// What a family's `recv` produced.
pub struct Received {
    /// Bytes written to the data buffer.
    pub len: usize,
    /// The datagram's full length when it was cut short (`MSG_TRUNC`).
    pub full_len: usize,
    /// Sender address, for datagram families.
    pub from: Option<SockAddr>,
    /// Bytes written to the control buffer.
    pub control_len: usize,
    /// `MSG_*` flags reported back in `msg_flags`.
    pub flags: u32,
}

impl Received {
    #[must_use]
    pub const fn data(len: usize) -> Self {
        Self {
            len,
            full_len: len,
            from: None,
            control_len: 0,
            flags: 0,
        }
    }
}

/// One outgoing message handed to a family.
pub struct Outgoing<'a> {
    pub data: &'a [u8],
    /// The caller's data did not fit the bounce buffer. A datagram family
    /// must refuse it with `EMSGSIZE`; a stream family sends `data` and
    /// reports the short count.
    pub truncated: bool,
    pub control: &'a [u8],
    pub to: Option<&'a SockAddr>,
    pub flags: u32,
}

/// Protocol state of one socket.
pub enum SocketKind {
    Unix(unix::UnixSocket),
    Inet(inet::InetSocket),
}

struct SocketSlot {
    refs: u32,
    nonblock: bool,
    kind: SocketKind,
}

struct Global<T>(UnsafeCell<T>);

// SAFETY: The table is used only from Linux syscalls, descriptor cleanup on
// exit, and the network poll, all of which run with interrupts masked on the
// single CPU. No reference into the table outlives one of those calls.
unsafe impl<T> Sync for Global<T> {}

static SOCKETS: Global<[Option<SocketSlot>; MAX_SOCKETS]> =
    Global(UnsafeCell::new([const { None }; MAX_SOCKETS]));

fn slots() -> &'static mut [Option<SocketSlot>; MAX_SOCKETS] {
    // SAFETY: See the `Sync` justification on `Global`.
    unsafe { &mut *SOCKETS.0.get() }
}

fn slot(index: usize) -> Result<&'static mut SocketSlot, i64> {
    slots().get_mut(index).and_then(Option::as_mut).ok_or(EBADF)
}

/// Stores a new socket and returns its slot with one reference.
pub fn allocate(kind: SocketKind, nonblock: bool) -> Result<usize, i64> {
    let table = slots();
    let Some(index) = table.iter().position(Option::is_none) else {
        close_kind(kind);
        return Err(ENOMEM);
    };
    table[index] = Some(SocketSlot {
        refs: 1,
        nonblock,
        kind,
    });
    Ok(index)
}

/// Adds a descriptor reference (`dup`, `fork`).
pub fn retain(index: usize) {
    if let Ok(slot) = slot(index) {
        slot.refs = slot.refs.saturating_add(1);
    }
}

/// Drops a descriptor reference and closes the socket with the last one.
pub fn release(index: usize) {
    let table = slots();
    let Some(entry) = table.get_mut(index) else {
        return;
    };
    let Some(slot) = entry.as_mut() else {
        return;
    };
    slot.refs = slot.refs.saturating_sub(1);
    if slot.refs == 0
        && let Some(slot) = entry.take()
    {
        close_kind(slot.kind);
        scheduler::linux_wake_pollers();
    }
}

fn close_kind(kind: SocketKind) {
    match kind {
        SocketKind::Unix(socket) => socket.close(),
        SocketKind::Inet(socket) => socket.close(),
    }
}

/// Whether the socket is in non-blocking mode.
#[must_use]
pub fn nonblocking(index: usize) -> bool {
    slot(index).is_ok_and(|slot| slot.nonblock)
}

/// `fcntl(F_SETFL)` support: sets or clears `O_NONBLOCK`.
pub fn set_nonblocking(index: usize, nonblock: bool) {
    if let Ok(slot) = slot(index) {
        slot.nonblock = nonblock;
    }
}

/// `read(2)` on a socket descriptor.
pub fn read(index: usize, out: &mut [u8]) -> Result<usize, i64> {
    let slot = slot(index)?;
    let mut control = [0u8; 0];
    recv_kind(&mut slot.kind, out, &mut control, 0).map(|received| received.len)
}

/// `write(2)` on a socket descriptor.
pub fn write(index: usize, data: &[u8]) -> Result<usize, i64> {
    let slot = slot(index)?;
    send_kind(
        &mut slot.kind,
        &Outgoing {
            data,
            truncated: false,
            control: &[],
            to: None,
            flags: 0,
        },
    )
}

/// Poll readiness of a socket for `events`.
#[must_use]
pub fn poll(index: usize, events: i16) -> i16 {
    let Ok(slot) = slot(index) else {
        return POLL_NVAL;
    };
    match &mut slot.kind {
        SocketKind::Unix(socket) => socket.poll(events),
        SocketKind::Inet(socket) => socket.poll(events),
    }
}

/// Whether the socket in slot `index` may travel in an `SCM_RIGHTS` message
/// sent through local pair `channel`. A local endpoint may not ride its own
/// pair, or any pair while its own pair holds queued sockets; either would
/// let a reference cycle keep both pairs alive after every descriptor
/// closes.
pub fn passable_socket(index: usize, channel: u64) -> bool {
    match slots().get(index).and_then(Option::as_ref) {
        Some(SocketSlot {
            kind: SocketKind::Unix(socket),
            ..
        }) => socket.channel_id() != channel && !socket.carries_sockets(),
        Some(_) => true,
        None => false,
    }
}

/// Pipe ends queued in local socket messages, as (readers, writers).
pub fn in_flight_pipe_users(index: usize) -> (usize, usize) {
    slots()
        .iter()
        .flatten()
        .filter_map(|slot| match &slot.kind {
            SocketKind::Unix(socket) => Some(socket.queued_pipe_ends(index)),
            SocketKind::Inet(_) => None,
        })
        .fold((0, 0), |total, ends| (total.0 + ends.0, total.1 + ends.1))
}

/// `fstat` inode number for a socket descriptor.
#[must_use]
pub const fn inode(index: usize) -> u64 {
    0x50_0000 + index as u64
}

fn recv_kind(
    kind: &mut SocketKind,
    out: &mut [u8],
    control: &mut [u8],
    flags: u32,
) -> Result<Received, i64> {
    match kind {
        SocketKind::Unix(socket) => socket.recv(out, control, flags),
        SocketKind::Inet(socket) => socket.recv(out, control, flags),
    }
}

fn send_kind(kind: &mut SocketKind, message: &Outgoing<'_>) -> Result<usize, i64> {
    let result = match kind {
        SocketKind::Unix(socket) => socket.send(message),
        SocketKind::Inet(socket) => socket.send(message),
    };
    if result.is_ok() {
        scheduler::linux_wake_pollers();
    }
    result
}

/// Dispatches a socket system call. `number` is one of 41..=55, 288, 299,
/// or 307; the caller has already recorded it for the syscall audit.
pub fn dispatch(frame: *mut InterruptFrame, pml4: PhysFrame, number: u64) -> *mut InterruptFrame {
    // SAFETY: The Linux entry stub passes a live frame for this syscall.
    let (a0, a1, a2, a3, a4, a5) = unsafe {
        (
            (*frame).rdi,
            (*frame).rsi,
            (*frame).rdx,
            (*frame).r10,
            (*frame).r8,
            (*frame).r9,
        )
    };
    let restarted = scheduler::linux_take_restart(number).is_some();
    let outcome = match number {
        41 => Done(socket(a0, a1, a2)),
        42 => connect(pml4, a0, a1, a2, restarted),
        43 => accept(pml4, a0, a1, a2, 0),
        288 => accept(pml4, a0, a1, a2, a3),
        44 => sendto(pml4, a0, a1, a2, a3 as u32, a4, a5),
        45 => recvfrom(pml4, a0, a1, a2, a3 as u32, a4, a5),
        46 => sendmsg(pml4, a0, a1, a2 as u32),
        47 => recvmsg(pml4, a0, a1, a2 as u32),
        48 => Done(with_socket(a0, |kind, _| match kind {
            SocketKind::Unix(socket) => socket.shutdown(a1),
            SocketKind::Inet(socket) => socket.shutdown(a1),
        })),
        49 => Done(bind(pml4, a0, a1, a2)),
        50 => Done(with_socket(a0, |kind, _| match kind {
            SocketKind::Unix(socket) => socket.listen(a1),
            SocketKind::Inet(socket) => socket.listen(a1),
        })),
        51 => Done(socket_name(pml4, a0, a1, a2, false)),
        52 => Done(socket_name(pml4, a0, a1, a2, true)),
        53 => Done(socketpair(pml4, a0, a1, a2, a3)),
        54 => Done(setsockopt(pml4, a0, a1 as u32, a2 as u32, a3, a4)),
        55 => Done(getsockopt(pml4, a0, a1 as u32, a2 as u32, a3, a4)),
        // recvmmsg and sendmmsg: glibc and Firefox fall back to the single
        // message calls.
        _ => {
            crate::linux_gap::record(number, crate::linux_gap::Tag::UnknownSyscall, 0, 0);
            Done(ENOSYS)
        }
    };
    match outcome {
        Done(result) => linux_result(frame, result),
        Block => {
            if scheduler::linux_signal_pending() {
                return linux_result(frame, EINTR);
            }
            scheduler::linux_park(frame, number, scheduler::LinuxWait::Poll, None)
        }
    }
}

/// Result of a socket call: finished, or would block on a blocking socket.
enum Outcome {
    Done(i64),
    Block,
}
use Outcome::{Block, Done};

/// Converts `EAGAIN` into a park unless the socket or call is non-blocking.
fn blocking(result: Result<i64, i64>, nonblock: bool) -> Outcome {
    match result {
        Err(EAGAIN) if !nonblock => Block,
        Ok(value) | Err(value) => Done(value),
    }
}

/// Resolves a descriptor to its socket slot index.
fn socket_index(fd: u64) -> Result<usize, i64> {
    scheduler::linux_socket_index(fd)
}

fn with_socket(fd: u64, f: impl FnOnce(&mut SocketKind, bool) -> Result<(), i64>) -> i64 {
    let result = socket_index(fd)
        .and_then(slot)
        .and_then(|slot| f(&mut slot.kind, slot.nonblock));
    scheduler::linux_wake_pollers();
    result.map_or_else(|error| error, |()| 0)
}

fn read_address(pml4: PhysFrame, pointer: u64, len: u64) -> Result<SockAddr, i64> {
    if len < 2 || len as usize > MAX_ADDRESS {
        return Err(EINVAL);
    }
    let mut address = SockAddr::EMPTY;
    address.len = len as usize;
    copy_from_user(pml4, pointer, &mut address.bytes[..address.len]).ok_or(EFAULT)?;
    Ok(address)
}

/// Writes `address` to a user `sockaddr` with a value-result length.
fn write_address(
    pml4: PhysFrame,
    pointer: u64,
    len_pointer: u64,
    address: &SockAddr,
) -> Result<(), i64> {
    if pointer == 0 || len_pointer == 0 {
        return Ok(());
    }
    let mut raw = [0u8; 4];
    copy_from_user(pml4, len_pointer, &mut raw).ok_or(EFAULT)?;
    let capacity = u32::from_ne_bytes(raw) as usize;
    if capacity > i32::MAX as usize {
        return Err(EINVAL);
    }
    let count = capacity.min(address.len);
    if count != 0 {
        copy_to_user(pml4, pointer, &address.bytes[..count]).ok_or(EFAULT)?;
    }
    copy_to_user(pml4, len_pointer, &(address.len as u32).to_ne_bytes()).ok_or(EFAULT)
}

fn socket(domain: u64, kind: u64, protocol: u64) -> i64 {
    if kind & !(SOCK_TYPE_MASK | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        crate::linux_gap::record(
            41,
            crate::linux_gap::Tag::SocketKind,
            domain as u32,
            kind as u32,
        );
        return EINVAL;
    }
    let nonblock = kind & SOCK_NONBLOCK != 0;
    let socket_type = (kind & SOCK_TYPE_MASK) as u32;
    let created = match u16::try_from(domain).unwrap_or(0) {
        AF_UNIX => unix::UnixSocket::new(socket_type, protocol).map(SocketKind::Unix),
        AF_INET | AF_INET6 => {
            inet::InetSocket::new(domain as u16, socket_type, protocol).map(SocketKind::Inet)
        }
        other => {
            crate::linux_gap::record(
                41,
                crate::linux_gap::Tag::SocketKind,
                other as u32,
                socket_type,
            );
            Err(EAFNOSUPPORT)
        }
    };
    let result = created
        .and_then(|kind| allocate(kind, nonblock))
        .and_then(|index| scheduler::linux_install_socket(index).inspect_err(|_| release(index)));
    result.map_or_else(|error| error, |fd| fd as i64)
}

fn socketpair(pml4: PhysFrame, domain: u64, kind: u64, protocol: u64, pointer: u64) -> i64 {
    if kind & !(SOCK_TYPE_MASK | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return EINVAL;
    }
    if domain != u64::from(AF_UNIX) {
        return if domain == u64::from(AF_INET) || domain == u64::from(AF_INET6) {
            EOPNOTSUPP
        } else {
            EAFNOSUPPORT
        };
    }
    if !user_range_writable(pml4, pointer, 8) {
        return EFAULT;
    }
    let nonblock = kind & SOCK_NONBLOCK != 0;
    let (first, second) = match unix::UnixSocket::pair((kind & SOCK_TYPE_MASK) as u32, protocol) {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    let first = match allocate(SocketKind::Unix(first), nonblock) {
        Ok(index) => index,
        Err(error) => {
            second.close();
            return error;
        }
    };
    let second = match allocate(SocketKind::Unix(second), nonblock) {
        Ok(index) => index,
        Err(error) => {
            release(first);
            return error;
        }
    };
    let a = match scheduler::linux_install_socket(first) {
        Ok(fd) => fd,
        Err(error) => {
            release(first);
            release(second);
            return error;
        }
    };
    let b = match scheduler::linux_install_socket(second) {
        Ok(fd) => fd,
        Err(error) => {
            // Closing the installed descriptor drops the first slot.
            let _ = scheduler::linux_close(a);
            release(second);
            return error;
        }
    };
    let mut buffer = [0u8; 8];
    buffer[..4].copy_from_slice(&(a as u32).to_ne_bytes());
    buffer[4..].copy_from_slice(&(b as u32).to_ne_bytes());
    if copy_to_user(pml4, pointer, &buffer).is_none() {
        // The caller never learns the numbers, so nothing could close them.
        let _ = scheduler::linux_close(a);
        let _ = scheduler::linux_close(b);
        return EFAULT;
    }
    0
}

fn connect(pml4: PhysFrame, fd: u64, pointer: u64, len: u64, restarted: bool) -> Outcome {
    let address = match read_address(pml4, pointer, len) {
        Ok(address) => address,
        Err(error) => return Done(error),
    };
    let slot = match socket_index(fd).and_then(slot) {
        Ok(slot) => slot,
        Err(error) => return Done(error),
    };
    let nonblock = slot.nonblock;
    let result = match &mut slot.kind {
        SocketKind::Unix(socket) => socket.connect(&address, nonblock, restarted),
        SocketKind::Inet(socket) => socket.connect(&address, nonblock, restarted),
    };
    scheduler::linux_wake_pollers();
    blocking(result.map(|()| 0), nonblock)
}

fn bind(pml4: PhysFrame, fd: u64, pointer: u64, len: u64) -> i64 {
    let address = match read_address(pml4, pointer, len) {
        Ok(address) => address,
        Err(error) => return error,
    };
    with_socket(fd, |kind, _| match kind {
        SocketKind::Unix(socket) => socket.bind(&address),
        SocketKind::Inet(socket) => socket.bind(&address),
    })
}

fn accept(pml4: PhysFrame, fd: u64, pointer: u64, len_pointer: u64, flags: u64) -> Outcome {
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Done(EINVAL);
    }
    let slot = match socket_index(fd).and_then(slot) {
        Ok(slot) => slot,
        Err(error) => return Done(error),
    };
    let nonblock = slot.nonblock;
    let accepted = match &mut slot.kind {
        SocketKind::Unix(socket) => socket
            .accept()
            .map(|(peer, address)| (SocketKind::Unix(peer), address)),
        SocketKind::Inet(socket) => socket
            .accept()
            .map(|(peer, address)| (SocketKind::Inet(peer), address)),
    };
    let (kind, address) = match accepted {
        Ok(accepted) => accepted,
        Err(error) => return blocking(Err(error), nonblock),
    };
    let index = match allocate(kind, flags & SOCK_NONBLOCK != 0) {
        Ok(index) => index,
        Err(error) => return Done(error),
    };
    let fd = match scheduler::linux_install_socket(index) {
        Ok(fd) => fd,
        Err(error) => {
            release(index);
            return Done(error);
        }
    };
    if let Err(error) = write_address(pml4, pointer, len_pointer, &address) {
        let _ = scheduler::linux_close(fd);
        return Done(error);
    }
    scheduler::linux_wake_pollers();
    Done(fd as i64)
}

fn sendto(
    pml4: PhysFrame,
    fd: u64,
    pointer: u64,
    len: u64,
    flags: u32,
    address_pointer: u64,
    address_len: u64,
) -> Outcome {
    let to = if address_pointer == 0 {
        None
    } else {
        match read_address(pml4, address_pointer, address_len) {
            Ok(address) => Some(address),
            Err(error) => return Done(error),
        }
    };
    let truncated = len as usize > MAX_SYSCALL_IO;
    let len = (len as usize).min(MAX_SYSCALL_IO);
    let mut storage = [0u8; MAX_SYSCALL_IO];
    if copy_from_user(pml4, pointer, &mut storage[..len]).is_none() {
        return Done(EFAULT);
    }
    send(
        fd,
        &Outgoing {
            data: &storage[..len],
            truncated,
            control: &[],
            to: to.as_ref(),
            flags,
        },
    )
}

fn send(fd: u64, message: &Outgoing<'_>) -> Outcome {
    let slot = match socket_index(fd).and_then(slot) {
        Ok(slot) => slot,
        Err(error) => return Done(error),
    };
    let nonblock = slot.nonblock || message.flags & MSG_DONTWAIT != 0;
    let result = send_kind(&mut slot.kind, message);
    blocking(result.map(|count| count as i64), nonblock)
}

fn recvfrom(
    pml4: PhysFrame,
    fd: u64,
    pointer: u64,
    len: u64,
    flags: u32,
    address_pointer: u64,
    len_pointer: u64,
) -> Outcome {
    let len = (len as usize).min(MAX_SYSCALL_IO);
    if !user_range_writable(pml4, pointer, len as u64) {
        return Done(EFAULT);
    }
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let received = match receive(fd, &mut storage[..len], &mut [], flags) {
        Ok(received) => received,
        Err(outcome) => return outcome,
    };
    if copy_to_user(pml4, pointer, &storage[..received.len]).is_none() {
        return Done(EFAULT);
    }
    if let Some(from) = received.from.as_ref()
        && let Err(error) = write_address(pml4, address_pointer, len_pointer, from)
    {
        return Done(error);
    }
    Done(if flags & MSG_TRUNC != 0 {
        received.full_len as i64
    } else {
        received.len as i64
    })
}

fn receive(fd: u64, out: &mut [u8], control: &mut [u8], flags: u32) -> Result<Received, Outcome> {
    let slot = socket_index(fd).and_then(slot).map_err(Done)?;
    let nonblock = slot.nonblock || flags & MSG_DONTWAIT != 0;
    let result = recv_kind(&mut slot.kind, out, control, flags);
    if result.is_ok() && flags & MSG_PEEK == 0 {
        scheduler::linux_wake_pollers();
    }
    result.map_err(|error| blocking(Err(error), nonblock))
}

/// The fields of a user `msghdr` the kernel uses.
struct MessageHeader {
    name: u64,
    name_len: u32,
    iov: u64,
    iov_len: u64,
    control: u64,
    control_len: u64,
}

fn read_message_header(pml4: PhysFrame, pointer: u64) -> Result<MessageHeader, i64> {
    let mut raw = [0u8; 56];
    copy_from_user(pml4, pointer, &mut raw).ok_or(EFAULT)?;
    let word =
        |offset: usize| u64::from_ne_bytes(raw[offset..offset + 8].try_into().unwrap_or([0; 8]));
    let header = MessageHeader {
        name: word(0),
        name_len: u32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0; 4])),
        iov: word(16),
        iov_len: word(24),
        control: word(32),
        control_len: word(40),
    };
    if header.iov_len as usize > MAX_IOVECS {
        return Err(EMSGSIZE);
    }
    Ok(header)
}

/// Reads up to [`MAX_IOVECS`] `(base, len)` pairs.
fn read_iovecs(
    pml4: PhysFrame,
    header: &MessageHeader,
) -> Result<([(u64, u64); MAX_IOVECS], usize), i64> {
    let count = header.iov_len as usize;
    let mut raw = [0u8; MAX_IOVECS * 16];
    copy_from_user(pml4, header.iov, &mut raw[..count * 16]).ok_or(EFAULT)?;
    let mut iovecs = [(0u64, 0u64); MAX_IOVECS];
    for (index, iovec) in iovecs.iter_mut().take(count).enumerate() {
        let offset = index * 16;
        let base = u64::from_ne_bytes(raw[offset..offset + 8].try_into().unwrap_or([0; 8]));
        let len = u64::from_ne_bytes(raw[offset + 8..offset + 16].try_into().unwrap_or([0; 8]));
        if len > i64::MAX as u64 {
            return Err(EINVAL);
        }
        *iovec = (base, len);
    }
    Ok((iovecs, count))
}

fn sendmsg(pml4: PhysFrame, fd: u64, pointer: u64, flags: u32) -> Outcome {
    let header = match read_message_header(pml4, pointer) {
        Ok(header) => header,
        Err(error) => return Done(error),
    };
    let (iovecs, count) = match read_iovecs(pml4, &header) {
        Ok(iovecs) => iovecs,
        Err(error) => return Done(error),
    };
    // Gather into one bounce buffer; anything past it is a short write for a
    // stream and `EMSGSIZE` for a datagram.
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let mut used = 0usize;
    let mut truncated = false;
    for &(base, len) in &iovecs[..count] {
        let room = MAX_SYSCALL_IO - used;
        let take = (len as usize).min(room);
        if take < len as usize {
            truncated = true;
        }
        if take != 0 && copy_from_user(pml4, base, &mut storage[used..used + take]).is_none() {
            return Done(EFAULT);
        }
        used += take;
    }
    let to = if header.name == 0 || header.name_len == 0 {
        None
    } else {
        match read_address(pml4, header.name, u64::from(header.name_len)) {
            Ok(address) => Some(address),
            Err(error) => return Done(error),
        }
    };
    if header.control_len as usize > MAX_CONTROL {
        return Done(ENOMEM);
    }
    let mut control = [0u8; MAX_CONTROL];
    let control_len = header.control_len as usize;
    if control_len != 0
        && copy_from_user(pml4, header.control, &mut control[..control_len]).is_none()
    {
        return Done(EFAULT);
    }
    send(
        fd,
        &Outgoing {
            data: &storage[..used],
            truncated,
            control: &control[..control_len],
            to: to.as_ref(),
            flags,
        },
    )
}

fn recvmsg(pml4: PhysFrame, fd: u64, pointer: u64, flags: u32) -> Outcome {
    let header = match read_message_header(pml4, pointer) {
        Ok(header) => header,
        Err(error) => return Done(error),
    };
    let (iovecs, count) = match read_iovecs(pml4, &header) {
        Ok(iovecs) => iovecs,
        Err(error) => return Done(error),
    };
    let capacity = iovecs[..count]
        .iter()
        .fold(0usize, |total, &(_, len)| {
            total.saturating_add(len as usize)
        })
        .min(MAX_SYSCALL_IO);
    for &(base, len) in &iovecs[..count] {
        let len = (len as usize).min(MAX_SYSCALL_IO) as u64;
        if len != 0 && !user_range_writable(pml4, base, len) {
            return Done(EFAULT);
        }
    }
    let control_capacity = (header.control_len as usize).min(MAX_CONTROL);
    let mut storage = [0u8; MAX_SYSCALL_IO];
    let mut control = [0u8; MAX_CONTROL];
    let received = match receive(
        fd,
        &mut storage[..capacity],
        &mut control[..control_capacity],
        flags,
    ) {
        Ok(received) => received,
        Err(outcome) => return outcome,
    };
    // Scatter the data back over the caller's iovecs.
    let mut copied = 0usize;
    for &(base, len) in &iovecs[..count] {
        if copied == received.len {
            break;
        }
        let take = (len as usize).min(received.len - copied);
        if copy_to_user(pml4, base, &storage[copied..copied + take]).is_none() {
            return Done(EFAULT);
        }
        copied += take;
    }
    let mut message_flags = received.flags;
    if received.full_len > received.len {
        message_flags |= MSG_TRUNC;
    }
    if received.control_len != 0
        && copy_to_user(pml4, header.control, &control[..received.control_len]).is_none()
    {
        return Done(EFAULT);
    }
    // msg_namelen (offset 8), msg_controllen (40), and msg_flags (48).
    let name_len = match received.from.as_ref() {
        Some(from) if header.name != 0 => {
            let count = (header.name_len as usize).min(from.len);
            if copy_to_user(pml4, header.name, &from.bytes[..count]).is_none() {
                return Done(EFAULT);
            }
            from.len as u32
        }
        _ => 0,
    };
    if copy_to_user(pml4, pointer + 8, &name_len.to_ne_bytes()).is_none()
        || copy_to_user(
            pml4,
            pointer + 40,
            &(received.control_len as u64).to_ne_bytes(),
        )
        .is_none()
        || copy_to_user(pml4, pointer + 48, &message_flags.to_ne_bytes()).is_none()
    {
        return Done(EFAULT);
    }
    Done(if flags & MSG_TRUNC != 0 {
        received.full_len as i64
    } else {
        received.len as i64
    })
}

fn socket_name(pml4: PhysFrame, fd: u64, pointer: u64, len_pointer: u64, peer: bool) -> i64 {
    let address = socket_index(fd)
        .and_then(slot)
        .and_then(|slot| match &slot.kind {
            SocketKind::Unix(socket) => socket.name(peer),
            SocketKind::Inet(socket) => socket.name(peer),
        });
    match address {
        Ok(address) if pointer != 0 && len_pointer != 0 => {
            write_address(pml4, pointer, len_pointer, &address).map_or_else(|error| error, |()| 0)
        }
        Ok(_) => EFAULT,
        Err(error) => error,
    }
}

fn setsockopt(pml4: PhysFrame, fd: u64, level: u32, name: u32, pointer: u64, len: u64) -> i64 {
    if len as usize > MAX_CONTROL {
        return EINVAL;
    }
    let mut value = [0u8; MAX_CONTROL];
    let len = len as usize;
    if len != 0 && copy_from_user(pml4, pointer, &mut value[..len]).is_none() {
        return EFAULT;
    }
    let result = socket_index(fd)
        .and_then(slot)
        .and_then(|slot| match &mut slot.kind {
            SocketKind::Unix(socket) => socket.setsockopt(level, name, &value[..len]),
            SocketKind::Inet(socket) => socket.setsockopt(level, name, &value[..len]),
        });
    scheduler::linux_wake_pollers();
    if let Err(error) = result
        && (error == ENOPROTOOPT || error == EINVAL || error == EOPNOTSUPP)
    {
        crate::linux_gap::record(54, crate::linux_gap::Tag::SetsockoptOption, level, name);
    }
    match result {
        Ok(()) => 0,
        Err(error) => error,
    }
}

fn getsockopt(
    pml4: PhysFrame,
    fd: u64,
    level: u32,
    name: u32,
    pointer: u64,
    len_pointer: u64,
) -> i64 {
    let mut raw = [0u8; 4];
    if copy_from_user(pml4, len_pointer, &mut raw).is_none() {
        return EFAULT;
    }
    let capacity = u32::from_ne_bytes(raw) as usize;
    if capacity > i32::MAX as usize {
        return EINVAL;
    }
    let mut value = [0u8; MAX_CONTROL];
    let slot = match socket_index(fd).and_then(slot) {
        Ok(slot) => slot,
        Err(error) => return error,
    };
    // The generic socket options every family answers the same way.
    let produced = match (level, name, &mut slot.kind) {
        (SOL_SOCKET, SO_TYPE, SocketKind::Unix(socket)) => {
            Ok(put_int(&mut value, socket.socket_type() as i32))
        }
        (SOL_SOCKET, SO_TYPE, SocketKind::Inet(socket)) => {
            Ok(put_int(&mut value, socket.socket_type() as i32))
        }
        (SOL_SOCKET, SO_DOMAIN, SocketKind::Unix(_)) => Ok(put_int(&mut value, i32::from(AF_UNIX))),
        (SOL_SOCKET, SO_DOMAIN, SocketKind::Inet(socket)) => {
            Ok(put_int(&mut value, i32::from(socket.domain())))
        }
        (_, _, SocketKind::Unix(socket)) => socket.getsockopt(level, name, &mut value),
        (_, _, SocketKind::Inet(socket)) => socket.getsockopt(level, name, &mut value),
    };
    let produced = match produced {
        Ok(produced) => produced,
        Err(error) => return error,
    };
    let count = capacity.min(produced);
    if count != 0 && copy_to_user(pml4, pointer, &value[..count]).is_none() {
        return EFAULT;
    }
    copy_to_user(pml4, len_pointer, &(count as u32).to_ne_bytes()).map_or(EFAULT, |()| 0)
}

/// Writes a native-endian `int` option value and returns its size.
pub fn put_int(out: &mut [u8], value: i32) -> usize {
    out[..4].copy_from_slice(&value.to_ne_bytes());
    4
}

/// Reads a native-endian `int` option value.
pub fn get_int(value: &[u8]) -> Result<i32, i64> {
    value
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(i32::from_ne_bytes)
        .ok_or(EINVAL)
}

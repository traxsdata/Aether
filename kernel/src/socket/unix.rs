// SPDX-License-Identifier: Apache-2.0
//! `AF_UNIX` socket pairs.
//!
//! `socketpair(AF_UNIX, ...)` is the only way to get a local socket. The two
//! endpoints share one [`Channel`] with a queue per direction, and the pair
//! reaches another process only by inheritance across `fork` or by passing
//! an endpoint in an `SCM_RIGHTS` message. There is no filesystem or
//! abstract namespace: `socket(AF_UNIX, ...)`, `bind`, `connect`, and
//! `listen` fail, so a sandboxed app cannot reach a service by name.
//!
//! - `SOCK_STREAM` delivers bytes, but a read stops at a message that
//!   carries descriptors so they arrive with their own bytes.
//! - `SOCK_SEQPACKET` and `SOCK_DGRAM` keep message boundaries; a short
//!   buffer cuts the message and reports `MSG_TRUNC`.
//! - `SCM_RIGHTS` passes descriptors as [`LinuxPassedFile`] references.
//!   An endpoint cannot be passed through its own pair, nor while its pair
//!   holds queued sockets, which rules out the reference cycles Linux needs
//!   a garbage collector for.
//! - Writing after the peer closed or shut down fails with `EPIPE`; there
//!   is no `SIGPIPE`.

use super::{
    EAFNOSUPPORT, EAGAIN, EINVAL, EISCONN, EMSGSIZE, ENOMEM, ENOPROTOOPT, EOPNOTSUPP, EPIPE,
    EPROTONOSUPPORT, MSG_CTRUNC, MSG_PEEK, Outgoing, POLL_ERR, POLL_HUP, POLL_IN, POLL_OUT,
    POLL_RDHUP, Received, SO_ERROR, SOCK_DGRAM, SOCK_SEQPACKET, SOCK_STREAM, SOL_SOCKET, SockAddr,
    get_int, put_int,
};
use crate::scheduler::{self, LinuxPassedFile};
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU64, Ordering};

const ESOCKTNOSUPPORT: i64 = -94;
const ECONNREFUSED: i64 = -111;
const ETOOMANYREFS: i64 = -109;

const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const SO_PASSCRED: u32 = 16;
const SO_PEERCRED: u32 = 17;
const SCM_RIGHTS: i32 = 1;

/// Queued bytes per direction.
const QUEUE_BYTES: usize = 64 * 1024;
/// Queued messages per direction.
const QUEUE_MESSAGES: usize = 256;
/// Descriptors in one `SCM_RIGHTS` message, the Linux `SCM_MAX_FD`.
const MAX_PASSED: usize = 253;
/// `struct cmsghdr`: `u64 cmsg_len`, `i32 cmsg_level`, `i32 cmsg_type`.
const CMSG_HEADER: usize = 16;
/// The uid and gid every Linux task reports.
const LINUX_ID: u32 = 1000;

static NEXT_CHANNEL: AtomicU64 = AtomicU64::new(1);

struct Message {
    data: Vec<u8>,
    /// Bytes already read from a stream message.
    read: usize,
    files: Vec<LinuxPassedFile>,
}

/// State shared by the two endpoints of a pair. `queues[side]` holds what
/// endpoint `side` receives.
struct Channel {
    queues: [VecDeque<Message>; 2],
    bytes: [usize; 2],
    open: [bool; 2],
    /// `shutdown(SHUT_WR)` by that side.
    no_write: [bool; 2],
    /// `shutdown(SHUT_RD)` by that side.
    no_read: [bool; 2],
    /// Process that created the pair, for `SO_PEERCRED`.
    creator: usize,
}

impl Channel {
    fn has_room(&self, side: usize, len: usize) -> bool {
        self.queues[side].len() < QUEUE_MESSAGES && self.bytes[side] + len <= QUEUE_BYTES
    }

    fn carries_sockets(&self) -> bool {
        self.queues
            .iter()
            .flatten()
            .flat_map(|message| message.files.iter())
            .any(|file| file.socket_index().is_some())
    }
}

/// One local socket endpoint.
pub struct UnixSocket {
    socket_type: u32,
    id: u64,
    side: usize,
    channel: Rc<RefCell<Channel>>,
}

impl UnixSocket {
    /// `socket(AF_UNIX, ...)`: unsupported, because unpaired local sockets
    /// exist only to bind or connect to a name.
    pub fn new(_socket_type: u32, _protocol: u64) -> Result<Self, i64> {
        Err(EAFNOSUPPORT)
    }

    /// `socketpair(AF_UNIX, ...)`.
    pub fn pair(socket_type: u32, protocol: u64) -> Result<(Self, Self), i64> {
        if protocol != 0 {
            return Err(EPROTONOSUPPORT);
        }
        if !matches!(socket_type, SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) {
            return Err(ESOCKTNOSUPPORT);
        }
        if !crate::memory::heap_has_headroom() {
            return Err(ENOMEM);
        }
        let channel = Rc::new(RefCell::new(Channel {
            queues: [VecDeque::new(), VecDeque::new()],
            bytes: [0; 2],
            open: [true; 2],
            no_write: [false; 2],
            no_read: [false; 2],
            creator: scheduler::linux_getpid(),
        }));
        let id = NEXT_CHANNEL.fetch_add(1, Ordering::Relaxed);
        let endpoint = |side| Self {
            socket_type,
            id,
            side,
            channel: Rc::clone(&channel),
        };
        Ok((endpoint(0), endpoint(1)))
    }

    pub fn socket_type(&self) -> u32 {
        self.socket_type
    }

    /// Identifies the pair, shared by both endpoints.
    pub(super) fn channel_id(&self) -> u64 {
        self.id
    }

    /// Whether either direction of this pair holds a queued socket. A busy
    /// channel reports `true` so the caller refuses rather than guesses.
    pub(super) fn carries_sockets(&self) -> bool {
        self.channel
            .try_borrow()
            .map_or(true, |channel| channel.carries_sockets())
    }

    /// Counts pipe ends queued for this endpoint as (readers, writers).
    pub(super) fn queued_pipe_ends(&self, index: usize) -> (usize, usize) {
        let Ok(channel) = self.channel.try_borrow() else {
            return (0, 0);
        };
        let mut ends = (0, 0);
        for file in channel.queues[self.side]
            .iter()
            .flat_map(|message| message.files.iter())
        {
            match file.pipe_end() {
                Some((slot, false)) if slot == index => ends.0 += 1,
                Some((slot, true)) if slot == index => ends.1 += 1,
                _ => {}
            }
        }
        ends
    }

    fn stream(&self) -> bool {
        self.socket_type == SOCK_STREAM
    }

    pub fn connect(
        &mut self,
        _to: &SockAddr,
        _nonblock: bool,
        _restarted: bool,
    ) -> Result<(), i64> {
        Err(EISCONN)
    }

    pub fn bind(&mut self, _address: &SockAddr) -> Result<(), i64> {
        Err(EINVAL)
    }

    pub fn listen(&mut self, _backlog: u64) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn accept(&mut self) -> Result<(Self, SockAddr), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn send(&mut self, message: &Outgoing<'_>) -> Result<usize, i64> {
        if message.to.is_some() {
            return Err(EISCONN);
        }
        if !self.stream() && message.truncated {
            return Err(EMSGSIZE);
        }
        let peer = 1 - self.side;
        let len = {
            let channel = self.channel.borrow();
            if !channel.open[peer] {
                return Err(if self.socket_type == SOCK_DGRAM {
                    ECONNREFUSED
                } else {
                    EPIPE
                });
            }
            if channel.no_write[self.side] || channel.no_read[peer] {
                return Err(EPIPE);
            }
            if self.stream() {
                let room = QUEUE_BYTES.saturating_sub(channel.bytes[peer]);
                if room == 0 || channel.queues[peer].len() >= QUEUE_MESSAGES {
                    return Err(EAGAIN);
                }
                message.data.len().min(room)
            } else {
                if message.data.len() > QUEUE_BYTES {
                    return Err(EMSGSIZE);
                }
                if !channel.has_room(peer, message.data.len()) {
                    return Err(EAGAIN);
                }
                message.data.len()
            }
        };
        let files = parse_rights(message.control, self.id)?;
        if self.stream() && len == 0 && files.is_empty() {
            return Ok(0);
        }
        let mut data = Vec::new();
        data.try_reserve_exact(len).map_err(|_| ENOMEM)?;
        data.extend_from_slice(&message.data[..len]);
        let mut channel = self.channel.borrow_mut();
        // A stream folds plain bytes into the previous plain message.
        if self.stream()
            && files.is_empty()
            && let Some(last) = channel.queues[peer].back_mut()
            && last.files.is_empty()
        {
            last.data.try_reserve(len).map_err(|_| ENOMEM)?;
            last.data.extend_from_slice(&data);
        } else {
            channel.queues[peer].try_reserve(1).map_err(|_| ENOMEM)?;
            channel.queues[peer].push_back(Message {
                data,
                read: 0,
                files,
            });
        }
        channel.bytes[peer] += len;
        Ok(len)
    }

    pub fn recv(
        &mut self,
        out: &mut [u8],
        control: &mut [u8],
        flags: u32,
    ) -> Result<Received, i64> {
        let peek = flags & MSG_PEEK != 0;
        let mut received = Received::data(0);
        let mut files = Vec::new();
        {
            let mut channel = self.channel.borrow_mut();
            let side = self.side;
            if channel.queues[side].is_empty() {
                let ended =
                    !channel.open[1 - side] || channel.no_write[1 - side] || channel.no_read[side];
                return if ended { Ok(received) } else { Err(EAGAIN) };
            }
            if self.stream() {
                let mut copied = 0usize;
                let mut index = 0usize;
                while let Some(message) = channel.queues[side].get_mut(index) {
                    // Descriptors arrive with the first read of their bytes.
                    let carried = !message.files.is_empty();
                    if copied != 0 && carried {
                        break;
                    }
                    let take = (message.data.len() - message.read).min(out.len() - copied);
                    out[copied..copied + take]
                        .copy_from_slice(&message.data[message.read..message.read + take]);
                    copied += take;
                    if peek {
                        index += 1;
                    } else {
                        message.read += take;
                        files.append(&mut message.files);
                        if message.read < message.data.len() {
                            break;
                        }
                        channel.queues[side].pop_front();
                    }
                    if carried || copied == out.len() {
                        break;
                    }
                }
                if !peek {
                    channel.bytes[side] -= copied;
                }
                received.len = copied;
                received.full_len = copied;
            } else {
                let Some(message) = channel.queues[side].front() else {
                    return Err(EAGAIN);
                };
                let take = message.data.len().min(out.len());
                out[..take].copy_from_slice(&message.data[..take]);
                received.len = take;
                received.full_len = message.data.len();
                if !peek && let Some(mut message) = channel.queues[side].pop_front() {
                    channel.bytes[side] -= message.data.len();
                    files.append(&mut message.files);
                }
            }
        }
        // Install outside the channel borrow: dropping a file that did not
        // fit can close another socket.
        if !files.is_empty() {
            let (control_len, truncated) = deliver_rights(files, control);
            received.control_len = control_len;
            if truncated {
                received.flags |= MSG_CTRUNC;
            }
        }
        Ok(received)
    }

    pub fn shutdown(&mut self, how: u64) -> Result<(), i64> {
        let mut channel = self.channel.borrow_mut();
        match how {
            0 => channel.no_read[self.side] = true,
            1 => channel.no_write[self.side] = true,
            2 => {
                channel.no_read[self.side] = true;
                channel.no_write[self.side] = true;
            }
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    /// Both endpoints are unnamed: the address is the family alone.
    pub fn name(&self, _peer: bool) -> Result<SockAddr, i64> {
        Ok(SockAddr::from_bytes(&super::AF_UNIX.to_ne_bytes()))
    }

    pub fn setsockopt(&mut self, level: u32, name: u32, value: &[u8]) -> Result<(), i64> {
        match (level, name) {
            // Buffer sizes are fixed; credentials are never sent.
            (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF | SO_PASSCRED) => get_int(value).map(|_| ()),
            _ => Err(ENOPROTOOPT),
        }
    }

    pub fn getsockopt(&mut self, level: u32, name: u32, out: &mut [u8]) -> Result<usize, i64> {
        match (level, name) {
            (SOL_SOCKET, SO_ERROR | SO_PASSCRED) => Ok(put_int(out, 0)),
            (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => Ok(put_int(out, QUEUE_BYTES as i32)),
            (SOL_SOCKET, SO_PEERCRED) => {
                // struct ucred { pid_t pid; uid_t uid; gid_t gid; }
                let creator = self.channel.borrow().creator as u32;
                out[..4].copy_from_slice(&creator.to_ne_bytes());
                out[4..8].copy_from_slice(&LINUX_ID.to_ne_bytes());
                out[8..12].copy_from_slice(&LINUX_ID.to_ne_bytes());
                Ok(12)
            }
            _ => Err(ENOPROTOOPT),
        }
    }

    pub fn poll(&mut self, events: i16) -> i16 {
        let channel = self.channel.borrow();
        let side = self.side;
        let peer = 1 - side;
        let mut ready = 0;
        let peer_gone = !channel.open[peer];
        let read_ended = peer_gone || channel.no_write[peer];
        if !channel.queues[side].is_empty() || read_ended || channel.no_read[side] {
            ready |= POLL_IN;
        }
        if read_ended {
            ready |= POLL_RDHUP;
        }
        if peer_gone || channel.no_read[peer] || (channel.no_write[side] && channel.no_write[peer])
        {
            ready |= POLL_HUP;
        }
        let writable = !peer_gone && !channel.no_write[side] && !channel.no_read[peer];
        let room = if self.stream() {
            channel.has_room(peer, 1)
        } else {
            channel.has_room(peer, 0)
        };
        if writable && room {
            ready |= POLL_OUT;
        }
        ready & (events | POLL_ERR | POLL_HUP)
    }

    /// Releases the endpoint when its last descriptor closes. Messages
    /// queued for it can never be read, so their descriptors drop now;
    /// messages it sent stay for the peer.
    pub fn close(self) {
        let unread = {
            let mut channel = self.channel.borrow_mut();
            channel.open[self.side] = false;
            channel.bytes[self.side] = 0;
            core::mem::take(&mut channel.queues[self.side])
        };
        drop(unread);
    }
}

/// Collects the descriptors named by `SCM_RIGHTS` control messages. Any
/// other control message, or a malformed one, fails with `EINVAL`. `own`
/// is the sending pair, which its own endpoints may not travel through.
fn parse_rights(control: &[u8], own: u64) -> Result<Vec<LinuxPassedFile>, i64> {
    let mut files = Vec::new();
    let mut offset = 0usize;
    while control.len() - offset >= CMSG_HEADER {
        let header = &control[offset..offset + CMSG_HEADER];
        let len = u64::from_ne_bytes(header[..8].try_into().unwrap_or([0; 8])) as usize;
        let level = i32::from_ne_bytes(header[8..12].try_into().unwrap_or([0; 4]));
        let kind = i32::from_ne_bytes(header[12..16].try_into().unwrap_or([0; 4]));
        if len < CMSG_HEADER || len > control.len() - offset {
            return Err(EINVAL);
        }
        if level != SOL_SOCKET as i32
            || kind != SCM_RIGHTS
            || !(len - CMSG_HEADER).is_multiple_of(4)
        {
            return Err(EINVAL);
        }
        let count = (len - CMSG_HEADER) / 4;
        if files.len() + count > MAX_PASSED {
            return Err(EINVAL);
        }
        files.try_reserve(count).map_err(|_| ENOMEM)?;
        for raw in control[offset + CMSG_HEADER..offset + len]
            .as_chunks::<4>()
            .0
        {
            let fd = i32::from_ne_bytes(*raw);
            let fd = u64::try_from(fd).map_err(|_| super::EBADF)?;
            let file = scheduler::linux_pass_file(fd)?;
            if let Some(index) = file.socket_index()
                && !super::passable_socket(index, own)
            {
                return Err(ETOOMANYREFS);
            }
            files.push(file);
        }
        // CMSG_NXTHDR: the next header starts at the 8-byte aligned end.
        offset += len.next_multiple_of(8);
        if offset > control.len() {
            break;
        }
    }
    Ok(files)
}

/// Installs received descriptors and writes their `SCM_RIGHTS` message to
/// `control`. Returns the control bytes written and whether descriptors
/// were dropped (`MSG_CTRUNC`) for lack of room or free descriptors.
fn deliver_rights(files: Vec<LinuxPassedFile>, control: &mut [u8]) -> (usize, bool) {
    let room = control.len().saturating_sub(CMSG_HEADER) / 4;
    let mut truncated = files.len() > room;
    let mut installed = 0usize;
    for file in files.into_iter().take(room) {
        match scheduler::linux_receive_file(file) {
            Some(fd) => {
                let at = CMSG_HEADER + installed * 4;
                control[at..at + 4].copy_from_slice(&(fd as i32).to_ne_bytes());
                installed += 1;
            }
            None => truncated = true,
        }
    }
    if installed == 0 {
        return (0, truncated);
    }
    let len = CMSG_HEADER + installed * 4;
    control[..8].copy_from_slice(&(len as u64).to_ne_bytes());
    control[8..12].copy_from_slice(&(SOL_SOCKET as i32).to_ne_bytes());
    control[12..16].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
    let space = len.next_multiple_of(8).min(control.len());
    control[len..space].fill(0);
    (space, truncated)
}

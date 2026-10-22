// SPDX-License-Identifier: Apache-2.0
//! `AF_INET` and `AF_INET6` sockets over the kernel TCP/IP stack.
//!
//! Policy comes first, in this order:
//!
//! 1. The task's application must declare `network=client` in its signed
//!    import record; otherwise `socket` fails with `EACCES`.
//! 2. `bind`, `listen`, and `accept` are refused: `client` never listens.
//! 3. The destination must be reachable under
//!    [`aether_net::addr::remote_allowed`] (no loopback, link-local,
//!    multicast, or broadcast), else `ENETUNREACH`.
//! 4. The user must have allowed the application. The first connection
//!    raises a consent request and waits for the answer: a non-blocking
//!    `connect` reports `EINPROGRESS` and completes or fails once the user
//!    answers; a blocking one parks. A denial fails with `EACCES`.
//!
//! The stack is IPv4-only. `AF_INET6` sockets work for IPv4 peers through
//! IPv4-mapped addresses (`::ffff:a.b.c.d`), which is how dual-stack
//! applications reach IPv4; native IPv6 destinations fail with
//! `ENETUNREACH`.

use super::{
    EACCES, EAFNOSUPPORT, EAGAIN, EALREADY, ECONNABORTED, ECONNREFUSED, ECONNRESET, EDESTADDRREQ,
    EINPROGRESS, EINVAL, EISCONN, EMSGSIZE, ENETUNREACH, ENOPROTOOPT, ENOTCONN, EOPNOTSUPP, EPIPE,
    EPROTONOSUPPORT, ETIMEDOUT, MSG_PEEK, Outgoing, POLL_ERR, POLL_HUP, POLL_IN, POLL_OUT,
    POLL_RDHUP, Received, SO_ERROR, SOCK_DGRAM, SOCK_STREAM, SOL_SOCKET, SockAddr, get_int,
    put_int,
};
use crate::net::{self, consent};
use crate::scheduler;
use aether_net::addr::{self, AddrError, Endpoint};
use alloc::vec;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{tcp, udp};
use smoltcp::time::Duration;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};

const TCP_RECV_BUFFER: usize = 128 * 1024;
const TCP_SEND_BUFFER: usize = 64 * 1024;
const UDP_PACKETS: usize = 32;
const UDP_BUFFER: usize = 32 * 1024;
/// Largest UDP payload without IP fragmentation.
const UDP_MAX_PAYLOAD: usize = net::MTU - 20 - 8;
/// Connect gives up after this long without an answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT_TICKS: u64 = 30 * crate::arch::x86_64::timer::TIMER_HZ as u64;

const IPPROTO_IP: u32 = 0;
const IPPROTO_TCP: u32 = 6;
const IPPROTO_UDP: u32 = 17;
const IPPROTO_IPV6: u32 = 41;
const SO_REUSEADDR: u32 = 2;
const SO_BROADCAST: u32 = 6;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const SO_KEEPALIVE: u32 = 9;
const SO_LINGER: u32 = 13;
const SO_RCVTIMEO: u32 = 20;
const SO_SNDTIMEO: u32 = 21;
const SO_ACCEPTCONN: u32 = 30;
const TCP_NODELAY: u32 = 1;
const TCP_KEEPIDLE: u32 = 4;
const TCP_KEEPINTVL: u32 = 5;
const TCP_KEEPCNT: u32 = 6;
const TCP_USER_TIMEOUT: u32 = 18;
const TCP_FASTOPEN_CONNECT: u32 = 30;
const IP_TOS: u32 = 1;
const IP_TTL: u32 = 2;
const IP_RECVERR: u32 = 11;
const IPV6_V6ONLY: u32 = 26;

/// Connection progress of a stream socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Not connected.
    Idle,
    /// Waiting for the user's consent before the SYN is sent.
    Consent(Endpoint),
    /// SYN sent.
    Connecting,
    /// Connection established and reported.
    Connected,
    /// The connection failed or was reset; the error is in `error`.
    Failed,
}

/// One Internet socket.
pub struct InetSocket {
    domain: u16,
    socket_type: u32,
    handle: Option<SocketHandle>,
    stage: Stage,
    /// Pending `SO_ERROR`, cleared when read.
    error: i64,
    /// Whether a blocking or non-blocking `connect` has been answered.
    connect_reported: bool,
    /// Tick the SYN was sent, to tell a timeout from a refusal.
    connect_started: u64,
    /// Connected peer (stream) or default destination (datagram).
    peer: Option<Endpoint>,
    read_shut: bool,
    write_shut: bool,
    nodelay: bool,
    keepalive: bool,
}

/// The current application's identity for the consent store.
fn app_id() -> &'static str {
    crate::linux::app_identity().map_or("", |(id, _, _)| id)
}

fn to_smoltcp(endpoint: Endpoint) -> IpEndpoint {
    IpEndpoint::new(
        IpAddress::Ipv4(Ipv4Address::from(endpoint.ip)),
        endpoint.port,
    )
}

fn from_smoltcp(endpoint: IpEndpoint) -> Endpoint {
    let IpAddress::Ipv4(ip) = endpoint.addr;
    Endpoint {
        ip: ip.octets(),
        port: endpoint.port,
    }
}

fn decode(domain: u16, address: &SockAddr) -> Result<Endpoint, i64> {
    addr::decode(domain, address.as_bytes()).map_err(|error| match error {
        AddrError::Invalid => EINVAL,
        AddrError::Ipv6Unsupported => ENETUNREACH,
        AddrError::Unspecified => EAFNOSUPPORT,
    })
}

/// Checks a destination against the reachable-address policy and the
/// interface state.
fn reachable(endpoint: Endpoint) -> Result<(), i64> {
    if endpoint.port == 0 && endpoint.ip == [0; 4] {
        return Err(EINVAL);
    }
    if !addr::remote_allowed(endpoint.ip) {
        crate::linux_policy::record_network_denial();
        return Err(ENETUNREACH);
    }
    match net::with(|net| net.address().is_some()) {
        Some(true) => Ok(()),
        _ => Err(ENETUNREACH),
    }
}

impl InetSocket {
    /// `socket(AF_INET or AF_INET6, ...)`.
    pub fn new(domain: u16, socket_type: u32, protocol: u64) -> Result<Self, i64> {
        if !scheduler::linux_network_declared() {
            crate::linux_policy::record_network_denial();
            return Err(EACCES);
        }
        match (socket_type, protocol as u32) {
            (SOCK_STREAM, 0 | IPPROTO_TCP) | (SOCK_DGRAM, 0 | IPPROTO_UDP) => {}
            (SOCK_STREAM | SOCK_DGRAM, _) => return Err(EPROTONOSUPPORT),
            // Raw sockets would let an application forge packets.
            _ => return Err(EPROTONOSUPPORT),
        }
        Ok(Self {
            domain,
            socket_type,
            handle: None,
            stage: Stage::Idle,
            error: 0,
            connect_reported: false,
            connect_started: 0,
            peer: None,
            read_shut: false,
            write_shut: false,
            nodelay: false,
            keepalive: false,
        })
    }

    pub fn domain(&self) -> u16 {
        self.domain
    }

    pub fn socket_type(&self) -> u32 {
        self.socket_type
    }

    fn stream(&self) -> bool {
        self.socket_type == SOCK_STREAM
    }

    /// Advances a stream socket through consent and the TCP handshake.
    fn refresh(&mut self) {
        match self.stage {
            Stage::Consent(endpoint) => match consent::check(app_id()) {
                consent::Access::Pending => {}
                consent::Access::Denied => self.fail(EACCES),
                consent::Access::Allowed => self.start_connect(endpoint),
            },
            Stage::Connecting => {
                let Some(handle) = self.handle else {
                    return self.fail(ECONNREFUSED);
                };
                let state = net::with(|net| net.tcp(handle).state());
                match state {
                    Some(tcp::State::SynSent | tcp::State::SynReceived) => {}
                    Some(tcp::State::Closed) | None => {
                        // smoltcp closes on a reset and on its own timeout;
                        // only the timeout takes the whole connect window.
                        let elapsed = scheduler::ticks().saturating_sub(self.connect_started);
                        let timed_out = elapsed + 100 >= CONNECT_TIMEOUT_TICKS;
                        self.fail(if timed_out { ETIMEDOUT } else { ECONNREFUSED });
                    }
                    Some(_) => self.stage = Stage::Connected,
                }
            }
            Stage::Connected => {
                // Blocking an app in Security Center ends its live
                // connections, not only new ones.
                if consent::blocked(app_id()) {
                    return self.fail(ECONNABORTED);
                }
                if let Some(handle) = self.handle
                    && net::with(|net| net.tcp(handle).state()) == Some(tcp::State::Closed)
                    && net::with(|net| net.tcp(handle).recv_queue()) == Some(0)
                    && !self.write_shut
                {
                    // Closed without our FIN: the peer reset or the stack
                    // timed out. A clean close passes through FIN states and
                    // `may_recv` reports end of file instead.
                    self.error = ECONNRESET;
                }
            }
            Stage::Idle | Stage::Failed => {}
        }
    }

    fn fail(&mut self, error: i64) {
        self.stage = Stage::Failed;
        self.error = error;
        self.connect_reported = false;
        self.release_handle();
    }

    fn start_connect(&mut self, endpoint: Endpoint) {
        let nodelay = self.nodelay;
        let keepalive = self.keepalive;
        let started = net::with(|net| {
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; TCP_RECV_BUFFER]),
                tcp::SocketBuffer::new(vec![0; TCP_SEND_BUFFER]),
            );
            socket.set_timeout(Some(CONNECT_TIMEOUT));
            socket.set_nagle_enabled(!nodelay);
            if keepalive {
                socket.set_keep_alive(Some(Duration::from_secs(60)));
            }
            let handle = net.sockets.add(socket);
            let port = net.ephemeral_port();
            let (socket, context) = net.tcp_with_context(handle);
            match socket.connect(context, to_smoltcp(endpoint), port) {
                Ok(()) => Ok(handle),
                Err(_) => {
                    net.sockets.remove(handle);
                    Err(ENETUNREACH)
                }
            }
        });
        match started {
            Some(Ok(handle)) => {
                self.handle = Some(handle);
                self.stage = Stage::Connecting;
                self.connect_started = scheduler::ticks();
                net::poll();
            }
            Some(Err(error)) => self.fail(error),
            None => self.fail(ENETUNREACH),
        }
    }

    /// `connect`. Restarts after a park come back here with `restarted`.
    pub fn connect(&mut self, to: &SockAddr, nonblock: bool, restarted: bool) -> Result<(), i64> {
        if !self.stream() {
            return self.connect_datagram(to);
        }
        self.refresh();
        match self.stage {
            Stage::Idle => {}
            Stage::Consent(_) | Stage::Connecting => {
                return Err(if nonblock && !restarted {
                    EALREADY
                } else {
                    EAGAIN
                });
            }
            Stage::Connected => {
                if self.connect_reported {
                    return Err(EISCONN);
                }
                self.connect_reported = true;
                return Ok(());
            }
            Stage::Failed => {
                if restarted || !self.connect_reported {
                    self.connect_reported = true;
                    let error = core::mem::take(&mut self.error);
                    self.stage = Stage::Idle;
                    return Err(if error == 0 { ECONNREFUSED } else { error });
                }
                self.stage = Stage::Idle;
            }
        }
        let endpoint = decode(self.domain, to)?;
        reachable(endpoint)?;
        self.peer = Some(endpoint);
        self.error = 0;
        self.connect_reported = false;
        self.stage = Stage::Consent(endpoint);
        self.refresh();
        match self.stage {
            Stage::Failed => {
                self.connect_reported = true;
                self.stage = Stage::Idle;
                Err(core::mem::take(&mut self.error))
            }
            _ if nonblock => Err(EINPROGRESS),
            _ => Err(EAGAIN),
        }
    }

    fn connect_datagram(&mut self, to: &SockAddr) -> Result<(), i64> {
        if to.family() == Some(0) {
            self.peer = None;
            return Ok(());
        }
        let endpoint = decode(self.domain, to)?;
        reachable(endpoint)?;
        self.datagram_consent()?;
        self.ensure_udp()?;
        self.peer = Some(endpoint);
        Ok(())
    }

    /// Datagrams are not held back while the user decides: the request is
    /// raised and the call fails with `EACCES` until the user allows it.
    fn datagram_consent(&self) -> Result<(), i64> {
        match consent::check(app_id()) {
            consent::Access::Allowed => Ok(()),
            consent::Access::Denied | consent::Access::Pending => Err(EACCES),
        }
    }

    fn ensure_udp(&mut self) -> Result<SocketHandle, i64> {
        if let Some(handle) = self.handle {
            return Ok(handle);
        }
        let handle = net::with(|net| {
            let mut socket = udp::Socket::new(
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
                    vec![0; UDP_BUFFER],
                ),
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
                    vec![0; UDP_BUFFER],
                ),
            );
            let port = net.ephemeral_port();
            socket.bind(port).map_err(|_| ENETUNREACH)?;
            Ok::<_, i64>(net.sockets.add(socket))
        })
        .ok_or(ENETUNREACH)??;
        self.handle = Some(handle);
        Ok(handle)
    }

    /// Binding is how a listener starts; `network=client` never listens.
    /// Binding to the any address and port 0 is a no-op some clients do.
    pub fn bind(&mut self, address: &SockAddr) -> Result<(), i64> {
        let endpoint = decode(self.domain, address)?;
        if endpoint.ip == [0; 4] && endpoint.port == 0 {
            return Ok(());
        }
        crate::linux_policy::record_network_denial();
        Err(EACCES)
    }

    pub fn listen(&mut self, _backlog: u64) -> Result<(), i64> {
        crate::linux_policy::record_network_denial();
        Err(EACCES)
    }

    pub fn accept(&mut self) -> Result<(Self, SockAddr), i64> {
        Err(EINVAL)
    }

    pub fn send(&mut self, message: &Outgoing<'_>) -> Result<usize, i64> {
        if self.stream() {
            self.send_stream(message)
        } else {
            self.send_datagram(message)
        }
    }

    fn send_stream(&mut self, message: &Outgoing<'_>) -> Result<usize, i64> {
        self.refresh();
        if self.write_shut {
            return Err(EPIPE);
        }
        match self.stage {
            Stage::Connected => {}
            Stage::Consent(_) | Stage::Connecting => return Err(EAGAIN),
            Stage::Failed => return Err(core::mem::take(&mut self.error).min(-1)),
            Stage::Idle => {
                return Err(if message.to.is_some() {
                    EOPNOTSUPP
                } else {
                    ENOTCONN
                });
            }
        }
        let handle = self.handle.ok_or(ENOTCONN)?;
        let sent = net::with(|net| {
            let socket = net.tcp(handle);
            if !socket.may_send() {
                return Err(EPIPE);
            }
            if !socket.can_send() {
                return Err(EAGAIN);
            }
            socket.send_slice(message.data).map_err(|_| EPIPE)
        })
        .ok_or(ENETUNREACH)??;
        net::poll();
        Ok(sent)
    }

    fn send_datagram(&mut self, message: &Outgoing<'_>) -> Result<usize, i64> {
        if message.truncated || message.data.len() > UDP_MAX_PAYLOAD {
            return Err(EMSGSIZE);
        }
        let destination = match message.to {
            Some(to) => {
                let endpoint = decode(self.domain, to)?;
                reachable(endpoint)?;
                endpoint
            }
            None => self.peer.ok_or(EDESTADDRREQ)?,
        };
        self.datagram_consent()?;
        let handle = self.ensure_udp()?;
        net::with(|net| {
            let socket = net.udp(handle);
            if !socket.can_send() {
                return Err(EAGAIN);
            }
            socket
                .send_slice(message.data, to_smoltcp(destination))
                .map_err(|_| EAGAIN)
        })
        .ok_or(ENETUNREACH)??;
        net::poll();
        Ok(message.data.len())
    }

    pub fn recv(
        &mut self,
        out: &mut [u8],
        _control: &mut [u8],
        flags: u32,
    ) -> Result<Received, i64> {
        if self.stream() {
            self.recv_stream(out, flags)
        } else {
            self.recv_datagram(out, flags)
        }
    }

    fn recv_stream(&mut self, out: &mut [u8], flags: u32) -> Result<Received, i64> {
        self.refresh();
        if self.read_shut {
            return Ok(Received::data(0));
        }
        match self.stage {
            Stage::Connected => {}
            Stage::Consent(_) | Stage::Connecting => return Err(EAGAIN),
            Stage::Failed => return Err(core::mem::take(&mut self.error).min(-1)),
            Stage::Idle => return Err(ENOTCONN),
        }
        let handle = self.handle.ok_or(ENOTCONN)?;
        let error = self.error;
        let result = net::with(|net| {
            let socket = net.tcp(handle);
            if socket.can_recv() {
                let count = if flags & MSG_PEEK != 0 {
                    socket.peek_slice(out)
                } else {
                    socket.recv_slice(out)
                };
                return count.map_err(|_| ECONNRESET);
            }
            if !socket.may_recv() {
                // A clean FIN is end of file; a reset is an error.
                return if error == ECONNRESET {
                    Err(ECONNRESET)
                } else {
                    Ok(0)
                };
            }
            Err(EAGAIN)
        })
        .ok_or(ENETUNREACH)?;
        if result == Err(ECONNRESET) {
            self.error = 0;
        }
        let count = result?;
        if count != 0 {
            // Reading frees window space; let the ACK go out now.
            net::poll();
        }
        Ok(Received::data(count))
    }

    fn recv_datagram(&mut self, out: &mut [u8], flags: u32) -> Result<Received, i64> {
        let Some(handle) = self.handle else {
            return Err(EAGAIN);
        };
        let peer = self.peer;
        let domain = self.domain;
        net::with(|net| {
            let socket = net.udp(handle);
            loop {
                let (payload, meta) = if flags & MSG_PEEK != 0 {
                    socket.peek().map(|(payload, meta)| (payload, *meta))
                } else {
                    socket.recv()
                }
                .map_err(|_| EAGAIN)?;
                let from = from_smoltcp(meta.endpoint);
                // A connected datagram socket only sees its peer.
                if peer.is_some_and(|peer| peer != from) {
                    if flags & MSG_PEEK != 0 {
                        return Err(EAGAIN);
                    }
                    continue;
                }
                let count = payload.len().min(out.len());
                out[..count].copy_from_slice(&payload[..count]);
                let mut encoded = [0u8; addr::SOCKADDR_IN6_LEN];
                let length = addr::encode(domain, from, &mut encoded);
                return Ok(Received {
                    len: count,
                    full_len: payload.len(),
                    from: Some(SockAddr::from_bytes(&encoded[..length])),
                    control_len: 0,
                    flags: 0,
                });
            }
        })
        .ok_or(ENETUNREACH)?
    }

    pub fn shutdown(&mut self, how: u64) -> Result<(), i64> {
        if how > 2 {
            return Err(EINVAL);
        }
        if self.stream() && self.stage != Stage::Connected {
            return Err(ENOTCONN);
        }
        if how != 1 {
            self.read_shut = true;
        }
        if how != 0 && !self.write_shut {
            self.write_shut = true;
            if let Some(handle) = self.handle.filter(|_| self.stream()) {
                net::with(|net| net.tcp(handle).close());
                net::poll();
            }
        }
        Ok(())
    }

    pub fn name(&self, peer: bool) -> Result<SockAddr, i64> {
        let endpoint = if peer {
            if self.stream() && self.stage != Stage::Connected {
                return Err(ENOTCONN);
            }
            self.peer.ok_or(ENOTCONN)?
        } else {
            let local = self.handle.and_then(|handle| {
                net::with(|net| {
                    let address = net.address().map_or([0; 4], |address| address.octets());
                    let port = if self.stream() {
                        net.tcp(handle)
                            .local_endpoint()
                            .map_or(0, |local| local.port)
                    } else {
                        net.udp(handle).endpoint().port
                    };
                    Endpoint { ip: address, port }
                })
            });
            local.unwrap_or(Endpoint {
                ip: [0; 4],
                port: 0,
            })
        };
        let mut encoded = [0u8; addr::SOCKADDR_IN6_LEN];
        let length = addr::encode(self.domain, endpoint, &mut encoded);
        Ok(SockAddr::from_bytes(&encoded[..length]))
    }

    pub fn setsockopt(&mut self, level: u32, name: u32, value: &[u8]) -> Result<(), i64> {
        match (level, name) {
            (IPPROTO_TCP, TCP_NODELAY) => {
                self.nodelay = get_int(value)? != 0;
                if let Some(handle) = self.handle.filter(|_| self.stream()) {
                    let nagle = !self.nodelay;
                    net::with(|net| net.tcp(handle).set_nagle_enabled(nagle));
                }
                Ok(())
            }
            (SOL_SOCKET, SO_KEEPALIVE) => {
                self.keepalive = get_int(value)? != 0;
                if let Some(handle) = self.handle.filter(|_| self.stream()) {
                    let interval = self.keepalive.then(|| Duration::from_secs(60));
                    net::with(|net| net.tcp(handle).set_keep_alive(interval));
                }
                Ok(())
            }
            // Accepted and ignored: the stack sizes buffers and timers itself.
            (
                SOL_SOCKET,
                SO_REUSEADDR | SO_SNDBUF | SO_RCVBUF | SO_LINGER | SO_RCVTIMEO | SO_SNDTIMEO,
            )
            | (
                IPPROTO_TCP,
                TCP_KEEPIDLE | TCP_KEEPINTVL | TCP_KEEPCNT | TCP_USER_TIMEOUT
                | TCP_FASTOPEN_CONNECT,
            )
            | (IPPROTO_IP, IP_TOS | IP_TTL | IP_RECVERR)
            | (IPPROTO_IPV6, IPV6_V6ONLY) => Ok(()),
            (SOL_SOCKET, SO_BROADCAST) => {
                if get_int(value)? != 0 {
                    crate::linux_policy::record_network_denial();
                    Err(EACCES)
                } else {
                    Ok(())
                }
            }
            _ => Err(ENOPROTOOPT),
        }
    }

    pub fn getsockopt(&mut self, level: u32, name: u32, out: &mut [u8]) -> Result<usize, i64> {
        match (level, name) {
            (SOL_SOCKET, SO_ERROR) => {
                self.refresh();
                let error = core::mem::take(&mut self.error);
                if self.stage == Stage::Failed {
                    self.stage = Stage::Idle;
                    self.connect_reported = true;
                }
                Ok(put_int(out, -error as i32))
            }
            (SOL_SOCKET, SO_RCVBUF) => Ok(put_int(out, TCP_RECV_BUFFER as i32)),
            (SOL_SOCKET, SO_SNDBUF) => Ok(put_int(out, TCP_SEND_BUFFER as i32)),
            (SOL_SOCKET, SO_KEEPALIVE) => Ok(put_int(out, i32::from(self.keepalive))),
            (SOL_SOCKET, SO_REUSEADDR | SO_ACCEPTCONN | SO_BROADCAST) => Ok(put_int(out, 0)),
            (IPPROTO_TCP, TCP_NODELAY) if self.stream() => {
                Ok(put_int(out, i32::from(self.nodelay)))
            }
            (IPPROTO_IPV6, IPV6_V6ONLY) => Ok(put_int(out, 0)),
            _ => Err(ENOPROTOOPT),
        }
    }

    pub fn poll(&mut self, events: i16) -> i16 {
        if !self.stream() {
            let readable = self
                .handle
                .and_then(|handle| net::with(|net| net.udp(handle).can_recv()))
                .unwrap_or(false);
            let mut ready = POLL_OUT;
            if readable {
                ready |= POLL_IN;
            }
            return ready & (events | POLL_ERR | POLL_HUP);
        }
        self.refresh();
        let ready = match self.stage {
            Stage::Idle => POLL_OUT | POLL_HUP,
            Stage::Consent(_) | Stage::Connecting => 0,
            Stage::Failed => POLL_ERR | POLL_HUP | POLL_OUT | POLL_IN,
            Stage::Connected => {
                let Some(handle) = self.handle else {
                    return POLL_HUP;
                };
                let error = self.error;
                net::with(|net| {
                    let socket = net.tcp(handle);
                    let mut ready = 0;
                    if socket.can_recv() || !socket.may_recv() {
                        ready |= POLL_IN;
                    }
                    if !socket.may_recv() {
                        ready |= POLL_RDHUP;
                    }
                    if socket.can_send() {
                        ready |= POLL_OUT;
                    }
                    if !socket.may_recv() && !socket.may_send() {
                        ready |= POLL_HUP;
                    }
                    if error != 0 {
                        ready |= POLL_ERR;
                    }
                    ready
                })
                .unwrap_or(POLL_HUP)
            }
        };
        ready & (events | POLL_ERR | POLL_HUP)
    }

    fn release_handle(&mut self) {
        if let Some(handle) = self.handle.take() {
            let stream = self.stream();
            net::with(|net| {
                if stream {
                    net.retire_tcp(handle);
                } else {
                    net.sockets.remove(handle);
                }
            });
        }
    }

    /// Releases the socket when its last descriptor closes. A connected
    /// stream sends its FIN and finishes closing in the background.
    pub fn close(mut self) {
        self.release_handle();
        net::poll();
    }
}

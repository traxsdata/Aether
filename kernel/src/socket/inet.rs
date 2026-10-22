// SPDX-License-Identifier: Apache-2.0
//! `AF_INET` and `AF_INET6` sockets.
//!
//! Policy comes first. A Linux task may create an Internet socket only when
//! its application's signed import record declares `network=client`; any
//! other task gets `EACCES`, exactly as before sockets existed. The user's
//! first-use consent and the TCP/IP stack itself arrive in later changes, so
//! for now an eligible application gets `EAFNOSUPPORT`, which glibc and
//! Firefox treat as "no network".

use super::{EACCES, EAFNOSUPPORT, EOPNOTSUPP, Outgoing, Received, SockAddr};
use crate::scheduler;

/// One Internet socket.
pub struct InetSocket {
    domain: u16,
    socket_type: u32,
}

impl InetSocket {
    /// `socket(AF_INET or AF_INET6, ...)`.
    pub fn new(_domain: u16, _socket_type: u32, _protocol: u64) -> Result<Self, i64> {
        if !scheduler::linux_network_declared() {
            crate::linux_policy::record_network_denial();
            return Err(EACCES);
        }
        Err(EAFNOSUPPORT)
    }

    pub fn domain(&self) -> u16 {
        self.domain
    }

    pub fn socket_type(&self) -> u32 {
        self.socket_type
    }

    pub fn connect(
        &mut self,
        _to: &SockAddr,
        _nonblock: bool,
        _restarted: bool,
    ) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    /// Binding is how a listener starts; `network=client` never listens.
    pub fn bind(&mut self, _address: &SockAddr) -> Result<(), i64> {
        Err(EACCES)
    }

    pub fn listen(&mut self, _backlog: u64) -> Result<(), i64> {
        Err(EACCES)
    }

    pub fn accept(&mut self) -> Result<(Self, SockAddr), i64> {
        Err(EACCES)
    }

    pub fn send(&mut self, _message: &Outgoing<'_>) -> Result<usize, i64> {
        Err(EOPNOTSUPP)
    }

    pub fn recv(
        &mut self,
        _out: &mut [u8],
        _control: &mut [u8],
        _flags: u32,
    ) -> Result<Received, i64> {
        Err(EOPNOTSUPP)
    }

    pub fn shutdown(&mut self, _how: u64) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn name(&self, _peer: bool) -> Result<SockAddr, i64> {
        Err(EOPNOTSUPP)
    }

    pub fn setsockopt(&mut self, _level: u32, _name: u32, _value: &[u8]) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn getsockopt(&mut self, _level: u32, _name: u32, _out: &mut [u8]) -> Result<usize, i64> {
        Err(EOPNOTSUPP)
    }

    pub fn poll(&mut self, _events: i16) -> i16 {
        0
    }

    /// Releases the socket when its last descriptor closes.
    pub fn close(self) {}
}

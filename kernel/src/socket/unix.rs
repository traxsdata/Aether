// SPDX-License-Identifier: Apache-2.0
//! `AF_UNIX` sockets.
//!
//! Local sockets are not implemented yet: creating one fails with
//! `EAFNOSUPPORT`. Socket pairs, brokered connects, and `SCM_RIGHTS` land
//! here, behind the same per-family interface [`super::inet`] implements.

use super::{EAFNOSUPPORT, EOPNOTSUPP, Outgoing, Received, SockAddr};

/// One local socket endpoint.
pub struct UnixSocket {
    socket_type: u32,
}

impl UnixSocket {
    /// `socket(AF_UNIX, ...)`.
    pub fn new(_socket_type: u32, _protocol: u64) -> Result<Self, i64> {
        Err(EAFNOSUPPORT)
    }

    /// `socketpair(AF_UNIX, ...)`.
    pub fn pair(_socket_type: u32, _protocol: u64) -> Result<(Self, Self), i64> {
        Err(EAFNOSUPPORT)
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

    pub fn bind(&mut self, _address: &SockAddr) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn listen(&mut self, _backlog: u64) -> Result<(), i64> {
        Err(EOPNOTSUPP)
    }

    pub fn accept(&mut self) -> Result<(Self, SockAddr), i64> {
        Err(EOPNOTSUPP)
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

    /// Releases the endpoint when its last descriptor closes.
    pub fn close(self) {}
}

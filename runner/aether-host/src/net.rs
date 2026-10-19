//! net.rs - outgoing TCP connections.
//!
//! On æther (`AETHER=1`) a `TcpStream` is a native network handle: the
//! kernel resolves the host name, the firewall decides whether this app may
//! reach it, and data moves with SYS_NET_SEND / SYS_NET_RECV. Anywhere else
//! (development and tests on a Linux host) it is a `std::net::TcpStream`.
//!
//! Errors map onto `io::ErrorKind` so ordinary Rust code can handle them:
//!
//! | æther      | io::ErrorKind      | text |
//! |------------|--------------------|------|
//! | E_DENIED   | PermissionDenied   | "blocked by the firewall" |
//! | E_PERM     | PermissionDenied   | "this app has no network permission" |
//! | E_TIMEOUT  | TimedOut           | "timed out" |
//! | E_NET      | ConnectionRefused (connect) / ConnectionReset (later) | "network error ..." |
//! | E_INVAL    | InvalidInput       | |

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::abi;
use crate::sys;
use crate::Error;

/// Turns an æther error from a network call into an io::Error. `connecting`
/// picks the kind for E_NET (refused while connecting, reset afterwards).
pub fn net_error(e: Error, connecting: bool) -> io::Error {
    use io::ErrorKind as K;
    let (kind, msg): (K, &str) = match e.0 {
        abi::E_DENIED => (K::PermissionDenied, "blocked by the firewall"),
        abi::E_PERM => (K::PermissionDenied, "this app has no network permission"),
        abi::E_TIMEOUT => (K::TimedOut, "timed out"),
        abi::E_NET if connecting => (
            K::ConnectionRefused,
            "network error: no network, the host can't be found, or the connection was refused",
        ),
        abi::E_NET => (K::ConnectionReset, "network error: the connection was reset"),
        abi::E_INVAL => (K::InvalidInput, "invalid argument"),
        abi::E_NOENT => (K::NotFound, "no such connection"),
        abi::E_LIMIT => (K::Other, "too many connections"),
        abi::E_NOMEM => (K::OutOfMemory, "out of memory"),
        abi::E_FAULT => (K::InvalidInput, "bad pointer"),
        _ => (K::Other, e.name()),
    };
    io::Error::new(kind, msg)
}

enum Inner {
    Aether { handle: u32, read_timeout: Option<u32> },
    Host(std::net::TcpStream),
}

/// An outgoing TCP connection. Closed when dropped.
pub struct TcpStream {
    inner: Inner,
}

impl TcpStream {
    /// Connects to `host:port`, giving up after `timeout_ms` (0 = the
    /// kernel's or OS's default). `host` is a DNS name or an IPv4 literal.
    pub fn connect(host: &str, port: u16, timeout_ms: u32) -> io::Result<TcpStream> {
        if host.is_empty() || host.len() >= abi::NET_HOST_MAX || host.bytes().any(|b| b <= b' ' || b >= 0x7f) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid host name"));
        }
        if port == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid port"));
        }
        if crate::on_aether() {
            let handle = sys::net_connect(host, port, timeout_ms).map_err(|e| net_error(e, true))?;
            Ok(TcpStream { inner: Inner::Aether { handle, read_timeout: None } })
        } else {
            Self::connect_host(host, port, timeout_ms)
        }
    }

    fn connect_host(host: &str, port: u16, timeout_ms: u32) -> io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        let addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
        let mut last = io::Error::new(io::ErrorKind::NotFound, "the host can't be found");
        for a in addrs {
            let r = if timeout_ms == 0 {
                std::net::TcpStream::connect(a)
            } else {
                std::net::TcpStream::connect_timeout(&a, Duration::from_millis(timeout_ms as u64))
            };
            match r {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    return Ok(TcpStream { inner: Inner::Host(s) });
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Reads give up with `TimedOut` after this long; None waits forever.
    pub fn set_read_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        if t == Some(Duration::ZERO) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "zero timeout"));
        }
        match &mut self.inner {
            Inner::Aether { read_timeout, .. } => {
                *read_timeout = t.map(|d| d.as_millis().clamp(1, u32::MAX as u128 - 1) as u32);
                Ok(())
            }
            Inner::Host(s) => s.set_read_timeout(t),
        }
    }

    /// Writes give up after this long (host only; on æther a send blocks
    /// until the kernel's window has room or the connection fails).
    pub fn set_write_timeout(&mut self, t: Option<Duration>) -> io::Result<()> {
        match &mut self.inner {
            Inner::Aether { .. } => Ok(()),
            Inner::Host(s) => s.set_write_timeout(t),
        }
    }

    /// True when this is a native æther connection.
    pub fn is_native(&self) -> bool {
        matches!(self.inner, Inner::Aether { .. })
    }
}

fn host_timeout(e: io::Error) -> io::Error {
    // std reports an expired read timeout as WouldBlock on Unix.
    if e.kind() == io::ErrorKind::WouldBlock {
        io::Error::new(io::ErrorKind::TimedOut, "timed out")
    } else {
        e
    }
}

impl Read for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.inner {
            Inner::Aether { handle, read_timeout } => {
                // Bounded so the length always fits the kernel's argument.
                let n = buf.len().min(1 << 20);
                sys::net_recv(*handle, &mut buf[..n], *read_timeout).map_err(|e| net_error(e, false))
            }
            Inner::Host(s) => s.read(buf).map_err(host_timeout),
        }
    }
}

impl Write for TcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.inner {
            Inner::Aether { handle, .. } => {
                let n = buf.len().min(1 << 20);
                let sent = sys::net_send(*handle, &buf[..n]).map_err(|e| net_error(e, false))?;
                if sent == 0 || sent > n {
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "the connection accepted no data"));
                }
                Ok(sent)
            }
            Inner::Host(s) => s.write(buf).map_err(host_timeout),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.inner {
            Inner::Aether { .. } => Ok(()),
            Inner::Host(s) => s.flush(),
        }
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        if let Inner::Aether { handle, .. } = self.inner {
            let _ = sys::net_close(handle);
        }
    }
}

/// The network's state (æther only).
pub fn info() -> io::Result<abi::net_info> {
    if !crate::on_aether() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "not running on æther"));
    }
    sys::net_info().map_err(|e| net_error(e, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_text() {
        let e = net_error(Error(abi::E_DENIED), true);
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("blocked by the firewall"));
        assert_eq!(net_error(Error(abi::E_TIMEOUT), false).kind(), io::ErrorKind::TimedOut);
        assert_eq!(net_error(Error(abi::E_NET), true).kind(), io::ErrorKind::ConnectionRefused);
        assert_eq!(net_error(Error(abi::E_NET), false).kind(), io::ErrorKind::ConnectionReset);
    }

    #[test]
    fn loopback() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut b = [0u8; 5];
            c.read_exact(&mut b).unwrap();
            c.write_all(&b).unwrap();
        });
        let mut s = TcpStream::connect("127.0.0.1", port, 2000).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        s.write_all(b"hello").unwrap();
        let mut b = [0u8; 5];
        s.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hello");
        t.join().unwrap();
        assert!(TcpStream::connect("bad host", 80, 100).is_err());
    }
}

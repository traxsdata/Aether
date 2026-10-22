// SPDX-License-Identifier: Apache-2.0
//! Linux Internet socket addresses and the reachable-address policy.

/// `AF_INET`.
pub const AF_INET: u16 = 2;
/// `AF_INET6`.
pub const AF_INET6: u16 = 10;
/// Size of `struct sockaddr_in`.
pub const SOCKADDR_IN_LEN: usize = 16;
/// Size of `struct sockaddr_in6`.
pub const SOCKADDR_IN6_LEN: usize = 28;

/// An IPv4 endpoint. IPv4-mapped IPv6 addresses decode to this too, because
/// the stack is IPv4-only and dual-stack sockets use them for IPv4 peers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Address in network order.
    pub ip: [u8; 4],
    /// Port in host order.
    pub port: u16,
}

/// Why an address was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrError {
    /// Too short for its family, or the family does not match the socket.
    Invalid,
    /// A native IPv6 address; the stack has no IPv6 yet.
    Ipv6Unsupported,
    /// `AF_UNSPEC`, which `connect` uses to dissolve an association.
    Unspecified,
}

/// Decodes a user `sockaddr` for a socket of `domain`.
///
/// An `AF_INET6` socket accepts `sockaddr_in6` only; an `AF_INET` socket
/// accepts `sockaddr_in` only, as Linux does.
pub fn decode(domain: u16, bytes: &[u8]) -> Result<Endpoint, AddrError> {
    if bytes.len() < 2 {
        return Err(AddrError::Invalid);
    }
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    if family == 0 {
        return Err(AddrError::Unspecified);
    }
    if family != domain {
        return Err(AddrError::Invalid);
    }
    match family {
        AF_INET => {
            if bytes.len() < SOCKADDR_IN_LEN {
                return Err(AddrError::Invalid);
            }
            Ok(Endpoint {
                port: u16::from_be_bytes([bytes[2], bytes[3]]),
                ip: [bytes[4], bytes[5], bytes[6], bytes[7]],
            })
        }
        AF_INET6 => {
            if bytes.len() < SOCKADDR_IN6_LEN {
                return Err(AddrError::Invalid);
            }
            let port = u16::from_be_bytes([bytes[2], bytes[3]]);
            let address = &bytes[8..24];
            if address[..10].iter().all(|byte| *byte == 0) && address[10..12] == [0xff, 0xff] {
                return Ok(Endpoint {
                    ip: [address[12], address[13], address[14], address[15]],
                    port,
                });
            }
            if address.iter().all(|byte| *byte == 0) {
                // `::` binds or connects to "any"; treat as the IPv4 any.
                return Ok(Endpoint { ip: [0; 4], port });
            }
            Err(AddrError::Ipv6Unsupported)
        }
        _ => Err(AddrError::Invalid),
    }
}

/// Encodes `endpoint` for a socket of `domain` and returns the length.
/// `AF_INET6` sockets see IPv4 peers as IPv4-mapped addresses.
pub fn encode(domain: u16, endpoint: Endpoint, out: &mut [u8; SOCKADDR_IN6_LEN]) -> usize {
    *out = [0; SOCKADDR_IN6_LEN];
    out[..2].copy_from_slice(&domain.to_ne_bytes());
    out[2..4].copy_from_slice(&endpoint.port.to_be_bytes());
    if domain == AF_INET6 {
        if endpoint.ip != [0; 4] {
            out[18..20].copy_from_slice(&[0xff, 0xff]);
            out[20..24].copy_from_slice(&endpoint.ip);
        }
        SOCKADDR_IN6_LEN
    } else {
        out[4..8].copy_from_slice(&endpoint.ip);
        SOCKADDR_IN_LEN
    }
}

/// Returns `true` if an application may send to `ip`.
///
/// Refused: "this network" (0/8), loopback (127/8, which has no stack
/// behind it), link-local (169.254/16, which includes cloud metadata
/// services), multicast and reserved space (224/3), and broadcast. Private
/// LAN ranges stay reachable so a router's or printer's page still works.
#[must_use]
pub fn remote_allowed(ip: [u8; 4]) -> bool {
    !(ip[0] == 0 || ip[0] == 127 || (ip[0] == 169 && ip[1] == 254) || ip[0] >= 224)
}

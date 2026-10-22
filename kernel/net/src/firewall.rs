// SPDX-License-Identifier: Apache-2.0
//! Inbound packet filter. Default deny.
//!
//! Every received Ethernet frame passes through [`inbound`] before the stack
//! sees it. A frame is accepted only when it is ARP, answers a flow a local
//! socket opened, is the DHCP reply the interface is waiting for, or is an
//! ICMP error about a flow a local socket opened. Everything else is dropped
//! silently: no TCP reset, no ICMP port-unreachable, and no answer to ping,
//! so a closed port looks the same as an absent host.
//!
//! A flow is exact: protocol, local port, remote address, and remote port.
//! For UDP the remote is a destination the socket has sent to, so a reply
//! from anyone else is dropped.
//!
//! ARP is accepted unfiltered. Address resolution trusts the local segment,
//! as every IPv4 host does; an attacker on the segment can redirect traffic
//! (ARP spoofing) but cannot open a flow through this filter, and TLS is what
//! protects the contents.

/// Transport protocol of a flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

/// One inbound transport packet, from the local socket's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flow {
    pub protocol: Protocol,
    pub local_port: u16,
    pub remote_ip: [u8; 4],
    pub remote_port: u16,
}

/// Filter decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accept,
    Drop,
}

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERNET_HEADER: usize = 14;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;

/// Decides whether `frame` may reach the stack.
///
/// `local_ip` is the interface address, or `None` before DHCP completes.
/// `open` answers whether a local socket owns the flow. `dhcp` says whether
/// the DHCP client is running.
pub fn inbound(
    frame: &[u8],
    local_ip: Option<[u8; 4]>,
    dhcp: bool,
    open: impl Fn(&Flow) -> bool,
) -> Verdict {
    let Some(header) = frame.get(..ETHERNET_HEADER) else {
        return Verdict::Drop;
    };
    match u16::from_be_bytes([header[12], header[13]]) {
        ETHERTYPE_ARP => Verdict::Accept,
        ETHERTYPE_IPV4 => ipv4(&frame[ETHERNET_HEADER..], local_ip, dhcp, open),
        _ => Verdict::Drop,
    }
}

fn ipv4(
    packet: &[u8],
    local_ip: Option<[u8; 4]>,
    dhcp: bool,
    open: impl Fn(&Flow) -> bool,
) -> Verdict {
    if packet.len() < 20 || packet[0] >> 4 != 4 {
        return Verdict::Drop;
    }
    let header_len = usize::from(packet[0] & 0x0f) * 4;
    let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if header_len < 20 || total_len < header_len || total_len > packet.len() {
        return Verdict::Drop;
    }
    // No reassembly: drop every fragment, first or later.
    let fragment = u16::from_be_bytes([packet[6], packet[7]]);
    if fragment & 0x3fff != 0 {
        return Verdict::Drop;
    }
    let source = [packet[12], packet[13], packet[14], packet[15]];
    let destination = [packet[16], packet[17], packet[18], packet[19]];
    let payload = &packet[header_len..total_len];
    let protocol = packet[9];

    // DHCP replies may be broadcast or addressed before the lease is known.
    if protocol == 17
        && dhcp
        && let Some((source_port, destination_port)) = ports(payload, 8)
        && source_port == DHCP_SERVER_PORT
        && destination_port == DHCP_CLIENT_PORT
    {
        return Verdict::Accept;
    }
    // Everything else must be unicast to the configured address.
    if local_ip != Some(destination) {
        return Verdict::Drop;
    }
    match protocol {
        // ICMP: destination unreachable (3) and time exceeded (11), only
        // about a packet one of our flows sent. Nothing local sends echo
        // requests, so echo replies are dropped with everything else.
        1 => match payload.first() {
            Some(3 | 11) => icmp_error(payload, destination, &open),
            _ => Verdict::Drop,
        },
        6 => flow(payload, 20, Protocol::Tcp, source, &open),
        17 => flow(payload, 8, Protocol::Udp, source, &open),
        _ => Verdict::Drop,
    }
}

/// Accepts an ICMP error only when the packet it quotes left through an open
/// flow, so a forged "unreachable" cannot tear down another connection.
fn icmp_error(payload: &[u8], local_ip: [u8; 4], open: &impl Fn(&Flow) -> bool) -> Verdict {
    // Type, code, checksum, and 4 unused bytes precede the quoted packet.
    let Some(quoted) = payload.get(8..) else {
        return Verdict::Drop;
    };
    if quoted.len() < 20 || quoted[0] >> 4 != 4 {
        return Verdict::Drop;
    }
    let header_len = usize::from(quoted[0] & 0x0f) * 4;
    if header_len < 20 || quoted[12..16] != local_ip {
        return Verdict::Drop;
    }
    let protocol = match quoted[9] {
        6 => Protocol::Tcp,
        17 => Protocol::Udp,
        _ => return Verdict::Drop,
    };
    let Some((local_port, remote_port)) = quoted
        .get(header_len..)
        .and_then(|transport| ports(transport, 4))
    else {
        return Verdict::Drop;
    };
    let flow = Flow {
        protocol,
        local_port,
        remote_ip: [quoted[16], quoted[17], quoted[18], quoted[19]],
        remote_port,
    };
    if open(&flow) {
        Verdict::Accept
    } else {
        Verdict::Drop
    }
}

fn ports(payload: &[u8], minimum: usize) -> Option<(u16, u16)> {
    (payload.len() >= minimum).then(|| {
        (
            u16::from_be_bytes([payload[0], payload[1]]),
            u16::from_be_bytes([payload[2], payload[3]]),
        )
    })
}

fn flow(
    payload: &[u8],
    minimum: usize,
    protocol: Protocol,
    source: [u8; 4],
    open: &impl Fn(&Flow) -> bool,
) -> Verdict {
    let Some((remote_port, local_port)) = ports(payload, minimum) else {
        return Verdict::Drop;
    };
    let flow = Flow {
        protocol,
        local_port,
        remote_ip: source,
        remote_port,
    };
    if open(&flow) {
        Verdict::Accept
    } else {
        Verdict::Drop
    }
}

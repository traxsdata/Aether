// SPDX-License-Identifier: Apache-2.0

extern crate std;

use crate::addr::{self, AF_INET, AF_INET6, AddrError, Endpoint};
use crate::consent::{ConsentError, Decision, Decisions};
use crate::firewall::{self, Flow, Protocol, Verdict};
use std::vec::Vec;

fn sockaddr_in(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..2].copy_from_slice(&AF_INET.to_ne_bytes());
    bytes[2..4].copy_from_slice(&port.to_be_bytes());
    bytes[4..8].copy_from_slice(&ip);
    bytes
}

#[test]
fn ipv4_sockaddr_round_trips() {
    let bytes = sockaddr_in([93, 184, 216, 34], 443);
    let endpoint = addr::decode(AF_INET, &bytes).unwrap();
    assert_eq!(
        endpoint,
        Endpoint {
            ip: [93, 184, 216, 34],
            port: 443
        }
    );
    let mut out = [0u8; 28];
    assert_eq!(addr::encode(AF_INET, endpoint, &mut out), 16);
    assert_eq!(&out[..16], &bytes);
}

#[test]
fn ipv6_sockets_use_mapped_ipv4() {
    let mut bytes = [0u8; 28];
    bytes[..2].copy_from_slice(&AF_INET6.to_ne_bytes());
    bytes[2..4].copy_from_slice(&80u16.to_be_bytes());
    bytes[18..20].copy_from_slice(&[0xff, 0xff]);
    bytes[20..24].copy_from_slice(&[10, 0, 2, 2]);
    let endpoint = addr::decode(AF_INET6, &bytes).unwrap();
    assert_eq!(endpoint.ip, [10, 0, 2, 2]);
    let mut out = [0u8; 28];
    assert_eq!(addr::encode(AF_INET6, endpoint, &mut out), 28);
    assert_eq!(out, bytes);

    bytes[8] = 0x20; // 2000::/3, a native IPv6 address
    assert_eq!(
        addr::decode(AF_INET6, &bytes),
        Err(AddrError::Ipv6Unsupported)
    );
}

#[test]
fn malformed_addresses_are_refused() {
    let bytes = sockaddr_in([1, 1, 1, 1], 53);
    assert_eq!(addr::decode(AF_INET, &bytes[..15]), Err(AddrError::Invalid));
    assert_eq!(addr::decode(AF_INET6, &bytes), Err(AddrError::Invalid));
    assert_eq!(addr::decode(AF_INET, &[]), Err(AddrError::Invalid));
    assert_eq!(
        addr::decode(AF_INET, &[0, 0, 0]),
        Err(AddrError::Unspecified)
    );
}

#[test]
fn reachable_address_policy() {
    for allowed in [
        [93, 184, 216, 34],
        [10, 0, 2, 2],
        [192, 168, 1, 1],
        [1, 1, 1, 1],
    ] {
        assert!(addr::remote_allowed(allowed), "{allowed:?}");
    }
    for denied in [
        [0, 0, 0, 0],
        [127, 0, 0, 1],
        [169, 254, 169, 254],
        [224, 0, 0, 251],
        [255, 255, 255, 255],
        [240, 0, 0, 1],
    ] {
        assert!(!addr::remote_allowed(denied), "{denied:?}");
    }
}

const LOCAL: [u8; 4] = [10, 0, 2, 15];
const REMOTE: [u8; 4] = [10, 0, 2, 2];

fn frame(protocol: u8, source: [u8; 4], destination: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut frame = std::vec![0u8; 14];
    frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
    let total = 20 + payload.len();
    let mut header = [0u8; 20];
    header[0] = 0x45;
    header[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    header[8] = 64;
    header[9] = protocol;
    header[12..16].copy_from_slice(&source);
    header[16..20].copy_from_slice(&destination);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(payload);
    frame
}

fn transport(source_port: u16, destination_port: u16, len: usize) -> Vec<u8> {
    let mut payload = std::vec![0u8; len];
    payload[..2].copy_from_slice(&source_port.to_be_bytes());
    payload[2..4].copy_from_slice(&destination_port.to_be_bytes());
    payload
}

fn opened(flow: &Flow) -> bool {
    *flow
        == Flow {
            protocol: Protocol::Tcp,
            local_port: 49152,
            remote_ip: REMOTE,
            remote_port: 80,
        }
}

#[test]
fn firewall_accepts_only_owned_flows() {
    let reply = frame(6, REMOTE, LOCAL, &transport(80, 49152, 20));
    assert_eq!(
        firewall::inbound(&reply, Some(LOCAL), false, opened),
        Verdict::Accept
    );
    // An unsolicited connection to another port, or from another peer.
    let probe = frame(6, REMOTE, LOCAL, &transport(80, 22, 20));
    assert_eq!(
        firewall::inbound(&probe, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    let spoof = frame(6, [10, 0, 2, 3], LOCAL, &transport(80, 49152, 20));
    assert_eq!(
        firewall::inbound(&spoof, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // Addressed to someone else.
    let other = frame(6, REMOTE, [10, 0, 2, 16], &transport(80, 49152, 20));
    assert_eq!(
        firewall::inbound(&other, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // Truncated TCP header.
    let short = frame(6, REMOTE, LOCAL, &transport(80, 49152, 12));
    assert_eq!(
        firewall::inbound(&short, Some(LOCAL), false, opened),
        Verdict::Drop
    );
}

#[test]
fn firewall_icmp_and_dhcp() {
    let echo_request = frame(1, REMOTE, LOCAL, &[8, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        firewall::inbound(&echo_request, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // Nothing local pings, so an echo reply is unsolicited.
    let echo_reply = frame(1, REMOTE, LOCAL, &[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        firewall::inbound(&echo_reply, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // An ICMP error must quote a packet one of our flows sent.
    let quote = |destination: [u8; 4], source_port: u16, destination_port: u16| {
        let mut icmp = std::vec![3u8, 1, 0, 0, 0, 0, 0, 0];
        let original = frame(
            6,
            LOCAL,
            destination,
            &transport(source_port, destination_port, 8),
        );
        icmp.extend_from_slice(&original[14..]);
        icmp
    };
    let router = [10, 0, 2, 1];
    let unreachable = frame(1, router, LOCAL, &quote(REMOTE, 49152, 80));
    assert_eq!(
        firewall::inbound(&unreachable, Some(LOCAL), false, opened),
        Verdict::Accept
    );
    let forged = frame(1, router, LOCAL, &quote(REMOTE, 49153, 80));
    assert_eq!(
        firewall::inbound(&forged, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    let elsewhere = frame(1, router, LOCAL, &quote([10, 0, 2, 9], 49152, 80));
    assert_eq!(
        firewall::inbound(&elsewhere, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    let bare = frame(1, router, LOCAL, &[3, 1, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        firewall::inbound(&bare, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    let full = quote(REMOTE, 49152, 80);
    for len in 0..full.len() {
        let _ = firewall::inbound(
            &frame(1, router, LOCAL, &full[..len]),
            Some(LOCAL),
            false,
            opened,
        );
    }

    let offer = frame(17, REMOTE, [255; 4], &transport(67, 68, 8));
    assert_eq!(
        firewall::inbound(&offer, None, true, opened),
        Verdict::Accept
    );
    assert_eq!(
        firewall::inbound(&offer, None, false, opened),
        Verdict::Drop
    );
    let forged = frame(17, REMOTE, [255; 4], &transport(53, 68, 8));
    assert_eq!(
        firewall::inbound(&forged, None, true, opened),
        Verdict::Drop
    );
}

#[test]
fn firewall_rejects_malformed_frames() {
    assert_eq!(
        firewall::inbound(&[0; 10], Some(LOCAL), true, |_| true),
        Verdict::Drop
    );
    let mut arp = std::vec![0u8; 42];
    arp[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    assert_eq!(
        firewall::inbound(&arp, Some(LOCAL), false, |_| false),
        Verdict::Accept
    );
    let mut ipv6 = std::vec![0u8; 60];
    ipv6[12..14].copy_from_slice(&0x86ddu16.to_be_bytes());
    assert_eq!(
        firewall::inbound(&ipv6, Some(LOCAL), true, |_| true),
        Verdict::Drop
    );

    let good = frame(6, REMOTE, LOCAL, &transport(80, 49152, 20));
    // Total length beyond the frame.
    let mut long = good.clone();
    long[16..18].copy_from_slice(&200u16.to_be_bytes());
    assert_eq!(
        firewall::inbound(&long, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // A fragment.
    let mut fragment = good.clone();
    fragment[20] = 0x20;
    assert_eq!(
        firewall::inbound(&fragment, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // Header length under 20 bytes.
    let mut ihl = good;
    ihl[14] = 0x44;
    assert_eq!(
        firewall::inbound(&ihl, Some(LOCAL), false, opened),
        Verdict::Drop
    );
    // Every prefix of a valid frame must be handled without panicking.
    let valid = frame(17, REMOTE, LOCAL, &transport(53, 40000, 8));
    for len in 0..valid.len() {
        let _ = firewall::inbound(&valid[..len], Some(LOCAL), true, |_| true);
    }
}

#[test]
fn consent_records_round_trip() {
    let mut decisions = Decisions::new();
    assert_eq!(decisions.get("org.mozilla.firefox"), None);
    decisions
        .set("org.mozilla.firefox", Decision::Allow)
        .unwrap();
    decisions.set("com.example.app", Decision::Deny).unwrap();
    decisions
        .set("org.mozilla.firefox", Decision::Deny)
        .unwrap();
    let mut out = [0u8; 256];
    let len = decisions.write(&mut out).unwrap();
    let parsed = Decisions::parse(&out[..len]).unwrap();
    assert_eq!(parsed.get("org.mozilla.firefox"), Some(Decision::Deny));
    assert_eq!(parsed.get("com.example.app"), Some(Decision::Deny));
    let mut parsed = parsed;
    parsed.forget("com.example.app");
    assert_eq!(parsed.get("com.example.app"), None);
    assert_eq!(parsed.iter().count(), 1);
}

#[test]
fn consent_parser_fails_closed() {
    for bad in [
        &b"org.example=yes\n"[..],
        b"org.example\n",
        b"Org.Example=allow\n",
        b"org.example=allow\norg.example=deny\n",
        b"=allow\n",
        b"org/../x=allow\n",
        b"\xff=allow\n",
    ] {
        assert_eq!(
            Decisions::parse(bad).err(),
            Some(ConsentError::Malformed),
            "{bad:?}"
        );
    }
    let mut decisions = Decisions::new();
    for index in 0..crate::consent::MAX_DECISIONS {
        decisions
            .set(&std::format!("org.example.app{index}"), Decision::Allow)
            .unwrap();
    }
    assert_eq!(
        decisions.set("org.example.overflow", Decision::Allow),
        Err(ConsentError::Full)
    );
}

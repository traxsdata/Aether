// SPDX-License-Identifier: Apache-2.0
//! Network interface: NIC, TCP/IP stack, DHCP, and the inbound firewall.
//!
//! One Ethernet interface is supported. At boot [`init`] finds an Intel
//! e1000/e1000e NIC, starts it, and creates a `smoltcp` interface with a
//! DHCPv4 client. [`poll`] runs the stack; the scheduler calls it on every
//! timer tick and from the idle loop, and the socket layer calls it after
//! every send so data leaves without waiting for the next tick.
//!
//! Every received frame goes through [`aether_net::firewall::inbound`]
//! before the stack sees it. The firewall is given the flows the local
//! sockets own, so unsolicited traffic is dropped silently.
//!
//! Locking: the state is used from Linux syscalls and the timer interrupt
//! (both with interrupts masked) and from the idle loop under an
//! [`InterruptGuard`], all on the single CPU.

pub mod consent;
pub mod e1000;
pub mod pci;

use crate::arch::x86_64::timer::InterruptGuard;
use crate::serial_line;
use aether_net::firewall::{self, Flow, Protocol, Verdict};
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use smoltcp::iface::{Config, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr, Ipv4Address, Ipv4Cidr};

/// Interface MTU (Ethernet payload).
pub const MTU: usize = 1500;
/// Ephemeral port range for outbound sockets (IANA dynamic ports).
const EPHEMERAL_FIRST: u16 = 49152;
/// How long a closed TCP socket may take to finish its shutdown (10 s).
const RETIRE_TICKS: u64 = 1000;
/// Destinations remembered per UDP socket for the inbound filter.
const UDP_PEERS: usize = 8;

struct Global<T>(UnsafeCell<T>);

// SAFETY: See the module documentation on locking.
unsafe impl<T> Sync for Global<T> {}

static NET: Global<Option<Net>> = Global(UnsafeCell::new(None));

/// Live interface state.
pub struct Net {
    iface: Interface,
    nic: e1000::E1000,
    pub sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    address: Option<Ipv4Cidr>,
    gateway: Option<Ipv4Address>,
    dns: Vec<Ipv4Address>,
    next_port: u16,
    /// Closed TCP sockets finishing their shutdown, with the tick after
    /// which they are dropped regardless.
    retired: Vec<(SocketHandle, u64)>,
    /// Remote endpoints each UDP socket has sent to; only they may answer.
    udp_peers: Vec<(SocketHandle, Flow)>,
    /// Reused flow list for the firewall, so polling does not allocate once
    /// it has grown to the number of open flows.
    flow_cache: Vec<Flow>,
    link_reported: bool,
    /// Inbound frames the firewall dropped.
    pub firewall_drops: u64,
    #[cfg(feature = "net-selftest")]
    selftest: selftest::State,
}

/// Monotonic stack time from the scheduler tick.
fn now() -> Instant {
    Instant::from_millis(
        crate::scheduler::ticks() as i64 * (1000 / i64::from(crate::arch::x86_64::timer::TIMER_HZ)),
    )
}

/// Finds and starts the NIC. Without a supported NIC the system runs
/// offline and every Internet socket reports `ENETUNREACH`.
pub fn init() {
    let Some(found) = e1000::probe() else {
        serial_line!("net: no supported network adapter; offline");
        return;
    };
    let address = found.function.address;
    let mut nic = match e1000::E1000::init(&found) {
        Ok(nic) => nic,
        Err(error) => {
            serial_line!(
                "net: {} at {:02x}:{:02x}.{} failed: {:?}",
                found.model,
                address.bus,
                address.device,
                address.function,
                error
            );
            return;
        }
    };
    let mac = nic.mac;
    serial_line!(
        "net: {} {:04x}:{:04x} at {:02x}:{:02x}.{} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        found.model,
        found.function.vendor,
        found.function.device,
        address.bus,
        address.device,
        address.function,
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5]
    );
    let mut seed = [0u8; 8];
    crate::random::fill(&mut seed);
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = u64::from_ne_bytes(seed);
    let mut device = NicDevice {
        nic: &mut nic,
        flows: &[],
        local: None,
        dhcp: true,
        drops: 0,
    };
    let iface = Interface::new(config, &mut device, now());
    let mut sockets = SocketSet::new(Vec::new());
    let dhcp = sockets.add(dhcpv4::Socket::new());
    let mut port = [0u8; 2];
    crate::random::fill(&mut port);
    let next_port = EPHEMERAL_FIRST + u16::from_ne_bytes(port) % (u16::MAX - EPHEMERAL_FIRST);
    // SAFETY: Boot is single-threaded and this is the only writer.
    unsafe {
        *NET.0.get() = Some(Net {
            iface,
            nic,
            sockets,
            dhcp,
            address: None,
            gateway: None,
            dns: Vec::new(),
            next_port,
            retired: Vec::new(),
            udp_peers: Vec::new(),
            flow_cache: Vec::new(),
            link_reported: false,
            firewall_drops: 0,
            #[cfg(feature = "net-selftest")]
            selftest: selftest::State::default(),
        });
    }
}

/// Runs `f` on the interface, or returns `None` when the system is offline.
pub fn with<R>(f: impl FnOnce(&mut Net) -> R) -> Option<R> {
    let _guard = InterruptGuard::enter();
    // SAFETY: Interrupts are masked for the borrow; see module docs. Callers
    // never re-enter `with` from inside `f`.
    unsafe { (*NET.0.get()).as_mut() }.map(f)
}

/// Polls the stack once. Wakes poll/epoll and blocked socket waiters when
/// any socket changed state.
pub fn poll() {
    let changed = with(Net::poll).unwrap_or(false);
    #[cfg(feature = "net-consent-test")]
    consent::test_step();
    if changed {
        crate::scheduler::linux_wake_pollers();
    }
}

impl Net {
    /// Runs the stack and handles DHCP. Returns whether sockets changed.
    pub fn poll(&mut self) -> bool {
        if !self.link_reported && self.nic.link_up() {
            self.link_reported = true;
            serial_line!("AETHER_NET_LINK_OK model={}", self.nic.model);
        }
        let mut flows = core::mem::take(&mut self.flow_cache);
        self.collect_flows(&mut flows);
        let mut device = NicDevice {
            nic: &mut self.nic,
            flows: &flows,
            local: self.address.map(|cidr| cidr.address().octets()),
            dhcp: true,
            drops: 0,
        };
        let result = self.iface.poll(now(), &mut device, &mut self.sockets);
        self.firewall_drops += device.drops;
        self.flow_cache = flows;
        let event = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll();
        match event {
            Some(dhcpv4::Event::Configured(config)) => {
                let address = config.address;
                let router = config.router;
                let dns: Vec<Ipv4Address> = config.dns_servers.iter().copied().collect();
                self.configure(address, router, dns);
            }
            // The client reports Deconfigured once at start; only a lost
            // lease is worth a line.
            Some(dhcpv4::Event::Deconfigured) if self.address.is_none() => {}
            Some(dhcpv4::Event::Deconfigured) => {
                serial_line!("net: dhcp lease lost");
                self.address = None;
                self.gateway = None;
                self.dns.clear();
                self.iface.update_ip_addrs(|addresses| addresses.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
            }
            None => {}
        }
        self.reap();
        #[cfg(feature = "net-selftest")]
        selftest::step(self);
        result == PollResult::SocketStateChanged
    }

    fn configure(&mut self, address: Ipv4Cidr, router: Option<Ipv4Address>, dns: Vec<Ipv4Address>) {
        self.iface.update_ip_addrs(|addresses| {
            addresses.clear();
            let _ = addresses.push(IpCidr::Ipv4(address));
        });
        match router {
            Some(router) => {
                let _ = self.iface.routes_mut().add_default_ipv4_route(router);
            }
            None => {
                self.iface.routes_mut().remove_default_ipv4_route();
            }
        }
        self.address = Some(address);
        self.gateway = router;
        self.dns = dns;
        let mut servers = [0u8; 64];
        let length = self.write_nameservers(&mut servers, " ");
        serial_line!(
            "AETHER_NET_DHCP_OK address={} gateway={} dns={}",
            address,
            router.unwrap_or(Ipv4Address::UNSPECIFIED),
            core::str::from_utf8(&servers[..length])
                .unwrap_or("")
                .trim()
        );
    }

    /// The local flows the firewall lets back in: every open TCP socket's
    /// connection and every destination a UDP socket has sent to.
    fn collect_flows(&self, flows: &mut Vec<Flow>) {
        flows.clear();
        for (_, socket) in self.sockets.iter() {
            if let smoltcp::socket::Socket::Tcp(socket) = socket
                && let (Some(local), Some(remote)) =
                    (socket.local_endpoint(), socket.remote_endpoint())
                && let smoltcp::wire::IpAddress::Ipv4(ip) = remote.addr
            {
                flows.push(Flow {
                    protocol: Protocol::Tcp,
                    local_port: local.port,
                    remote_ip: ip.octets(),
                    remote_port: remote.port,
                });
            }
        }
        flows.extend(self.udp_peers.iter().map(|(_, flow)| *flow));
    }

    /// Records that the UDP socket `handle` sent to `remote`, so its reply is
    /// let in. Each socket keeps its most recent [`UDP_PEERS`] destinations.
    pub fn note_udp_peer(&mut self, handle: SocketHandle, remote_ip: [u8; 4], remote_port: u16) {
        let local_port = self.udp(handle).endpoint().port;
        let flow = Flow {
            protocol: Protocol::Udp,
            local_port,
            remote_ip,
            remote_port,
        };
        if self
            .udp_peers
            .iter()
            .any(|(owner, known)| *owner == handle && *known == flow)
        {
            return;
        }
        let owned = self
            .udp_peers
            .iter()
            .filter(|(owner, _)| *owner == handle)
            .count();
        if owned >= UDP_PEERS
            && let Some(oldest) = self
                .udp_peers
                .iter()
                .position(|(owner, _)| *owner == handle)
        {
            self.udp_peers.remove(oldest);
        }
        self.udp_peers.push((handle, flow));
    }

    /// Removes a UDP socket and the destinations it was answering.
    pub fn remove_udp(&mut self, handle: SocketHandle) {
        self.udp_peers.retain(|(owner, _)| *owner != handle);
        self.sockets.remove(handle);
    }

    /// The interface address, once DHCP configured one.
    #[must_use]
    pub fn address(&self) -> Option<Ipv4Address> {
        self.address.map(|cidr| cidr.address())
    }

    /// Picks a free ephemeral port.
    pub fn ephemeral_port(&mut self) -> u16 {
        let span = u16::MAX - EPHEMERAL_FIRST;
        for _ in 0..span {
            let port = self.next_port;
            self.next_port = EPHEMERAL_FIRST + (self.next_port - EPHEMERAL_FIRST + 1) % span;
            if !self.port_in_use(port) {
                return port;
            }
        }
        0
    }

    fn port_in_use(&self, port: u16) -> bool {
        self.sockets.iter().any(|(_, socket)| match socket {
            smoltcp::socket::Socket::Tcp(socket) => socket
                .local_endpoint()
                .is_some_and(|local| local.port == port),
            smoltcp::socket::Socket::Udp(socket) => socket.endpoint().port == port,
            _ => false,
        })
    }

    /// The stack context needed by `tcp::Socket::connect`, together with
    /// the socket.
    pub fn tcp_with_context(
        &mut self,
        handle: SocketHandle,
    ) -> (&mut tcp::Socket<'static>, &mut smoltcp::iface::Context) {
        (
            self.sockets.get_mut::<tcp::Socket>(handle),
            self.iface.context(),
        )
    }

    /// Takes over a TCP socket whose last descriptor closed. A connection
    /// sends its FIN and is dropped once closed; one still opening is
    /// reset. Either way the slot is freed within [`RETIRE_TICKS`].
    pub fn retire_tcp(&mut self, handle: SocketHandle) {
        let socket = self.tcp(handle);
        match socket.state() {
            tcp::State::Closed | tcp::State::Listen => {
                self.sockets.remove(handle);
                return;
            }
            tcp::State::SynSent | tcp::State::SynReceived => socket.abort(),
            _ => socket.close(),
        }
        self.retired
            .push((handle, crate::scheduler::ticks() + RETIRE_TICKS));
    }

    fn reap(&mut self) {
        if self.retired.is_empty() {
            return;
        }
        let now = crate::scheduler::ticks();
        let mut index = 0;
        while index < self.retired.len() {
            let (handle, deadline) = self.retired[index];
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);
            let done = matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait);
            if done || now >= deadline {
                if !done {
                    socket.abort();
                }
                self.sockets.remove(handle);
                self.retired.swap_remove(index);
            } else {
                index += 1;
            }
        }
    }

    pub fn tcp(&mut self, handle: SocketHandle) -> &mut tcp::Socket<'static> {
        self.sockets.get_mut::<tcp::Socket>(handle)
    }

    pub fn udp(&mut self, handle: SocketHandle) -> &mut udp::Socket<'static> {
        self.sockets.get_mut::<udp::Socket>(handle)
    }

    /// Writes the DHCP name servers separated by `separator`.
    fn write_nameservers(&self, out: &mut [u8], separator: &str) -> usize {
        let mut text = Text { out, len: 0 };
        for server in &self.dns {
            let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{server}{separator}"));
        }
        text.len
    }
}

/// `/etc/resolv.conf` for Linux applications: the DHCP name servers.
pub fn resolv_conf(out: &mut [u8]) -> usize {
    with(|net| {
        let mut text = Text { out, len: 0 };
        let _ = core::fmt::Write::write_str(&mut text, "# generated by aether from dhcp\n");
        for server in &net.dns {
            let _ = core::fmt::Write::write_fmt(&mut text, format_args!("nameserver {server}\n"));
        }
        let _ = core::fmt::Write::write_str(&mut text, "options timeout:2 attempts:2\n");
        text.len
    })
    .unwrap_or(0)
}

/// One-line status for diagnostics.
pub fn status(out: &mut [u8]) -> usize {
    with(|net| {
        let mut text = Text { out, len: 0 };
        let _ = core::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "{} link={} address={} gateway={} sockets={} dropped={}+{}",
                net.nic.model,
                if net.nic.link_up() { "up" } else { "down" },
                net.address
                    .map_or(Ipv4Address::UNSPECIFIED, |address| address.address()),
                net.gateway.unwrap_or(Ipv4Address::UNSPECIFIED),
                net.sockets.iter().count().saturating_sub(1),
                net.firewall_drops,
                net.nic.rx.dropped,
            ),
        );
        text.len
    })
    .unwrap_or(0)
}

struct Text<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for Text<'_> {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(core::fmt::Error)?;
        let target = self.out.get_mut(self.len..end).ok_or(core::fmt::Error)?;
        target.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// The NIC as a `smoltcp` device, with the firewall in the receive path.
struct NicDevice<'a> {
    nic: &'a mut e1000::E1000,
    flows: &'a [Flow],
    local: Option<[u8; 4]>,
    dhcp: bool,
    drops: u64,
}

impl NicDevice<'_> {
    fn allowed(flows: &[Flow], flow: &Flow) -> bool {
        flows.contains(flow)
    }
}

impl Device for NicDevice<'_> {
    type RxToken<'a>
        = NicRx<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = NicTx<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        loop {
            let verdict = {
                let frame = self.nic.rx.peek()?;
                firewall::inbound(frame, self.local, self.dhcp, |flow| {
                    Self::allowed(self.flows, flow)
                })
            };
            if verdict == Verdict::Accept {
                break;
            }
            self.drops += 1;
            self.nic.rx.release();
        }
        Some((
            NicRx {
                rx: &mut self.nic.rx,
            },
            NicTx {
                tx: &mut self.nic.tx,
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        let ready = self.nic.tx.ready();
        ready.then_some(NicTx {
            tx: &mut self.nic.tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = e1000::MAX_FRAME;
        capabilities.max_burst_size = Some(e1000::RING / 2);
        capabilities
    }
}

struct NicRx<'a> {
    rx: &'a mut e1000::Rx,
}

impl RxToken for NicRx<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        // `receive` only hands out a token after `peek` found a frame.
        let result = f(self.rx.peek().unwrap_or(&[]));
        self.rx.release();
        result
    }
}

struct NicTx<'a> {
    tx: &'a mut e1000::Tx,
}

impl TxToken for NicTx<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        self.tx.send(len, f)
    }
}

/// Boot self-test: fetch a page from the QEMU host over TCP.
#[cfg(feature = "net-selftest")]
mod selftest {
    use super::Net;
    use crate::serial_line;
    use alloc::vec;
    use smoltcp::socket::tcp;
    use smoltcp::wire::{IpAddress, Ipv4Address};

    /// The QEMU user-mode network's alias for the host.
    const HOST: Ipv4Address = Ipv4Address::new(10, 0, 2, 2);
    const PORT: u16 = match option_env!("AETHER_NET_TEST_PORT") {
        Some(port) => match u16::from_str_radix(port, 10) {
            Ok(port) => port,
            Err(_) => 8080,
        },
        None => 8080,
    };
    const REQUEST: &[u8] = b"GET /aether-net-test HTTP/1.0\r\nHost: 10.0.2.2\r\n\r\n";
    const EXPECT: &[u8] = b"AETHER_NET_PAYLOAD";

    #[derive(Default)]
    pub struct State {
        handle: Option<smoltcp::iface::SocketHandle>,
        sent: bool,
        received: usize,
        matched: bool,
        window: [u8; 32],
        done: bool,
    }

    pub fn step(net: &mut Net) {
        if net.selftest.done || net.address.is_none() {
            return;
        }
        let Some(handle) = net.selftest.handle else {
            let socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 16384]),
                tcp::SocketBuffer::new(vec![0; 4096]),
            );
            let handle = net.sockets.add(socket);
            let port = net.ephemeral_port();
            let (socket, context) = net.tcp_with_context(handle);
            if socket
                .connect(context, (IpAddress::Ipv4(HOST), PORT), port)
                .is_err()
            {
                serial_line!("AETHER_NET_TCP_FAILED reason=connect");
                net.selftest.done = true;
                return;
            }
            net.selftest.handle = Some(handle);
            return;
        };
        let state = &mut net.selftest;
        let socket = net.sockets.get_mut::<tcp::Socket>(handle);
        if !state.sent && socket.can_send() {
            let _ = socket.send_slice(REQUEST);
            state.sent = true;
        }
        let mut buffer = [0u8; 1024];
        while socket.can_recv() {
            let Ok(count) = socket.recv_slice(&mut buffer) else {
                break;
            };
            if count == 0 {
                break;
            }
            for byte in &buffer[..count] {
                state.window.copy_within(1.., 0);
                *state.window.last_mut().unwrap_or(&mut 0) = *byte;
                if state.window.ends_with(EXPECT) {
                    state.matched = true;
                }
            }
            state.received += count;
        }
        if state.sent && !socket.may_recv() {
            socket.close();
            state.done = true;
            if state.matched {
                serial_line!(
                    "AETHER_NET_TCP_OK host=10.0.2.2:{PORT} bytes={}",
                    state.received
                );
            } else {
                serial_line!(
                    "AETHER_NET_TCP_FAILED reason=payload bytes={}",
                    state.received
                );
            }
        } else if socket.state() == tcp::State::Closed && !state.sent {
            state.done = true;
            serial_line!("AETHER_NET_TCP_FAILED reason=refused");
        }
    }
}

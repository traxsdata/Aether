# ADR-018: Networking and the network permission

- **Status:** Accepted; landing in stages (socket layer first)
- **Date:** 2026-10-22
- **Supersedes in part:** the "network denied" decision D-08 in the
  [host-interface inventory](../specifications/linux-host-interface-inventory.md#decisions)
  and the network clause of [ADR-013](ADR-013-application-packaging.md)
- **Related:** [ADR-006](ADR-006-application-isolation.md),
  [ADR-017](ADR-017-linux-app-runtime.md)

## Context

Aether had no network. Every socket call returned `EACCES`, and the package
format had no network permission at all. A browser (Firefox, run through the
Linux runtime) needs outbound TCP, and the system needs a driver, a TCP/IP
stack, DHCP, and DNS configuration to provide it.

The security goals do not change: an application gets nothing by default, a
signature is not consent, and the user decides what reaches the network.
The usability goal is that a browser the user chose to run works without
configuration.

## Decision

1. **Permission model.** Network access is a capability with two keys:
   - The application's signed import record must declare `network=client`.
     This is publisher intent and grants nothing by itself. `client` is the
     only value: there is deliberately no listening or "server" policy.
   - The user approves it on first use. The first Internet socket an eligible
     application creates asks the user; the answer is stored per application
     and can be changed in Security Center.

   An application without the field gets `EACCES` for every Internet socket,
   exactly as before. `bind`, `listen`, and `accept` on Internet sockets are
   refused for `network=client`.
2. **Inbound firewall.** Inbound connections are dropped by default. No
   listening Internet socket exists for applications, unsolicited TCP and UDP
   are dropped without a reply, and ICMP echo requests are not answered.
3. **One socket layer.** `kernel/src/socket/` decodes the Linux socket calls
   once and owns a global socket table. A descriptor names a slot
   (`LinuxFileSource::Socket`); `dup`, `fork`, and threads share it. Families
   implement one interface: `AF_UNIX` (local socket pairs, `SCM_RIGHTS`) in
   `socket/unix.rs`, `AF_INET`/`AF_INET6` in `socket/inet.rs`. Other families,
   `AF_NETLINK` included, return `EAFNOSUPPORT`.
4. **Stack.** Use `smoltcp`, a memory-safe, `no_std`, widely used TCP/IP
   stack, rather than writing one. Packet parsing is the largest attack
   surface in networking, and a vetted safe-Rust implementation is the better
   trade than new code.
5. **Drivers.** Start with the Intel e1000/e1000e family, which is the default
   NIC of both QEMU q35 and VMware Workstation, then virtio-net. Drivers copy
   frames between the stack and device-owned DMA buffers; the device never
   sees kernel objects outside its own rings and buffers.
6. **Configuration.** DHCPv4 configures the interface. Linux applications see
   a generated `/etc/resolv.conf` naming the DHCP DNS server; name resolution
   happens inside the application (glibc or Firefox's own resolver).

## Consequences

- The socket layer, driver, and stack run in ring 0, like the rest of the
  Linux ABI today (see the [ADR-017 deviation](ADR-017-linux-app-runtime.md#current-deviation-linux-abi-in-ring-0)).
  Moving them into a network service is follow-up work that keeps the same
  socket interface.
- Without an IOMMU a NIC can DMA anywhere in memory. This is a property of
  the hardware platform, recorded in the threat model; emulated NICs in QEMU
  and VMware are part of the trusted hypervisor.
- `SIGPIPE` is not raised by socket writes; they return `EPIPE`.

# Networking and the firewall

æther 0.5 can reach the network. The design is one-way: **apps open
connections; nothing can open one to æther.**

## What's there

| Layer | File | Notes |
|---|---|---|
| Card driver | `kernel/e1000.c` | Intel PRO/1000: 82540EM/82545EM (`e1000`) and 82574L (`e1000e`), what QEMU and VMware emulate. Polled, like the disk driver. |
| Ethernet, ARP, IPv4 | `kernel/net.c` | No fragments, no ICMP replies. |
| DHCP | `kernel/net.c` | Gets the address, router and DNS server; renews at half the lease. |
| DNS | `kernel/net.c` | A records only, from the DHCP-given server, random id and source port per query, strict parsing, a small cache (30 s to 1 h). |
| TCP | `kernel/net.c` | Client only: random initial sequence numbers and ports, retransmission with backoff, RFC 5961 checks against blind resets and injected SYNs. |
| Firewall | `kernel/firewall.c` | Rules by app and host name, a built-in web policy for the browser, and a kernel dialog for anything else. |

Apps use five system calls (`include/abi.h`): `SYS_NET_CONNECT` (host name,
port), `SYS_NET_SEND`, `SYS_NET_RECV`, `SYS_NET_CLOSE` and `SYS_NET_INFO`.
They need `CAP_NET`. Encryption (TLS) is the app's job; the browser brings
its own (see [BROWSER.md](BROWSER.md)).

## Inbound: nothing

There are no listening sockets, no UDP services and no ping replies. A
packet that isn't part of a connection an app opened, the DHCP exchange,
or the answer to a DNS query in flight is dropped without a reply, so a
scan can't even tell æther is there. `net` in the Terminal shows how many
packets were dropped.

## Outbound: checked before the first packet

When an app calls `SYS_NET_CONNECT`, in this order:

1. **Data loss prevention.** An app that has read Confidential or Secret
   data can't connect, and can't send on a connection it already has
   ([DLP.md](DLP.md)).
2. **The firewall.** The first rule matching the app, host and port
   decides. If none does, the built-in web policy lets the browser reach
   ports 443 (https) and 80 (http) on any host without asking, since that is
   what a browser is for and a prompt per site only teaches you to click
   Allow. Your own rules come first, so `fw deny browser tracker.example`
   still blocks a site, and the address and DLP checks still apply. Anything
   else, any other app or port, the kernel asks you:

   > **Let browser connect to example.com?**
   > *Block* · *Allow once* · *Always allow*

   *Block* is the default button. *Allow once* lasts until that app exits.
   *Always allow* adds a rule. An app refused three times isn't asked again
   until it restarts, so it can't nag. The dialog is drawn by the kernel
   like the file picker, so an app can't fake or click it.
   The check comes before the DNS lookup, so a blocked name never even
   leaves the machine.
3. **The address.** The name is resolved, and the address must be a public
   one: never loopback, `0.0.0.0/8`, link-local (`169.254/16`, where cloud
   metadata services live), multicast or broadcast. LAN addresses
   (`10/8`, `172.16/12`, `192.168/16`, `100.64/10`) and real IANA
   special-use ranges (`192.0.0/24`, `192.88.99/24`, `198.18/15`) are
   allowed only when typed as such, never when a name resolves to one; that
   stops DNS rebinding, where a web page's name suddenly points at your
   router or a metadata service. TEST-NET ranges (`192.0.2/24`,
   `198.51.100/24`, `203.0.113/24`) are allowed by name as well: they are
   never assigned on real networks and exist for local test harnesses.

UDP replies (DHCP and DNS) must carry a correct checksum when they have one;
a DNS A record is accepted only for the queried name or a name reached through
the answer's CNAME chain. A literal LAN or special-use address always gets a
kernel consent dialog, even for the browser, so a page cannot reach the
router or a local service through the built-in web policy. An untrusted app
sees whether there is a link, but not the machine's addresses, DNS server or
MAC.

## Rules

Rules live in `/system/firewall` on the encrypted volume. No app can open,
list or change `/system`; only the kernel and the Terminal's `fw` command
(through `SYS_FIREWALL`, which only system apps may call) touch them.

```
fw                                  list the rules, with how often each matched
fw allow browser *.example.com 443  allow names under example.com, port 443
fw deny * tracker.example           block a host for every app
fw rm 2                             remove rule 2
```

A host can be `*`, `*.example.com` (names under it, not example.com
itself) or an exact name; a port of 0 or none means any. Rules for a
specific app and host come before wildcard ones, so a final `deny * *` is
possible. With temporary storage (no disk), rules last until shutdown.
`fw` lists the built-in web policy after your rules (marked *built in*),
with its hit counts; it can't be removed, but any rule of yours overrides
it.

## Static addresses

Settings can replace DHCP with a static IPv4 address, netmask, router and
DNS server. `SYS_NET_CONFIG` applies the change immediately, stops the DHCP
timers and saves the four values in `/system/settings`; they are applied again
at the next unlock. Switching back to DHCP restarts the client. `net` in the
Terminal shows the mode.

Existing TCP connections are not migrated when the address changes; new
connections use the new address. The kernel validates the mode and requires a
non-zero address and netmask.

## Trying it

- `net` shows the card, address, router, DNS and traffic.
- `get <host> [path]` fetches a page over plain HTTP, to check the path end
  to end. The first time, the firewall asks (only the browser has the
  built-in web policy).
- `test net` runs the unit tests: checksums, host-name and address checks,
  the DNS parser against hostile replies, and firewall patterns.
- `tools/test-https.sh` runs the browser inside QEMU against an HTTPS
  server on the host: a real TLS 1.3 handshake through æther's stack and
  firewall (with a throwaway CA trusted only by that test build).
- `tools/test-real-sites.sh` runs it against real sites on the internet,
  checked against the built-in roots (see [BROWSER.md](BROWSER.md)).
- `tests/net.txt` drives all of it in QEMU against a web server and a fake
  DNS server on the host (`tools/test-dns.py`); see the comments at its top.

In VMware the adapter must be `e1000` or `e1000e` on NAT; see
[VMWARE.md](VMWARE.md#networking-05).

## Not yet

IPv6, TCP window scaling and selective acknowledgements (downloads are
limited to 64 KiB in flight), DNS over TLS, an interrupt-driven driver,
Wi-Fi.

#!/usr/bin/env python3
"""A tiny DNS responder for the network test (tests/net.txt): answers A
queries from a fixed table on 127.0.0.1:53. QEMU's user network forwards the
guest's DNS (10.0.2.3) to the host's resolver, so point /etc/resolv.conf at
127.0.0.1 while the test runs.

    aether.test  -> 203.0.113.2   (the host, with QEMU's user network moved to 203.0.113.0/24)
    rebind.test  -> 192.168.1.1   (a LAN address behind a name: æther must refuse it)
    anything else -> NXDOMAIN
"""
import socket
import struct

TABLE = {"aether.test": "203.0.113.2", "rebind.test": "192.168.1.1"}

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", 53))
while True:
    q, addr = s.recvfrom(512)
    if len(q) < 12:
        continue
    i, labels = 12, []
    while i < len(q) and q[i]:
        labels.append(q[i + 1:i + 1 + q[i]].decode(errors="replace"))
        i += 1 + q[i]
    name, question = ".".join(labels).lower(), q[12:i + 5]
    ip = TABLE.get(name)
    flags = 0x8180 if ip else 0x8183
    r = q[:2] + struct.pack(">HHHHH", flags, 1, 1 if ip else 0, 0, 0) + question
    if ip:
        r += struct.pack(">HHHIH", 0xC00C, 1, 1, 60, 4) + socket.inet_aton(ip)
    s.sendto(r, addr)

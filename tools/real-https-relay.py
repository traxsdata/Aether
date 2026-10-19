#!/usr/bin/env python3
"""Lets æther in QEMU reach real HTTPS sites from a sandbox whose only way
out is an HTTP proxy (such as a cloud dev container), so the browser can be
tested against real servers and their real certificates.

Inside such a sandbox, direct connections are intercepted and re-signed by
a gateway, which the browser rightly refuses; CONNECT through the proxy is
passed through untouched. So this runs two small services on the host:

    DNS  127.0.0.1:53   answers every A query with 203.0.113.2 (the host,
                        with QEMU's user network moved to 203.0.113.0/24)
    TLS  127.0.0.1:443  reads the server name (SNI) from each ClientHello
                        and tunnels the connection, untouched, through
                        $HTTPS_PROXY with CONNECT name:443

The relay never sees plaintext: TLS still runs end to end between the
browser and the real server. On a machine with direct internet access none
of this is needed; QEMU's user network reaches the internet by itself.

    tools/real-https-relay.py       (needs root for ports 53 and 443; point
                                     /etc/resolv.conf at 127.0.0.1 meanwhile)
Used by tools/test-real-sites.sh.
"""
import os
import socket
import struct
import sys
import threading

HOST_IP = "203.0.113.2"


def dns():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("127.0.0.1", 53))
    while True:
        q, addr = s.recvfrom(512)
        if len(q) < 12:
            continue
        i = 12
        while i < len(q) and q[i]:
            i += 1 + q[i]
        if i + 5 > len(q):
            continue
        question = q[12:i + 5]
        qtype = struct.unpack(">H", q[i + 1:i + 3])[0]
        answer = qtype == 1  # A; anything else gets an empty answer
        r = q[:2] + struct.pack(">HHHHH", 0x8180, 1, 1 if answer else 0, 0, 0) + question
        if answer:
            r += struct.pack(">HHHIH", 0xC00C, 1, 1, 30, 4) + socket.inet_aton(HOST_IP)
        s.sendto(r, addr)


def read_exact(c, n):
    b = b""
    while len(b) < n:
        d = c.recv(n - len(b))
        if not d:
            raise EOFError
        b += d
    return b


def sni(hello):
    """The host_name from a ClientHello handshake message, or None."""
    try:
        i = 4 + 2 + 32
        i += 1 + hello[i]  # session id
        i += 2 + struct.unpack(">H", hello[i:i + 2])[0]  # cipher suites
        i += 1 + hello[i]  # compression
        end = i + 2 + struct.unpack(">H", hello[i:i + 2])[0]
        i += 2
        while i + 4 <= end:
            t, n = struct.unpack(">HH", hello[i:i + 4])
            i += 4
            if t == 0:
                j = i + 2 + 1  # list length, name type
                ln = struct.unpack(">H", hello[j:j + 2])[0]
                return hello[j + 2:j + 2 + ln].decode("ascii")
            i += n
    except (IndexError, struct.error, UnicodeDecodeError):
        pass
    return None


def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d:
                break
            b.sendall(d)
    except OSError:
        pass
    for s in (a, b):
        try:
            s.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


def proxy_addr():
    p = os.environ.get("HTTPS_PROXY") or os.environ.get("https_proxy")
    if not p:
        sys.exit("real-https-relay: HTTPS_PROXY is not set")
    p = p.split("://", 1)[-1].rstrip("/")
    h, _, port = p.rpartition(":")
    return h, int(port)


def handle(c, proxy):
    try:
        hdr = read_exact(c, 5)
        body = read_exact(c, struct.unpack(">H", hdr[3:5])[0])
        name = sni(body) if hdr[0] == 22 else None
        if not name:
            print("real-https-relay: a connection without a server name, dropped", flush=True)
            c.close()
            return
        u = socket.create_connection(proxy, timeout=20)
        u.sendall(f"CONNECT {name}:443 HTTP/1.1\r\nHost: {name}:443\r\n\r\n".encode())
        resp = b""
        while b"\r\n\r\n" not in resp:
            d = u.recv(1)
            if not d:
                raise EOFError
            resp += d
        status = resp.split(b" ", 2)[1:2]
        print(f"real-https-relay: {name}: proxy says {resp.splitlines()[0].decode(errors='replace')}", flush=True)
        if status != [b"200"]:
            c.close()
            u.close()
            return
        u.settimeout(None)
        u.sendall(hdr + body)
        threading.Thread(target=pipe, args=(u, c), daemon=True).start()
        pipe(c, u)
    except (OSError, EOFError) as e:
        print(f"real-https-relay: {e}", flush=True)
        c.close()


def main():
    proxy = proxy_addr()
    threading.Thread(target=dns, daemon=True).start()
    ls = socket.socket()
    ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", 443))
    ls.listen(16)
    print(f"real-https-relay: DNS and TLS relay up, tunnelling through {proxy[0]}:{proxy[1]}", flush=True)
    while True:
        c, _ = ls.accept()
        threading.Thread(target=handle, args=(c, proxy), daemon=True).start()


if __name__ == "__main__":
    main()

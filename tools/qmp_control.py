#!/usr/bin/env python3
"""Minimal QMP/HMP control client for the local Aether debug VM."""
import argparse
import json
import socket
import sys
import time


def connect(host, port):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try:
            sock = socket.create_connection((host, port), timeout=5)
            break
        except OSError:
            time.sleep(0.1)
    else:
        raise SystemExit(f"cannot connect to QMP at {host}:{port}")
    f = sock.makefile("rwb", buffering=0)
    json.loads(f.readline())
    f.write(json.dumps({"execute": "qmp_capabilities"}).encode() + b"\n")
    while True:
        reply = json.loads(f.readline())
        if "return" in reply or "error" in reply:
            break
    return sock, f


def cmd(f, name, arguments=None):
    f.write(json.dumps({"execute": name, "arguments": arguments or {}}).encode() + b"\n")
    while True:
        reply = json.loads(f.readline())
        if "return" in reply:
            return reply["return"]
        if "error" in reply:
            raise SystemExit(f"{name}: {reply['error']}")
        # Ignore asynchronous events.


def hmp(f, line):
    return cmd(f, "human-monitor-command", {"command-line": line})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="192.168.1.85")
    parser.add_argument("--port", type=int, default=4444)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("move")
    p.add_argument("dx", type=int)
    p.add_argument("dy", type=int)
    p = sub.add_parser("click")
    p.add_argument("button", type=int, nargs="?", default=1)
    p = sub.add_parser("key")
    p.add_argument("keys")
    p = sub.add_parser("shot")
    p.add_argument("path")
    sub.add_parser("status")
    args = parser.parse_args()

    sock, f = connect(args.host, args.port)
    try:
        if args.command == "move":
            print(hmp(f, f"mouse_move {args.dx} {args.dy}"), end="")
            print(hmp(f, "info mice"), end="")
        elif args.command == "click":
            print(hmp(f, f"mouse_button {args.button}"), end="")
            time.sleep(0.08)
            print(hmp(f, "mouse_button 0"), end="")
        elif args.command == "key":
            print(hmp(f, f"sendkey {args.keys} 20"), end="")
        elif args.command == "shot":
            cmd(f, "screendump", {"filename": args.path})
            print(args.path)
        elif args.command == "status":
            print(json.dumps(cmd(f, "query-status"), indent=2))
    finally:
        sock.close()


if __name__ == "__main__":
    main()

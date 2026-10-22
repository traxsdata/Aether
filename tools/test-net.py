#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Boot the network self-test on each supported QEMU NIC model.

The image is built by `make net-test`. The kernel takes a DHCP lease from
QEMU's user-mode network and fetches a page from the host alias 10.0.2.2 over
TCP; then the staged Linux app (signed `network=client`) fetches the same page
through the socket API once the test build answers its consent request. This script serves that page on the host loopback
port the kernel was built for, then boots once per NIC model.
"""

from __future__ import annotations

import http.server
import os
import subprocess
import sys
import threading
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PORT = int(os.environ.get("AETHER_NET_TEST_PORT", "18080"))
MODELS = os.environ.get("AETHER_NIC_MODELS", "e1000,e1000e").split(",")
PAYLOAD = b"AETHER_NET_PAYLOAD served by tools/test-net.py\n"


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (http.server naming)
        if self.path != "/aether-net-test":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        self.wfile.write(PAYLOAD)

    def log_message(self, format: str, *args: object) -> None:
        sys.stderr.write("net-test http: " + (format % args) + "\n")


def main() -> int:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    failures = []
    try:
        for model in MODELS:
            environment = dict(os.environ)
            environment["AETHER_NIC_MODEL"] = model
            environment["AETHER_EXTRA_MARKERS"] = ",".join(
                [
                    "AETHER_NET_LINK_OK",
                    "AETHER_NET_DHCP_OK",
                    "AETHER_NET_TCP_OK",
                    "AETHER_NET_CONSENT_TEST",
                    "AETHER_LINUX_NET_OK",
                ]
            )
            print(f"net-test: booting with {model}", flush=True)
            result = subprocess.run(
                [sys.executable, str(ROOT / "tools" / "test-boot.py")],
                env=environment,
                capture_output=True,
                text=True,
            )
            log = (ROOT / "build" / "serial.log").read_text(errors="replace")
            for line in log.splitlines():
                if line.startswith(("net:", "AETHER_NET", "AETHER_LINUX_NET")):
                    print(f"  {line}")
            if result.returncode != 0:
                print(result.stdout[-4000:])
                print(result.stderr[-2000:], file=sys.stderr)
                failures.append(model)
    finally:
        server.shutdown()
    if failures:
        print(f"FAIL: network self-test failed on {', '.join(failures)}", file=sys.stderr)
        return 1
    print(f"PASS: DHCP, kernel and Linux-app TCP fetch on {', '.join(MODELS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

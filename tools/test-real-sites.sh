#!/bin/sh
# test-real-sites.sh - the browser inside QEMU on real HTTPS sites
# (tests/real-sites.txt), from a sandbox whose only way out is an HTTP proxy:
# tools/real-https-relay.py answers DNS with the host and tunnels each TLS
# connection, untouched, through $HTTPS_PROXY, so the browser checks the
# sites' real certificates against its built-in roots.
#
#   tools/test-real-sites.sh      (needs root for ports 53/443 and /etc/resolv.conf)
# Screenshots go to build/test-real-sites.
set -e
cd "$(dirname "$0")/.."
tools/build-browser.sh >/dev/null
make >/dev/null
work=build/test-real-sites
mkdir -p "$work"
cp /etc/resolv.conf "$work/resolv.conf.saved"
cleanup() {
    cp "$work/resolv.conf.saved" /etc/resolv.conf
    kill $RELAY 2>/dev/null || true
}
trap cleanup EXIT
python3 tools/real-https-relay.py > "$work/relay.log" 2>&1 & RELAY=$!
echo "nameserver 127.0.0.1" > /etc/resolv.conf
sleep 1
status=0
AETHER_DISK=none AETHER_NIC_OPTS=net=203.0.113.0/24 tools/qemu-drive.py tests/real-sites.txt "$work" || status=1
exit $status

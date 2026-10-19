#!/bin/sh
# Boot a blank disk, create the encrypted volume, lock the desktop, fail one
# passphrase and unlock it again. Uses the same QEMU harness as the other
# desktop tests.
set -eu
cd "$(dirname "$0")/.."
disk=$(mktemp "${TMPDIR:-/tmp}/aether-lock-XXXXXX.img")
trap 'rm -f "$disk"' EXIT INT TERM
dd if=/dev/zero of="$disk" bs=1M count=64 status=none
AETHER_DISK="$disk" tools/qemu-drive.py tests/lock.txt build/test-lock

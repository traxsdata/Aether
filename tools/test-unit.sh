#!/bin/sh
# Headless unit test run: boots build/aether.iso in QEMU on temporary
# storage, types `test` into the terminal (tests/unit.txt) and passes only if
# every kernel test and every sandbox attack check passed. The full report
# is in build/unit/serial.log, and a screenshot in build/unit/unit.png.
#   tools/test-unit.sh
set -eu
OUT=build/unit
AETHER_DISK=none python3 tools/qemu-drive.py tests/unit.txt "$OUT" >/dev/null || true
LOG="$OUT/serial.log"
grep -E '^\[test\]|^\[terminal\] test:|PANIC' "$LOG" || true
if grep -q PANIC "$LOG"; then
    echo "FAIL (kernel panic)"; exit 1
fi
if grep -q '^\[test\] FAIL' "$LOG"; then
    echo "FAIL (kernel unit tests)"; exit 1
fi
if ! grep -Eq '^\[test\] kernel: [1-9][0-9]* passed, 0 failed' "$LOG"; then
    echo "FAIL (the kernel tests didn't run to the end)"; exit 1
fi
if ! grep -Eq '^\[terminal\] test: [1-9][0-9]* passed, 0 failed' "$LOG"; then
    echo "FAIL (sandbox attack checks)"; exit 1
fi
echo "PASS"

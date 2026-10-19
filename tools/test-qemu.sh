#!/bin/sh
# Headless boot test: boots build/aether.iso under QEMU + OVMF, saves the
# serial log and a screenshot, and passes if the desktop came up and the
# terminal started. A disk that already holds someone else's data is
# attached (or $AETHER_DISK): the AHCI self-test must pass, and the disk must
# come back byte-for-byte unchanged, because æther never formats data it
# doesn't recognise. (tests/storage.txt and tests/files.txt cover the
# encrypted volume, driven by tools/qemu-drive.py.)
#   tools/test-qemu.sh [seconds] [extra qemu args...]
set -eu
OUT=build/test
SECS=${1:-20}
[ $# -gt 0 ] && shift
OVMF=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
DISK=${AETHER_DISK:-}
if [ -z "$DISK" ]; then
    DISK=build/foreign.img
    head -c 1048576 /dev/urandom > "$DISK"
    truncate -s 64M "$DISK"
fi
BEFORE=$(sha256sum "$DISK" | cut -d' ' -f1)
mkdir -p "$OUT"
rm -f "$OUT/serial.log" "$OUT/screen.ppm" "$OUT/monitor.sock"

qemu-system-x86_64 -machine q35 -cpu "${QEMU_CPU:-max}" -m 1G -display none -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -cdrom build/aether.iso \
    -serial file:"$OUT/serial.log" \
    -monitor unix:"$OUT/monitor.sock",server,nowait \
    ${DISK:+-drive file="$DISK",format=raw,if=none,id=d0 -device ide-hd,drive=d0,bus=ide.0} "$@" &
QEMU=$!

i=0
while [ $i -lt "$SECS" ]; do
    sleep 1; i=$((i + 1))
    grep -q "started terminal" "$OUT/serial.log" 2>/dev/null && break
done
sleep 2
printf 'screendump %s\nquit\n' "$OUT/screen.ppm" | \
    python3 -c "import socket,sys,time; s=socket.socket(socket.AF_UNIX); s.connect('$OUT/monitor.sock'); s.sendall(sys.stdin.buffer.read()); time.sleep(1)" || true
wait $QEMU 2>/dev/null || true

cat "$OUT/serial.log"
if [ -z "${AETHER_DISK:-}" ] && [ "$(sha256sum "$DISK" | cut -d' ' -f1)" != "$BEFORE" ]; then
    echo "FAIL (a disk with foreign data was modified)"; exit 1
fi
if [ -n "$DISK" ] && ! grep -q "ahci: selftest ok" "$OUT/serial.log"; then
    echo "FAIL (disk attached but the AHCI self-test didn't pass)"; exit 1
fi
if grep -q "desktop ready" "$OUT/serial.log" && grep -q "started terminal" "$OUT/serial.log" && \
   ! grep -q "PANIC" "$OUT/serial.log" && ! grep -q "selftest FAILED" "$OUT/serial.log"; then
    echo "PASS"
else
    echo "FAIL"; exit 1
fi

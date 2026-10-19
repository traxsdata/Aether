#!/bin/sh
# Headless USB test: boots a live ISO with QEMU's xHCI controller, a USB
# keyboard, a USB mouse and a CDC-ECM USB Ethernet adapter. It checks that
# the controller and all three class drivers came up, that a key and pointer
# movement reach the input queue, and that DHCP completes over USB Ethernet.
#
# QEMU's usb-net offers RNDIS and CDC-ECM as two configurations; the driver
# must pick the ECM one.
set -eu
cd "$(dirname "$0")/.."

. "$HOME/.cargo/env" 2>/dev/null || true
make -j"$(nproc)" >/dev/null

OUT=build/test-usb
OVMF=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
[ -f "$OVMF" ] || OVMF=/usr/share/OVMF/OVMF_CODE.fd
rm -rf "$OUT"
mkdir -p "$OUT"

qemu-system-x86_64 -machine q35 -cpu "${QEMU_CPU:-max}" -m 1G -display none -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -cdrom build/aether.iso \
    -serial file:"$OUT/serial.log" \
    -monitor unix:"$OUT/monitor.sock",server,nowait \
    -nic none \
    -device qemu-xhci,id=xhci \
    -device usb-kbd,bus=xhci.0 \
    -device usb-mouse,bus=xhci.0 \
    -netdev user,id=usbnet0 \
    -device usb-net,bus=xhci.0,netdev=usbnet0 &
QEMU=$!

i=0
while [ $i -lt 40 ]; do
    sleep 1
    i=$((i + 1))
    grep -q "started terminal" "$OUT/serial.log" 2>/dev/null && \
        grep -q "USB Ethernet ready" "$OUT/serial.log" 2>/dev/null && break
done
sleep 2

python3 - "$OUT/monitor.sock" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
for cmd in ("sendkey a", "mouse_move 8 6", "sendkey b"):
    s.sendall((cmd + "\n").encode())
    time.sleep(0.4)
time.sleep(1)
s.sendall(b"quit\n")
time.sleep(0.5)
PY
wait "$QEMU" 2>/dev/null || true

cat "$OUT/serial.log"
fail=0
for want in \
    "[usb] xHCI" \
    "HID keyboard ready" \
    "HID mouse ready" \
    "HID keyboard input active" \
    "HID mouse input active" \
    "USB Ethernet ready" \
    "[net] address 10.0.2.15"
do
    grep -Fq "$want" "$OUT/serial.log" || { echo "FAIL: missing $want"; fail=1; }
done
grep -q "PANIC" "$OUT/serial.log" && { echo "FAIL: panic"; fail=1; }
[ "$fail" -eq 0 ] && echo PASS || exit 1

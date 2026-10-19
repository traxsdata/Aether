#!/bin/sh
# Headless test for the USB mass storage class driver. QEMU attaches a small
# blank disk behind xHCI; the driver must enumerate the BOT interface, answer
# READ CAPACITY and register a blkdev. With no other disk, the storage layer
# then offers to format the USB disk, which is the expected blank-disk flow.
set -eu
cd "$(dirname "$0")/.."

. "$HOME/.cargo/env" 2>/dev/null || true
make -j"$(nproc)" >/dev/null

OUT=build/test-usb-storage
OVMF=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
[ -f "$OVMF" ] || OVMF=/usr/share/OVMF/OVMF_CODE.fd
rm -rf "$OUT"
mkdir -p "$OUT"
dd if=/dev/zero of="$OUT/disk.img" bs=1M count=16 status=none

qemu-system-x86_64 -machine q35 -cpu "${QEMU_CPU:-max}" -m 1G -display none -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -cdrom build/aether.iso \
    -serial file:"$OUT/serial.log" \
    -nic none \
    -device qemu-xhci,id=xhci \
    -drive if=none,file="$OUT/disk.img",format=raw,id=usbdisk \
    -device usb-storage,drive=usbdisk,bus=xhci.0 &
QEMU=$!

i=0
while [ $i -lt 40 ]; do
    sleep 1
    i=$((i + 1))
    grep -q "mass storage ready" "$OUT/serial.log" 2>/dev/null && \
        grep -q "blank disk" "$OUT/serial.log" 2>/dev/null && break
done
sleep 2
kill "$QEMU" 2>/dev/null || true
wait "$QEMU" 2>/dev/null || true

cat "$OUT/serial.log"
fail=0
for want in \
    "[usb] xHCI" \
    "mass storage ready" \
    "[blk] usb0: 32768 sectors" \
    "[storage] usb0: blank disk"
do
    grep -Fq "$want" "$OUT/serial.log" || { echo "FAIL: missing $want"; fail=1; }
done
grep -q "PANIC" "$OUT/serial.log" && { echo "FAIL: panic"; fail=1; }
[ "$fail" -eq 0 ] && echo PASS || exit 1

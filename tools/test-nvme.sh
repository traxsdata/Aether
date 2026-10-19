#!/bin/sh
# Headless NVMe driver/layout test. Builds an installed A/B disk, boots it
# under QEMU with an NVMe namespace, and checks the Rust driver, GPT parse and
# installed-system detection. This does not format the data partition; it
# stops at the first-boot lock screen, which is the point where storage and
# the A/B slot have already been validated.
set -eu
cd "$(dirname "$0")/.."

: "${AETHER_SIGNING_KEY:?set AETHER_SIGNING_KEY to the release signing key}"
. "$HOME/.cargo/env" 2>/dev/null || true
make -j"$(nproc)" >/dev/null
make -j"$(nproc)" boot-image system-image AETHER_SIGNING_KEY="$AETHER_SIGNING_KEY" >/dev/null
OUT=build/test-nvme
mkdir -p "$OUT"
rm -f "$OUT/serial.log"
tools/mkdisk.py --output build/nvme-test.img \
    --boot build/boot.img --system build/system.img --size-mib 2048 >/dev/null

OVMF=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
qemu-system-x86_64 -machine q35 -cpu "${QEMU_CPU:-max}" -m 1G -display none -no-reboot \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -drive file=build/nvme-test.img,format=raw,if=none,id=dn \
    -device nvme,drive=dn,serial=TESTNVME \
    -serial file:"$OUT/serial.log" &
QEMU=$!
i=0
while [ $i -lt 30 ]; do
    sleep 1; i=$((i + 1))
    grep -q "storage\\] nvme0p5" "$OUT/serial.log" 2>/dev/null && break
done
kill "$QEMU" 2>/dev/null || true
wait "$QEMU" 2>/dev/null || true
cat "$OUT/serial.log"

grep -q "NVMe namespace ready" "$OUT/serial.log"
grep -q "blk\\] nvme0:" "$OUT/serial.log"
grep -q "installed system on nvme0" "$OUT/serial.log"
! grep -q "KERNEL PANIC" "$OUT/serial.log"
echo "PASS"

#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build and sign a small AppImage type 2 fixture for the in-OS import path.
#
# This is a test artifact staged through the harness, not a user download. It
# has a distinct identity from the embedded runtime rootfs so the import path
# cannot rely on the fixture the kernel already knows. The container is an ELF
# runtime stub with a zstd SquashFS image appended; the signed linux-appimage
# record pins its digest, identity, version, and publisher.
#
# Pass memory=jit as the fifth argument to declare writable+executable
# memory in the signed record.
#
# Usage: build-appimage-fixture.sh <out.AppImage> <out.record> <seed> <app-elf> [memory=jit]
set -eu
if [ "$#" -ne 4 ] && [ "$#" -ne 5 ]; then
    echo "usage: build-appimage-fixture.sh <out.AppImage> <out.record> <seed> <app-elf> [memory=jit]" >&2
    exit 2
fi
OUT=$1
RECORD=$2
SEED=$3
APP=$4
MEMORY=${5:-}

if [ ! -f "$APP" ]; then
    echo "error: app ELF not found: $APP" >&2
    exit 1
fi
if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "error: mksquashfs not found (install squashfs-tools)" >&2
    exit 1
fi

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/usr/bin" "$ROOT/usr/share/doc/app" "$ROOT/etc"
cp "$APP" "$ROOT/usr/bin/app"
printf 'AETHER_LINUX_APPIMAGE_ROOT_OK app=user\n' > "$ROOT/etc/motd"
printf 'Aether user AppImage fixture 1.0.0\n' > "$ROOT/usr/share/doc/app/version"

mksquashfs "$ROOT" "$ROOT/app.sqfs" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress >/dev/null

# AppImage type 2: an x86-64 ELF runtime with "AI\x02" in the ELF identity
# padding and the SquashFS image appended after the runtime.
cp "$APP" "$OUT"
python3 - "$OUT" "$ROOT/app.sqfs" <<'PY'
import sys

image = sys.argv[1]
payload = sys.argv[2]
with open(image, "r+b") as handle:
    header = bytearray(handle.read(64))
    if header[:4] != b"\x7fELF" or header[4] != 2 or header[5] != 1:
        raise SystemExit("fixture runtime is not ELF64 little-endian")
    header[8:11] = b"AI\x02"
    handle.seek(0)
    handle.write(header)
    handle.seek(0, 2)
    with open(payload, "rb") as squash:
        while chunk := squash.read(65536):
            handle.write(chunk)
PY

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" org.example.aether-appimage-demo 1.0.0 \
    "Aether Test Publisher" "$RECORD" linux-appimage $MEMORY >/dev/null

echo "built signed AppImage fixture: $OUT"

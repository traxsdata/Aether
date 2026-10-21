#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the Gate 3 headless runtime rootfs.
#
# Gate 3 imports the pinned GridCraft AppImage and runs `gridcraft-cli` with a
# read-only root and a private /tmp overlay. The rootfs therefore carries only
# the host glibc, its loader, and the CSV input fixture; the application itself
# comes from the signed AppImage. Every packaged library is hashed into
# etc/aether-runtime.manifest and the manifest digest is written to the pin
# file, which the kernel compares at boot.
#
# The profile is derived from the host's installed x86-64 glibc. This is a
# host-derived pin, not a downloaded release: a different host produces a
# different pin and the boot test says so; nothing is silently upgraded.
#
# Usage: build-gate3-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed>
set -eu
if [ "$#" -ne 5 ]; then
    echo "usage: build-gate3-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed>" >&2
    exit 2
fi
OUT=$1
RECORD=$2
PIN=$3
MANIFEST=$4
SEED=$5

if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "error: mksquashfs not found (install squashfs-tools)" >&2
    exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
    echo "error: sha256sum not found" >&2
    exit 1
fi
if [ ! -f "$SEED" ]; then
    echo "error: signing seed not found: $SEED" >&2
    exit 1
fi

LIBDIR=/lib/x86_64-linux-gnu
LDSO=/lib64/ld-linux-x86-64.so.2
if [ ! -e "$LIBDIR/libc.so.6" ] || [ ! -e "$LDSO" ]; then
    echo "error: host x86-64 glibc not found; cannot derive a Gate 3 runtime pin" >&2
    exit 1
fi

ROOT=build/gate3-runtime-root
rm -rf "$ROOT"
mkdir -p "$ROOT/etc" "$ROOT/lib64" "$ROOT/lib/x86_64-linux-gnu" \
    "$ROOT/usr/share/gridcraft"

printf 'AETHER_GATE3_ROOT_OK source=/etc/motd\n' > "$ROOT/etc/motd"
cp -L "$LDSO" "$ROOT/lib64/ld-linux-x86-64.so.2"

# `readelf -d` on gridcraft-cli lists exactly these three NEEDED libraries.
# Do not add speculative libraries: the boot test hashes what is packaged.
for library in libc.so.6 libm.so.6 libgcc_s.so.1; do
    if [ ! -e "$LIBDIR/$library" ]; then
        echo "error: required host library not found: $LIBDIR/$library" >&2
        exit 1
    fi
    cp -L "$LIBDIR/$library" "$ROOT/lib/x86_64-linux-gnu/$library"
done

# Input for the Gate 3 one-process formula and CSV read/write check.
printf 'item,value\nalpha,1\nbeta,2\n' > "$ROOT/usr/share/gridcraft/gate3-in.csv"

GLIBC=$(getconf GNU_LIBC_VERSION | tr ' ' '-')
LDSO_SHA=$(sha256sum "$ROOT/lib64/ld-linux-x86-64.so.2" | cut -d' ' -f1)
LIBC_SHA=$(sha256sum "$ROOT/lib/x86_64-linux-gnu/libc.so.6" | cut -d' ' -f1)

{
    echo "profile=gate3-host-glibc"
    echo "glibc=$GLIBC"
    echo "source=host-derived-gate3"
    echo "ld_so=lib64/ld-linux-x86-64.so.2"
    echo "ld_so_sha256=$LDSO_SHA"
    echo "libc=lib/x86_64-linux-gnu/libc.so.6"
    echo "libc_sha256=$LIBC_SHA"
    echo "mesa=absent"
    echo "wayland=absent"
} > "$ROOT/etc/aether-runtime.manifest"

PACKAGED_SHA=$(sha256sum "$ROOT/etc/aether-runtime.manifest" | cut -d' ' -f1)
printf '%s\n' "$PACKAGED_SHA" > "$PIN"
cp "$ROOT/etc/aether-runtime.manifest" "$MANIFEST"

# -repro-time makes the image itself deterministic for a fixed host rootfs.
mksquashfs "$ROOT" "$OUT" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress -repro-time 0 >/dev/null

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" org.aether.linux-runtime "$GLIBC" \
    "Aether Test Publisher" "$RECORD" linux-rootfs >/dev/null

ROOTFS_SHA=$(sha256sum "$OUT" | cut -d' ' -f1)
echo "built Gate 3 runtime rootfs: $OUT"
echo "rootfs sha256: $ROOTFS_SHA"
echo "runtime manifest: $MANIFEST"
echo "runtime pin: $PACKAGED_SHA"

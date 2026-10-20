#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the pinned Linux runtime rootfs used by the boot test.
#
# The profile is derived from the host's installed x86-64 glibc. This is a
# host-derived pin, not a downloaded release: every packaged file is hashed
# into etc/aether-runtime.manifest and the manifest digest is written to the
# pin file, which the kernel compares at boot. A different host produces a
# different pin and the boot test says so; nothing is silently upgraded.
#
# Usage: build-pinned-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed>
set -eu
if [ "$#" -ne 5 ]; then
    echo "usage: build-pinned-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed>" >&2
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

LIBDIR=/lib/x86_64-linux-gnu
LDSO=/lib64/ld-linux-x86-64.so.2
if [ ! -e "$LIBDIR/libc.so.6" ] || [ ! -e "$LDSO" ]; then
    echo "error: host x86-64 glibc not found; cannot derive a runtime pin" >&2
    exit 1
fi

ROOT=build/linux-runtime-pinned-root
rm -rf "$ROOT"
mkdir -p "$ROOT/etc" "$ROOT/lib" "$ROOT/lib64" "$ROOT/lib/x86_64-linux-gnu" "$ROOT/usr/bin"

# Test payloads that the boot fixtures resolve through this root.
printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\n' > "$ROOT/etc/motd"
for pair in \
    "build/linux-interp.elf:lib/ld-aether.so" \
    "build/linux-dynamic.elf:usr/bin/dynamic" \
    "build/linux-exec.elf:usr/bin/exec" \
    "build/linux-dynamic-exec.elf:usr/bin/dynamic-exec"
do
    source=${pair%%:*}
    target=${pair#*:}
    if [ -f "$source" ]; then
        cp "$source" "$ROOT/$target"
    fi
done

# The pinned production libraries: host glibc and its loader, recorded exactly.
cp -L "$LDSO" "$ROOT/lib64/ld-linux-x86-64.so.2"
for library in libc.so.6 libm.so.6 libgcc_s.so.1; do
    if [ -e "$LIBDIR/$library" ]; then
        cp -L "$LIBDIR/$library" "$ROOT/lib/x86_64-linux-gnu/$library"
    fi
done

GLIBC=$(getconf GNU_LIBC_VERSION | tr ' ' '-')
LDSO_SHA=$(sha256sum "$ROOT/lib64/ld-linux-x86-64.so.2" | cut -d' ' -f1)
LIBC_SHA=$(sha256sum "$ROOT/lib/x86_64-linux-gnu/libc.so.6" | cut -d' ' -f1)

{
    echo "profile=pinned-host-glibc"
    echo "glibc=$GLIBC"
    echo "source=host-derived-pin"
    echo "ld_so=lib64/ld-linux-x86-64.so.2"
    echo "ld_so_sha256=$LDSO_SHA"
    echo "libc=lib/x86_64-linux-gnu/libc.so.6"
    echo "libc_sha256=$LIBC_SHA"
    echo "mesa=absent"
    echo "wayland=absent"
} > "$ROOT/etc/aether-runtime.manifest"

PACKAGED_SHA=$(sha256sum "$ROOT/etc/aether-runtime.manifest" | cut -d' ' -f1)
printf '%s\n' "$PACKAGED_SHA" > "$PIN"

mksquashfs "$ROOT" "$OUT" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress >/dev/null

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" org.aether.linux-runtime "$GLIBC" \
    "Aether Test Publisher" "$RECORD" linux-rootfs >/dev/null

cp "$ROOT/etc/aether-runtime.manifest" "$MANIFEST.packaged"
ROOTFS_SHA=$(sha256sum "$OUT" | cut -d' ' -f1)
{
    echo "profile=pinned-host-glibc"
    echo "glibc=$GLIBC"
    echo "rootfs=$OUT"
    echo "rootfs_sha256=$ROOTFS_SHA"
    echo "manifest_sha256=$PACKAGED_SHA"
    echo "ld_so_sha256=$LDSO_SHA"
    echo "libc_sha256=$LIBC_SHA"
    echo "mesa=absent"
    echo "wayland=absent"
} > "$MANIFEST"

echo "pinned runtime rootfs: $OUT"
echo "runtime pin: $PACKAGED_SHA"

#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build a host-glibc runtime rootfs for the opt-in Linux loader smoke test.
#
# This is not the pinned production runtime. It packages the host's current
# glibc so the dynamic loader, auxv, file syscalls, and mmap paths can be
# exercised against a real libc. The pinned rootfs remains a packaging task.
#
# Usage: build-glibc-rootfs.sh <out.sqfs> <out.import> <signing-seed> [pin-file]
set -eu
if [ "$#" -ne 3 ] && [ "$#" -ne 4 ]; then
    echo "usage: build-glibc-rootfs.sh <out.sqfs> <out.import> <seed> [pin-file]" >&2
    exit 2
fi
OUT=$1
RECORD=$2
SEED=$3
PIN=${4:-}
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT

if ! command -v cc >/dev/null 2>&1; then
    echo "error: cc is required for the glibc smoke test" >&2
    exit 1
fi
if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "error: mksquashfs is required (install squashfs-tools)" >&2
    exit 1
fi

LIBDIR=/lib/x86_64-linux-gnu
if [ ! -e "$LIBDIR/libc.so.6" ] || [ ! -e /lib64/ld-linux-x86-64.so.2 ]; then
    echo "error: host x86-64 glibc not found under $LIBDIR" >&2
    exit 1
fi

mkdir -p "$ROOT/etc" "$ROOT/lib64" "$ROOT/lib/x86_64-linux-gnu" "$ROOT/usr/bin"
printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\n' > "$ROOT/etc/motd"
cp -L /lib64/ld-linux-x86-64.so.2 "$ROOT/lib64/ld-linux-x86-64.so.2"
for library in libc.so.6 libm.so.6 libgcc_s.so.1; do
    if [ -e "$LIBDIR/$library" ]; then
        cp -L "$LIBDIR/$library" "$ROOT/lib/x86_64-linux-gnu/$library"
    fi
done
cc -O2 -o "$ROOT/usr/bin/hello" tests/fixtures/linux/glibc-hello.c
# The static Linux boot fixture also runs in this image and execs this path.
if [ -f build/linux-exec.elf ]; then
    cp build/linux-exec.elf "$ROOT/usr/bin/exec"
fi
# The static boot fixture chains through a dynamic exec proof. Keep the
# freestanding test interpreter and target beside the host-glibc files; this
# does not replace the host PT_INTERP used by /usr/bin/hello.
if [ -f build/linux-dynamic-exec.elf ] && [ -f build/linux-interp.elf ]; then
    cp build/linux-dynamic-exec.elf "$ROOT/usr/bin/dynamic-exec"
    cp build/linux-interp.elf "$ROOT/lib/ld-aether.so"
fi

GLIBC=$(getconf GNU_LIBC_VERSION | tr ' ' '-')
LDSO_SHA=$(sha256sum "$ROOT/lib64/ld-linux-x86-64.so.2" | cut -d' ' -f1)
LIBC_SHA=$(sha256sum "$ROOT/lib/x86_64-linux-gnu/libc.so.6" | cut -d' ' -f1)
{
    echo "profile=host-glibc-smoke"
    echo "glibc=$GLIBC"
    echo "source=host-derived-smoke"
    echo "ld_so=lib64/ld-linux-x86-64.so.2"
    echo "ld_so_sha256=$LDSO_SHA"
    echo "libc=lib/x86_64-linux-gnu/libc.so.6"
    echo "libc_sha256=$LIBC_SHA"
    echo "mesa=absent"
    echo "wayland=absent"
} > "$ROOT/etc/aether-runtime.manifest"
if [ -n "$PIN" ]; then
    sha256sum "$ROOT/etc/aether-runtime.manifest" | cut -d' ' -f1 > "$PIN"
fi

mksquashfs "$ROOT" "$OUT" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress >/dev/null
cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" org.aether.linux-runtime host-glibc \
    "Aether Test Publisher" "$RECORD" linux-rootfs >/dev/null
echo "built host-glibc rootfs: $OUT"
ldd "$ROOT/usr/bin/hello" 2>/dev/null || true

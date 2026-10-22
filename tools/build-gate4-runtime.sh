#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build the Gate 4 GUI runtime rootfs.
#
# Gate 4 hosts the real GridCraft GUI from its signed AppImage. The application
# is not baked into this rootfs; the rootfs carries the dynamic loader, glibc,
# Wayland client libraries, libxkbcommon, the Vulkan loader, Mesa lavapipe and
# its complete ldd closure, plus the Mesa ICD configuration.
#
# Every packaged file is hashed into etc/aether-runtime.manifest and the
# manifest digest is written to the pin file. The kernel verifies the manifest
# digest and each file_sha256 entry before importing the tree. The signed
# linux-rootfs record binds the packaged SquashFS image.
#
# The profile is derived from the host's installed x86-64 libraries. This is a
# host-derived pin, not a downloaded release: a different host produces a
# different pin and the boot test says so.
#
# libwayland-client.so.0 is initially the host library at its normal SONAME
# path. The Gate 4 Wayland bridge replaces it later without changing the path.
#
# Usage: build-gate4-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed> [extra-root]
set -eu
if [ "$#" -ne 5 ] && [ "$#" -ne 6 ]; then
    echo "usage: build-gate4-runtime.sh <out.sqfs> <out.import> <out.pin> <out.manifest> <seed> [extra-root]" >&2
    exit 2
fi
OUT=$1
RECORD=$2
PIN=$3
MANIFEST=$4
SEED=$5
EXTRA=${6:-}

for tool in mksquashfs sha256sum readelf getconf; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "error: $tool not found" >&2
        exit 1
    fi
done
if [ ! -f "$SEED" ]; then
    echo "error: signing seed not found: $SEED" >&2
    exit 1
fi

LIBDIR=/lib/x86_64-linux-gnu
ALT_LIBDIR=/usr/lib/x86_64-linux-gnu
LDSO=/lib64/ld-linux-x86-64.so.2
if [ ! -e "$LIBDIR/libc.so.6" ] || [ ! -e "$LDSO" ]; then
    echo "error: host x86-64 glibc not found; cannot derive a Gate 4 runtime pin" >&2
    exit 1
fi

ROOT=build/gate4-runtime-root
rm -rf "$ROOT"
mkdir -p "$ROOT/etc" "$ROOT/lib64" "$ROOT/lib/x86_64-linux-gnu" \
    "$ROOT/usr/share/vulkan/icd.d" "$ROOT/usr/share/doc/aether-runtime"

find_library() {
    name=$1
    for directory in "$LIBDIR" "$ALT_LIBDIR"; do
        if [ -e "$directory/$name" ]; then
            printf '%s\n' "$directory/$name"
            return 0
        fi
    done
    return 1
}

# Seed set: the loader, glibc, Wayland client ABI, keyboard, Vulkan and the
# lavapipe software driver. The driver's ldd output supplies LLVM and the rest
# of the closure.
SEEDS="libc.so.6 libm.so.6 libgcc_s.so.1 libwayland-client.so.0 \
libwayland-cursor.so.0 libxkbcommon.so.0 libvulkan.so.1 libvulkan_lvp.so \
libasound.so.2"
for library in $SEEDS; do
    if ! find_library "$library" >/dev/null; then
        echo "error: required Gate 4 host library not found: $library" >&2
        exit 1
    fi
done

# Resolve the complete ldd closure. The list grows while it is walked, so each
# dependency of a newly discovered library is visited too.
LIST=build/gate4-runtime-closure.txt
: > "$LIST"
add_path() {
    resolved=$(readlink -f "$1")
    if [ -z "$resolved" ] || [ ! -f "$resolved" ]; then
        return 1
    fi
    if ! grep -qxF "$resolved" "$LIST"; then
        printf '%s\n' "$resolved" >> "$LIST"
    fi
}
for library in $SEEDS; do
    add_path "$(find_library "$library")"
done
index=1
while [ "$index" -le "$(wc -l < "$LIST" | tr -d ' ')" ]; do
    path=$(sed -n "${index}p" "$LIST")
    index=$((index + 1))
    if ! ldd_output=$(ldd "$path" 2>&1); then
        echo "error: ldd failed for $path: $ldd_output" >&2
        exit 1
    fi
    LDD_OUTPUT=build/gate4-runtime-ldd.txt
    printf '%s\n' "$ldd_output" > "$LDD_OUTPUT"
    while IFS= read -r line; do
        case "$line" in
            *"=> not found"*)
                echo "error: unresolved dependency: $line" >&2
                exit 1
                ;;
            *"=> /"*)
                dependency=${line#*=> }
                dependency=${dependency%% (*}
                add_path "$dependency"
                ;;
            /*)
                dependency=${line%% (*}
                add_path "$dependency"
                ;;
        esac
    done < "$LDD_OUTPUT"
done

# Copy glibc/loader first, then each closure member under its SONAME.
cp -L "$LDSO" "$ROOT/lib64/ld-linux-x86-64.so.2"
copied=0
while IFS= read -r path; do
    base=$(basename "$path")
    case "$base" in
        ld-linux-x86-64.so.2|ld-*.so.*)
            continue
            ;;
    esac
    soname=$(readelf -d "$path" 2>/dev/null | sed -n 's/.*SONAME.*\[\(.*\)\]/\1/p' | head -n 1)
    if [ -z "$soname" ]; then
        soname=$base
    fi
    destination=$ROOT/lib/x86_64-linux-gnu/$soname
    if [ -e "$destination" ]; then
        existing=$(sha256sum "$destination" | cut -d' ' -f1)
        candidate=$(sha256sum "$path" | cut -d' ' -f1)
        if [ "$existing" != "$candidate" ]; then
            echo "error: two host libraries map to the same SONAME: $soname" >&2
            exit 1
        fi
        continue
    fi
    cp -L "$path" "$destination"
    copied=$((copied + 1))
done < "$LIST"
if [ "$copied" -eq 0 ]; then
    echo "error: runtime closure is empty" >&2
    exit 1
fi

# Replace the host Wayland client with the in-app Aether bridge when the build
# supplies one. The SONAME and path stay identical so winit/SCTK and lavapipe
# resolve the bridge without application changes.
if [ -n "${AETHER_WAYLAND_BRIDGE_DIR:-}" ]; then
    for bridge in libwayland-client.so.0 libwayland-cursor.so.0; do
        if [ ! -f "$AETHER_WAYLAND_BRIDGE_DIR/$bridge" ]; then
            echo "error: Wayland bridge missing: $AETHER_WAYLAND_BRIDGE_DIR/$bridge" >&2
            exit 1
        fi
        cp "$AETHER_WAYLAND_BRIDGE_DIR/$bridge" "$ROOT/lib/x86_64-linux-gnu/$bridge"
    done
fi

# The Vulkan loader resolves this ICD by its normal system path.
cat > "$ROOT/usr/share/vulkan/icd.d/lvp_icd.json" <<'ICD'
{
    "ICD": {
        "api_version": "1.4.335",
        "library_path": "libvulkan_lvp.so"
    },
    "file_format_version": "1.0.1"
}
ICD

# Runtime-owned test input and provenance/notice. The GridCraft GUI binary and
# data come from the signed AppImage, not this tree.
printf 'AETHER_GATE4_RUNTIME_ROOT_OK source=/etc/motd\n' > "$ROOT/etc/motd"

# The boot gate also exercises the earlier Linux ABI fixtures. They are small
# test payloads, not application content, and are merged before hashing.
if [ -n "$EXTRA" ]; then
    if [ ! -d "$EXTRA" ]; then
        echo "error: extra rootfs directory not found: $EXTRA" >&2
        exit 1
    fi
    cp -a "$EXTRA"/. "$ROOT"/
fi

GLIBC=$(getconf GNU_LIBC_VERSION | tr ' ' '-')
MESA_VERSION=$(dpkg-query -W -f='${Version}' mesa-vulkan-drivers 2>/dev/null || true)
WAYLAND_VERSION=$(dpkg-query -W -f='${Version}' libwayland-client0 2>/dev/null || true)
[ -n "$MESA_VERSION" ] || MESA_VERSION=host
[ -n "$WAYLAND_VERSION" ] || WAYLAND_VERSION=host
VULKAN_VERSION=$(dpkg-query -W -f='${Version}' libvulkan1 2>/dev/null || true)
[ -n "$VULKAN_VERSION" ] || VULKAN_VERSION=host

cat > "$ROOT/usr/share/doc/aether-runtime/provenance" <<NOTICE
Aether Gate 4 runtime rootfs
============================

Profile: gate4-host-lavapipe
Source: host package installation (no network download, no vendor blob)

Bundled binary components and their upstream licenses:
  glibc / ld-linux        LGPL-2.1-or-later (GNU C Library)
  Mesa lavapipe           MIT (Mesa 3D)
  LLVM                    Apache-2.0 WITH LLVM-exception
  libwayland-client       MIT (Wayland)
  libxkbcommon            MIT
  libvulkan (loader)      Apache-2.0 (Vulkan-Loader)
  transitive dependencies as recorded by the host package database

This rootfs is assembled from the build host's installed x86-64 libraries.
It is a host-derived pin, not a reproducible cross-host artifact, until a
signed package channel exists. Exact per-file SHA-256 digests are recorded in
/etc/aether-runtime.manifest and checked by the kernel at boot.

The GridCraft application is not included in this rootfs. It is supplied as a
separately signed AppImage and staged by the UEFI loader.
NOTICE

# Hash every regular file except the manifest itself, which cannot contain its
# own digest. The kernel verifies each file_sha256 entry.
FILES=build/gate4-runtime-files.txt
: > "$FILES"
(
    cd "$ROOT"
    find . -type f ! -path './etc/aether-runtime.manifest' | sed 's|^\./||' | sort
) > "$FILES"

LDSO_SHA=$(sha256sum "$ROOT/lib64/ld-linux-x86-64.so.2" | cut -d' ' -f1)
LIBC_SHA=$(sha256sum "$ROOT/lib/x86_64-linux-gnu/libc.so.6" | cut -d' ' -f1)
{
    echo "profile=gate4-host-lavapipe"
    echo "glibc=$GLIBC"
    echo "source=host-derived-gate4"
    echo "mesa=lavapipe-$MESA_VERSION"
    echo "wayland=$WAYLAND_VERSION"
    echo "vulkan=$VULKAN_VERSION"
    echo "ld_so=lib64/ld-linux-x86-64.so.2"
    echo "ld_so_sha256=$LDSO_SHA"
    echo "libc=lib/x86_64-linux-gnu/libc.so.6"
    echo "libc_sha256=$LIBC_SHA"
    while IFS= read -r file; do
        digest=$(sha256sum "$ROOT/$file" | cut -d' ' -f1)
        echo "file_sha256=/$file:$digest"
    done < "$FILES"
} > "$ROOT/etc/aether-runtime.manifest"

PACKAGED_SHA=$(sha256sum "$ROOT/etc/aether-runtime.manifest" | cut -d' ' -f1)
printf '%s\n' "$PACKAGED_SHA" > "$PIN"
cp "$ROOT/etc/aether-runtime.manifest" "$MANIFEST"

# Fixed timestamps make the image deterministic for a fixed host rootfs. The
# contents themselves remain host-derived.
mksquashfs "$ROOT" "$OUT" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress -mkfs-time 0 -all-time 0 >/dev/null

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" org.aether.linux-runtime "$GLIBC" \
    "Aether Test Publisher" "$RECORD" linux-rootfs >/dev/null

ROOTFS_SHA=$(sha256sum "$OUT" | cut -d' ' -f1)
PACKED_FILES=$(wc -l < "$FILES" | tr -d ' ')
PACKED_BYTES=$(du -sb "$ROOT" | cut -f1)
echo "built Gate 4 runtime rootfs: $OUT"
echo "rootfs sha256: $ROOTFS_SHA"
echo "runtime manifest: $MANIFEST"
echo "runtime pin: $PACKAGED_SHA"
echo "profile: gate4-host-lavapipe glibc=$GLIBC mesa=lavapipe-$MESA_VERSION wayland=$WAYLAND_VERSION"
echo "packaged files: $PACKED_FILES root bytes: $PACKED_BYTES"

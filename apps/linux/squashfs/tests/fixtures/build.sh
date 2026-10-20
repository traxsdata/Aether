#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Reproducible SquashFS fixtures for the reader tests.
set -eu
cd "$(dirname "$0")"
ROOT=$(mktemp -d)
BAD=$(mktemp -d)
trap 'rm -rf "$ROOT" "$BAD"' EXIT
mkdir -p "$ROOT/etc" "$ROOT/lib"
printf 'AETHER_LINUX_ROOT_OK source=/etc/motd\n' > "$ROOT/etc/motd"
printf 'hello from squashfs\n' > "$ROOT/hello.txt"
ln "$ROOT/hello.txt" "$ROOT/hard.txt"
ln -s etc/motd "$ROOT/link"
mksquashfs "$ROOT" root.sqfs \
    -comp zstd -noappend -all-root -no-xattrs -no-progress
for compression in gzip lz4 xz lzo lzma; do
    mksquashfs "$ROOT" "root-$compression.sqfs" \
        -comp "$compression" -noappend -all-root -no-xattrs -no-progress
done

printf 'outside\n' > "$BAD/secret"
ln -s ../../secret "$BAD/escape"
mksquashfs "$BAD" unsafe-symlink.sqfs \
    -comp zstd -noappend -all-root -no-xattrs -no-progress

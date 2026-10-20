// SPDX-License-Identifier: Apache-2.0
//! Reproducible SquashFS fixture for the reader tests.
set -eu
cd "$(dirname "$0")"
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/etc" "$ROOT/lib"
printf 'AETHER_SQUASHFS_OK source=/etc/motd\n' > "$ROOT/etc/motd"
printf 'hello from squashfs\n' > "$ROOT/hello.txt"
ln -s etc/motd "$ROOT/link"
mksquashfs "$ROOT" root.sqfs \
    -comp zstd -noappend -all-root -no-xattrs -no-progress

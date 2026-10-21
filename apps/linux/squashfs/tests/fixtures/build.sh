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
# Exercise the importer's explicit xattr allowlist with a retained user.*
# attribute. Privileged namespaces (security.capability, trusted.*) cannot be
# created unprivileged here; the allowlist unit tests cover those names.
python3 - "$ROOT/hello.txt" <<'PY'
import os
import sys
os.setxattr(sys.argv[1], b"user.allowed", b"yes")
PY
ln "$ROOT/hello.txt" "$ROOT/hard.txt"
ln -s etc/motd "$ROOT/link"
mksquashfs "$ROOT" root.sqfs \
    -comp zstd -noappend -all-root -xattrs -no-progress
for compression in gzip lz4 xz lzo lzma; do
    mksquashfs "$ROOT" "root-$compression.sqfs" \
        -comp "$compression" -noappend -all-root -xattrs -no-progress
done

printf 'outside\n' > "$BAD/secret"
ln -s ../../secret "$BAD/escape"
mksquashfs "$BAD" unsafe-symlink.sqfs \
    -comp zstd -noappend -all-root -no-xattrs -no-progress

# A capability-like xattr needs privilege to create on the host. Build an
# uncompressed image with `user.capability`, then rewrite the xattr type from
# user (0) to security (2). The importer must drop it.
DENY=$(mktemp -d)
trap 'rm -rf "$ROOT" "$BAD" "$DENY"' EXIT
printf 'denied\n' > "$DENY/secret.txt"
python3 - "$DENY/secret.txt" <<'PY'
import os
import sys
os.setxattr(sys.argv[1], b"user.capability", b"\x01\x00\x00\x02")
PY
mksquashfs "$DENY" xattr-denied.sqfs \
    -comp gzip -noI -noD -noF -noX -noappend -all-root -xattrs -no-progress
python3 - xattr-denied.sqfs <<'PY'
import sys
path = sys.argv[1]
data = bytearray(open(path, "rb").read())
needle = b"\x00\x00\x0a\x00capability"
offset = data.find(needle)
if offset < 0:
    raise SystemExit("xattr fixture: capability name not found")
data[offset:offset + 2] = b"\x02\x00"
open(path, "wb").write(data)
PY

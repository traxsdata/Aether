#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Package a Linux runtime root directory as a zstd SquashFS image and sign it
# with a linux-rootfs import record.
#
# Required:
#   $1 rootfs directory
#   $2 output SquashFS image
#   $3 signing seed file (64 hex characters)
#   $4 application/runtime identity
#   $5 version
#   $6 publisher name
#   $7 output import record
#
# Requires mksquashfs (squashfs-tools) and the workspace cargo toolchain.
# The image is readable by aether-squashfs; in-OS mounting is still pending.
set -eu
if [ "$#" -ne 7 ]; then
    echo "usage: build-runtime-rootfs.sh <rootfs-dir> <out.sqfs> <seed> <id> <version> <publisher> <out.import>" >&2
    exit 2
fi
ROOT=$1
OUT=$2
SEED=$3
ID=$4
VERSION=$5
PUBLISHER=$6
RECORD=$7

if [ ! -d "$ROOT" ]; then
    echo "error: rootfs directory not found: $ROOT" >&2
    exit 1
fi
if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "error: mksquashfs not found (install squashfs-tools)" >&2
    exit 1
fi

mksquashfs "$ROOT" "$OUT" \
    -comp zstd -noappend -all-root -no-xattrs -no-progress

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$OUT" "$ID" "$VERSION" "$PUBLISHER" "$RECORD" linux-rootfs

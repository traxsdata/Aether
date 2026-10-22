#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Sign a pinned Craft app AppImage for an Aether runtime test.
#
# The expected digest is supplied by the caller so the build fails closed if
# the file is not the reviewed artifact. The AppImage is read in place: this
# script never copies or rewrites it. The signed linux-appimage record pins the
# identity, version, exact digest, and test publisher, and the record is
# verified against the image before the script succeeds.
#
# Usage: build-craft-appimage.sh <appimage> <out.record> <seed> <id> <version> <pinned-sha256> [memory=jit]
set -eu
if [ "$#" -ne 6 ] && [ "$#" -ne 7 ]; then
    echo "usage: build-craft-appimage.sh <appimage> <out.record> <seed> <id> <version> <pinned-sha256> [memory=jit]" >&2
    exit 2
fi
APP=$1
RECORD=$2
SEED=$3
ID=$4
VERSION=$5
PINNED=$6
MEMORY=${7:-}
if [ -n "$MEMORY" ] && [ "$MEMORY" != "memory=jit" ]; then
    echo "error: the only memory policy is memory=jit" >&2
    exit 2
fi

if [ ! -f "$APP" ]; then
    echo "error: AppImage not found: $APP" >&2
    exit 1
fi
if [ ! -f "$SEED" ]; then
    echo "error: signing seed not found: $SEED" >&2
    exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
    echo "error: sha256sum not found" >&2
    exit 1
fi
case $PINNED in
    *[!0-9a-f]* | "")
        echo "error: pinned digest must be 64 lowercase hexadecimal characters" >&2
        exit 1
        ;;
esac
if [ ${#PINNED} -ne 64 ]; then
    echo "error: pinned digest must be 64 lowercase hexadecimal characters" >&2
    exit 1
fi

ACTUAL=$(sha256sum "$APP" | cut -d' ' -f1)
if [ "$ACTUAL" != "$PINNED" ]; then
    echo "error: AppImage digest does not match the pinned $ID $VERSION artifact" >&2
    echo "  expected: $PINNED" >&2
    echo "  actual:   $ACTUAL" >&2
    exit 1
fi

if [ -n "$MEMORY" ]; then
    cargo run -q -p aether-appimage --bin appimage-manifest -- \
        sign "$SEED" "$APP" "$ID" "$VERSION" \
        "Aether Test Publisher" "$RECORD" linux-appimage "$MEMORY" >/dev/null
else
    cargo run -q -p aether-appimage --bin appimage-manifest -- \
        sign "$SEED" "$APP" "$ID" "$VERSION" \
        "Aether Test Publisher" "$RECORD" linux-appimage >/dev/null
fi

PUBLIC_LINE=$(cargo run -q -p aether-admin -- key public --seed "$SEED")
case $PUBLIC_LINE in
    public-key=*) PUBLIC_KEY=${PUBLIC_LINE#public-key=} ;;
    *)
        echo "error: could not derive the publisher key from $SEED" >&2
        exit 1
        ;;
esac
case $PUBLIC_KEY in
    "" | *[!0-9a-f]*)
        echo "error: derived publisher key is not lowercase hexadecimal" >&2
        exit 1
        ;;
esac
if [ ${#PUBLIC_KEY} -ne 64 ]; then
    echo "error: derived publisher key is not 32 bytes" >&2
    exit 1
fi

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    verify "$RECORD" "$APP" "Aether Test Publisher:$PUBLIC_KEY" >/dev/null

echo "appimage sha256: $ACTUAL"
echo "import record: $RECORD"

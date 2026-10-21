#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Sign the pinned GridCraft AppImage for the Gate 3 headless import.
#
# The digest is pinned so the build fails closed if the supplied file is not
# the reviewed GridCraft 0.8.0 artifact. The AppImage is read in place: this
# script never copies or rewrites it. The signed linux-appimage record pins the
# identity, version, exact digest, and test publisher, and the record is
# verified against the image before the script succeeds.
#
# Usage: build-gate3-appimage.sh <appimage> <out.record> <seed>
set -eu
if [ "$#" -ne 3 ]; then
    echo "usage: build-gate3-appimage.sh <appimage> <out.record> <seed>" >&2
    exit 2
fi
APP=$1
RECORD=$2
SEED=$3

PINNED=942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77

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

ACTUAL=$(sha256sum "$APP" | cut -d' ' -f1)
if [ "$ACTUAL" != "$PINNED" ]; then
    echo "error: AppImage digest does not match the pinned GridCraft artifact" >&2
    echo "  expected: $PINNED" >&2
    echo "  actual:   $ACTUAL" >&2
    exit 1
fi

cargo run -q -p aether-appimage --bin appimage-manifest -- \
    sign "$SEED" "$APP" ai.storyteller.gridcraft 0.8.0 \
    "Aether Test Publisher" "$RECORD" linux-appimage >/dev/null

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

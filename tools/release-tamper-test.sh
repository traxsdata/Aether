#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Release-bundle tamper acceptance test.
#
# Takes an already verified bundle and mutates it two ways. Each attempt must
# make `aether-admin release verify` fail; the checksum tamper must fail with
# the specific cross-check message, not an incidental error. A tamper that is
# accepted, or a failure for the wrong reason, is a test failure.
#
# Required:
#   AETHER_TRUST   <publisher>:<64 hex character public key>
#   AETHER_BUNDLE  directory of the verified bundle (release-info.txt ...)
set -eu

cd "$(dirname "$0")/.."

if [ -z "${AETHER_TRUST:-}" ]; then
    echo "error: set AETHER_TRUST to <publisher>:<public-key-hex>" >&2
    exit 1
fi
if [ -z "${AETHER_BUNDLE:-}" ]; then
    echo "error: set AETHER_BUNDLE to the directory of the verified release bundle" >&2
    exit 1
fi
BUNDLE=$AETHER_BUNDLE
if [ ! -f "$BUNDLE/release-info.txt" ]; then
    echo "error: no bundle to tamper with: $BUNDLE/release-info.txt" >&2
    exit 1
fi

IMAGE_NAME=$(sed -n 's/^image=//p' "$BUNDLE/release-info.txt")
if [ -z "$IMAGE_NAME" ]; then
    echo "error: bundle release-info.txt does not declare an image" >&2
    exit 1
fi

verify() {
    # Prints the combined output so the caller can assert the failure reason.
    AETHER_RELEASE_DIR="$1" sh tools/release-check.sh 2>&1
}

# Both mutations run under one work directory so the EXIT trap removes every
# copy; replacing TAMPERED for the second step previously leaked the first.
WORK=$(mktemp -d "${TMPDIR:-/tmp}/aether-tamper.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# Step 1: a poisoned SHA256SUMS entry must be refused before the image even
# reaches the signature verifier, and the refusal must name the disagreement.
TAMPERED="$WORK/sums"
mkdir "$TAMPERED"
cp "$BUNDLE"/* "$TAMPERED"/
# Flip the first digest character to a hex digit that is guaranteed to be
# different from the original, otherwise a digest that already starts with
# the replacement character would not change at all and the verify step
# would correctly accept it.
# SHA256SUMS has multiple records. Read only the first line: `cut -c1` on the
# whole file yields several characters separated by newlines, so even a leading
# zero would miss the `0)` case and be replaced with itself.
ORIGINAL=$(head -n 1 "$TAMPERED/SHA256SUMS" | cut -c1)
case "$ORIGINAL" in
    0) REPLACEMENT=a ;;
    *) REPLACEMENT=0 ;;
esac
sed -i "1s/^./$REPLACEMENT/" "$TAMPERED/SHA256SUMS"
if OUTPUT=$(verify "$TAMPERED"); then
    echo "error: a tampered SHA256SUMS entry was accepted" >&2
    echo "$OUTPUT"
    exit 1
fi
case "$OUTPUT" in
*SHA256SUMS*"and release-info.txt disagree about the image"*) ;;
*)
    echo "error: tampered SHA256SUMS was refused for the wrong reason" >&2
    echo "$OUTPUT" >&2
    exit 1
    ;;
esac
echo "tamper: SHA256SUMS entry rejected"

# Step 2: one flipped byte inside the image must break the signature that is
# verified against the (still consistent) checksums and manifest files.
TAMPERED="$WORK/image"
mkdir "$TAMPERED"
cp "$BUNDLE"/* "$TAMPERED"/
IMAGE="$TAMPERED/$IMAGE_NAME"
IMAGE_SIZE=$(wc -c <"$IMAGE")
OFFSET=262144
if [ "$IMAGE_SIZE" -le "$OFFSET" ]; then
    echo "error: release image is smaller than the tamper offset" >&2
    exit 1
fi
BYTE=$(dd if="$IMAGE" bs=1 skip=$OFFSET count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
if [ -z "$BYTE" ]; then
    echo "error: could not read an image byte at offset $OFFSET" >&2
    exit 1
fi
FLIPPED=$(printf '\\%03o' "$((0x$BYTE ^ 0xff))")
printf "$FLIPPED" | dd of="$IMAGE" bs=1 seek=$OFFSET count=1 conv=notrunc 2>/dev/null
if OUTPUT=$(verify "$TAMPERED"); then
    echo "error: a flipped image byte was accepted" >&2
    echo "$OUTPUT"
    exit 1
fi
case "$OUTPUT" in
*"DigestMismatch"*) ;;
*)
    echo "error: flipped image byte was refused for the wrong reason" >&2
    echo "$OUTPUT" >&2
    exit 1
    ;;
esac
echo "tamper: flipped image byte rejected"
echo "tamper: verify refused both mutations"

#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Assemble and sign a release bundle.
#
# Unsigned releases are refused: signing is a required step, not an option.
# The signing key is a file outside the repository; never commit a release key.
#
# Required:
#   AETHER_SIGNING_KEY   path to a 32-byte (64 hex character) Ed25519 seed
#   AETHER_PUBLISHER     publisher display name
#   AETHER_TRUST_PUBLIC_KEY_FILE
#                        hex public key the kernel trusts for packages,
#                        updates, and Linux runtimes; it must not be the
#                        checked-in test key
# Optional:
#   AETHER_ALLOW_TEST_TRUST  set to 1 only for demos: lets the kernel keep the
#                            test trust root, whose private seed is public
#   AETHER_VERSION           default 0.2.0
#   AETHER_RELEASE_CHANNEL   default developer
#   AETHER_RELEASE_COUNTER   default the git commit count
#   AETHER_RELEASE_DIR       default build/release
set -eu

cd "$(dirname "$0")/.."

if [ -z "${AETHER_SIGNING_KEY:-}" ]; then
    echo "error: set AETHER_SIGNING_KEY to a 32-byte hex seed file" >&2
    echo "       unsigned releases are not produced" >&2
    exit 1
fi
if [ ! -f "$AETHER_SIGNING_KEY" ]; then
    echo "error: signing key file not found: $AETHER_SIGNING_KEY" >&2
    exit 1
fi
if [ -z "${AETHER_PUBLISHER:-}" ]; then
    echo "error: set AETHER_PUBLISHER to the publisher display name" >&2
    exit 1
fi

# The kernel embeds its trust root at build time. With the default (the
# checked-in test key) anyone can sign packages, updates, and runtimes the
# released kernel accepts, so a real release must name its own key.
TEST_KEY=$(tr -d ' \n' < tests/fixtures/keys/test.pub | tr 'A-F' 'a-f')
TRUST_KEY=$TEST_KEY
if [ -n "${AETHER_TRUST_PUBLIC_KEY_FILE:-}" ]; then
    if [ ! -f "$AETHER_TRUST_PUBLIC_KEY_FILE" ]; then
        echo "error: trust key file not found: $AETHER_TRUST_PUBLIC_KEY_FILE" >&2
        exit 1
    fi
    TRUST_KEY=$(tr -d ' \n' < "$AETHER_TRUST_PUBLIC_KEY_FILE" | tr 'A-F' 'a-f')
    # Cargo runs the kernel build script from kernel/, so pass an absolute path.
    AETHER_TRUST_PUBLIC_KEY_FILE=$(cd "$(dirname "$AETHER_TRUST_PUBLIC_KEY_FILE")" && pwd)/$(basename "$AETHER_TRUST_PUBLIC_KEY_FILE")
    export AETHER_TRUST_PUBLIC_KEY_FILE
fi
if [ "$TRUST_KEY" = "$TEST_KEY" ] && [ "${AETHER_ALLOW_TEST_TRUST:-0}" != "1" ]; then
    echo "error: the kernel would trust the checked-in test key, whose seed is public" >&2
    echo "       set AETHER_TRUST_PUBLIC_KEY_FILE to the release public key" >&2
    echo "       (AETHER_ALLOW_TEST_TRUST=1 is for demos only)" >&2
    exit 1
fi

VERSION=${AETHER_VERSION:-0.2.0}
CHANNEL=${AETHER_RELEASE_CHANNEL:-developer}
COUNTER=${AETHER_RELEASE_COUNTER:-$(git rev-list --count HEAD)}
OUT=${AETHER_RELEASE_DIR:-build/release}
COMMIT=$(git rev-parse HEAD 2>/dev/null || echo unknown)

make iso

ISO=$(ls -t build/ISOs/*.iso 2>/dev/null | head -n 1)
if [ -z "$ISO" ]; then
    echo "error: make iso did not produce an ISO under build/ISOs" >&2
    exit 1
fi

mkdir -p "$OUT"
rm -f "$OUT/aether-$VERSION.iso" "$OUT/aether-$VERSION.iso.manifest"
rm -f "$OUT/SHA256SUMS" "$OUT/release-info.txt"

cargo run -q -p aether-admin -- release build \
    --image "$ISO" \
    --name "aether-$VERSION.iso" \
    --version "$VERSION" \
    --counter "$COUNTER" \
    --channel "$CHANNEL" \
    --publisher "$AETHER_PUBLISHER" \
    --seed "$AETHER_SIGNING_KEY" \
    --commit "$COMMIT" \
    --out-dir "$OUT"

echo "release bundle written to $OUT"

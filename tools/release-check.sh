#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Verify a signed release bundle against a trusted publisher key.
#
# Required:
#   AETHER_TRUST   <publisher>:<64 hex character public key>
# Optional:
#   AETHER_RELEASE_DIR   default build/release
set -eu

cd "$(dirname "$0")/.."

if [ -z "${AETHER_TRUST:-}" ]; then
    echo "error: set AETHER_TRUST to <publisher>:<public-key-hex>" >&2
    exit 1
fi

cargo run -q -p aether-admin -- release verify \
    --dir "${AETHER_RELEASE_DIR:-build/release}" \
    --trust "$AETHER_TRUST"

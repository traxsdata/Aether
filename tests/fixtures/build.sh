#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Regenerate the checked-in package and update test fixtures.
#
# The keys in tests/fixtures/keys are TEST KEYS. They are public, provide no
# security, and must never sign anything outside this repository's tests.
# `keys/*.pub` is derived from the seeds and is what the kernel trust store
# embeds, so regenerating the keys keeps the fixtures consistent.
set -eu

cd "$(dirname "$0")/../.."

ADMIN="cargo run -q -p aether-admin --"
ROOT=tests/fixtures
KEYS="$ROOT/keys"
PACKAGES="$ROOT/packages"
UPDATES="$ROOT/updates"

mkdir -p "$KEYS" "$PACKAGES/payload" "$PACKAGES/manifests" "$UPDATES/images" "$UPDATES/manifests"

if [ ! -f "$KEYS/test.seed" ]; then
    openssl rand -hex 32 > "$KEYS/test.seed"
fi
if [ ! -f "$KEYS/untrusted.seed" ]; then
    openssl rand -hex 32 > "$KEYS/untrusted.seed"
fi

$ADMIN key public --seed "$KEYS/test.seed" | sed 's/^public-key=//' > "$KEYS/test.pub"
$ADMIN key public --seed "$KEYS/untrusted.seed" | sed 's/^public-key=//' > "$KEYS/untrusted.pub"

cat > "$PACKAGES/payload/demo.bin" <<'EOF'
aether native application marker
EOF
cat > "$PACKAGES/payload/data.txt" <<'EOF'
package payload data
EOF

cat > "$PACKAGES/manifests/demo.txt" <<'EOF'
format=1
id=org.aether.demo
version=0.2.0
arch=x86_64
runtime=native
entry=demo.bin
publisher=Aether Test Publisher
channel=developer
permissions=file.read,file.write
limits.memory_kib=256
limits.open_files=4
limits.tasks=1
EOF

# The publisher key is derived from the test seed, so the source manifest does
# not repeat it. The tool renders the canonical text that is signed.
{
    cat "$PACKAGES/manifests/demo.txt"
    echo "publisher-key=$(cat "$KEYS/test.pub")"
} > "$PACKAGES/manifests/demo.signed.txt"

$ADMIN package build \
    --manifest "$PACKAGES/manifests/demo.signed.txt" \
    --payload-dir "$PACKAGES/payload" \
    --seed "$KEYS/test.seed" \
    --out "$PACKAGES/demo.aether"

# The untrusted fixture declares the untrusted key, so the verifier rejects it
# at the trust-store lookup rather than at the signature check.
{
    sed "s/^publisher=.*/publisher=Untrusted Test Publisher/" "$PACKAGES/manifests/demo.txt"
    echo "publisher-key=$(cat "$KEYS/untrusted.pub")"
} > "$PACKAGES/manifests/untrusted.signed.txt"

$ADMIN package build \
    --manifest "$PACKAGES/manifests/untrusted.signed.txt" \
    --payload-dir "$PACKAGES/payload" \
    --seed "$KEYS/untrusted.seed" \
    --out "$PACKAGES/untrusted.aether"

$ADMIN package verify \
    --package "$PACKAGES/demo.aether" \
    --trust "Aether Test Publisher:$(cat "$KEYS/test.pub")"

printf 'aether system image 0.2.0\n' > "$UPDATES/images/image-0.2.0.bin"
printf 'aether system image 0.1.0\n' > "$UPDATES/images/image-0.1.0.bin"
printf 'aether system image 0.0.9\n' > "$UPDATES/images/image-0.0.9.bin"
printf 'aether system image 0.3.0\n' > "$UPDATES/images/image-0.3.0.bin"

sign_update() {
    image=$1
    version=$2
    counter=$3
    key=$4
    output=$5
    $ADMIN update sign \
        --image "$UPDATES/images/$image" \
        --name "$image" \
        --version "$version" \
        --counter "$counter" \
        --channel developer \
        --publisher "Aether Test Publisher" \
        --seed "$KEYS/$key" \
        --out "$output"
}

sign_update image-0.2.0.bin 0.2.0 2 test.seed "$UPDATES/manifests/current.manifest"
sign_update image-0.1.0.bin 0.1.0 1 test.seed "$UPDATES/manifests/previous.manifest"
sign_update image-0.0.9.bin 0.0.9 3 test.seed "$UPDATES/manifests/downgrade.manifest"
sign_update image-0.3.0.bin 0.3.0 4 untrusted.seed "$UPDATES/manifests/forged.manifest"

$ADMIN update verify \
    --manifest "$UPDATES/manifests/current.manifest" \
    --image "$UPDATES/images/image-0.2.0.bin" \
    --trust "Aether Test Publisher:$(cat "$KEYS/test.pub")"

echo "fixtures regenerated under $ROOT"

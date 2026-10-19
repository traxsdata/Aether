#!/bin/sh
# build-kernel-drivers.sh - build the no_std Rust drivers linked into the kernel.
#
# These are not user programs: they run in ring 0, so the build pins the
# bare-metal target, disables the Rust runtime and allocator, enables
# overflow checks, and uses a panic handler that only stops that CPU.
set -eu
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
fi
command -v cargo >/dev/null 2>&1 || {
    echo "build-kernel-drivers: cargo not found" >&2
    exit 1
}

TARGET=x86_64-unknown-none
OUT=build/rust-kernel
mkdir -p "$OUT"

# The target's core is installed with `rustup target add x86_64-unknown-none`.
cargo build --release --target "$TARGET" --manifest-path drivers/Cargo.toml -p aether-nvme
cp "drivers/target/$TARGET/release/libaether_nvme.a" "$OUT/libaether_nvme.a"
echo "build-kernel-drivers: $OUT/libaether_nvme.a" >&2
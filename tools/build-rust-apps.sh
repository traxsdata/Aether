#!/bin/sh
# build-rust-apps.sh - build the Rust apps that ship with æther.
#
# Settings and App Store are clean-room Rust programs built for
# x86_64-unknown-linux-musl and wrapped by tools/mkapp.py. They draw with
# runner/system-ui (the kernel apps' C graphics and Facet), not egui. The
# kernel's system manifest, not the package header, decides their
# capabilities. Calculator, Notes, Files, Probe and Terminal stay native C
# programs built into the kernel image.
set -eu
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
fi
command -v cargo >/dev/null 2>&1 || {
    echo "build-rust-apps: cargo not found (install Rust, or run 'make RUST_APPS=0')" >&2
    exit 1
}

TARGET=x86_64-unknown-linux-musl
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
OUT=build/rust-out
mkdir -p "$OUT" build/apps

echo "build-rust-apps: building settings installer" >&2
cargo build --release --target "$TARGET" --manifest-path runner/Cargo.toml \
    -p settings -p installer

package() {
    name=$1
    title=$2
    quota=$3
    color=$4
    shift 4
    elf="runner/target/$TARGET/release/$name"
    [ -f "$elf" ] || { echo "build-rust-apps: $elf is missing" >&2; exit 1; }
    strip -o "$OUT/$name.elf" "$elf"
    tools/mkapp.py --name "$name" --title "$title" --version 0.8.0 \
        --source "aether/runner/apps/$name" --quota "$quota" --color "$color" "$@" \
        "$OUT/$name.elf" "build/apps/$name.app"
}

# Header caps are advisory for system apps; the manifest is authoritative.
package settings   Settings   256 8C96B8 --icon assets/icons/settings.png --no-files --no-storage --net
package installer  "App Store" 512 5CF2B0 --icon assets/icons/installer.png --net

echo "build-rust-apps: packages in build/apps" >&2

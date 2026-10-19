#!/bin/sh
# build-browser.sh - builds the browser (runner/browser) and packages it as
# build/apps/browser.app: a window and outgoing connections (CAP_WINDOW and
# CAP_NET in the header), no file picker and no private folder, since it
# keeps nothing.
#
#   tools/build-browser.sh            build and package
#   tools/build-browser.sh --shots    also render headless screenshots of the
#                                     test pages into build/browser-shots
#   tools/build-browser.sh --test-root CA.pem
#                                     TEST BUILD: also trust CA.pem (tests/https.txt);
#                                     never ship this package
#
# Needs Rust with the x86_64-unknown-linux-musl target. Then:
#   make && AETHER_DISK=none tools/qemu-drive.py tests/browser.txt
set -e
cd "$(dirname "$0")/.."
unset WEBNET_TEST_ROOT_PEM # an extra trusted root only ever comes from --test-root
if [ "${1:-}" = "--test-root" ]; then
    WEBNET_TEST_ROOT_PEM=$(cat "$2")
    export WEBNET_TEST_ROOT_PEM
    echo "build-browser: TEST BUILD trusting $2 as well as the built-in roots"
    shift 2
fi
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-musl --manifest-path runner/Cargo.toml -p browser
elf=runner/target/x86_64-unknown-linux-musl/release/browser
mkdir -p build/apps build/browser-out
strip -o build/browser-out/browser-stripped.elf "$elf"
tools/mkapp.py --name browser --title Browser --version 0.7.0 --source aether/runner/browser \
    --quota 512 --color 38E1F5 --icon assets/icons/browser-128.png --no-files --no-storage --net \
    build/browser-out/browser-stripped.elf build/apps/browser.app

if [ "${1:-}" = "--shots" ]; then
    out=build/browser-shots
    mkdir -p "$out/shots"
    root=$(pwd)
    (cd "$out" && BROWSER_TEST_ROOT="$root/runner/browser/testpages" \
        AETHER_SCRIPT="$root/runner/browser/tests/screens.txt" "$root/$elf")
    echo "screenshots in $out/shots"
fi

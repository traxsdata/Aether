#!/bin/sh
# Builds the Linux-layer test program (runner/linuxtest) into build/apps/linuxtest.app.
# Needs Rust with the x86_64-unknown-linux-musl target. Then: make && tools/qemu-drive.py tests/linux.txt
set -e
cd "$(dirname "$0")/.."
cargo build --release --target x86_64-unknown-linux-musl --manifest-path runner/linuxtest/Cargo.toml
mkdir -p build/apps
tools/mkapp.py --name linuxtest --title "Linux test" --version 0.1.0 --source aether/runner/linuxtest \
    --quota 256 --no-window runner/linuxtest/target/x86_64-unknown-linux-musl/release/linuxtest build/apps/linuxtest.app

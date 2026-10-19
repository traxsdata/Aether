#!/bin/sh
# Builds the eframe example (runner/examples/hello) into build/apps/hello.app.
# Needs Rust with the x86_64-unknown-linux-musl target. Then:
#   make && AETHER_DISK=none tools/qemu-drive.py tests/hello.txt
set -e
cd "$(dirname "$0")/.."
RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target x86_64-unknown-linux-musl --manifest-path runner/Cargo.toml -p hello
mkdir -p build/apps
tools/mkapp.py --name hello --title Hello --version 0.1.0 --source aether/runner/examples/hello \
    --quota 256 runner/target/x86_64-unknown-linux-musl/release/hello build/apps/hello.app

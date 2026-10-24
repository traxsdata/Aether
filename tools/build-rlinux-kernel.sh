#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Phase 0 of docs/rlinux-migration.md (issue #185): build the pinned rlinux
# kernel (bzImage) for Aether's QEMU/VM device profile.
#
# The source for the pinned commit (tools/pins/rlinux.pin) is placed in
# build/rlinux/src, reused if that tree already holds the recorded SHA, and
# otherwise obtained from:
#   1. the shared git mirror (RLINUX_MIRROR, default
#      /opt/aether-shared/rlinux-cache/rlinux.git when it exists), via
#      `git archive <sha> | tar -x` - no network when the SHA is already in
#      the mirror (a `git fetch` is the mirror's only write path); or
#   2. a GitHub archive download - the no-mirror fallback (CI, other
#      machines), from RLINUX_URL/archive/<sha>.tar.gz.
# The mirror is never cloned, re-cloned, or deleted here.
#
# Product: build/rlinux/bzImage (also build/rlinux/src/arch/x86/boot/bzImage).
#
# Environment:
#   RLINUX_MIRROR  shared rlinux mirror path (see above)
#   RLINUX_LLVM     value passed to the kernel's `make LLVM=<value>`
#                   (default -21: this machine ships clang-21, no plain
#                   clang; the kernel maps LLVM=-21 to clang-21 per its
#                   Documentation/kbuild/llvm.rst)
#   RLINUX_JOBS     build parallelism (default: nproc)
set -eu

die() {
    echo "error: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

SRC=build/rlinux/src
FRAG=$ROOT/tools/rlinux/aether.config

# --- pin --------------------------------------------------------------------
[ -f tools/pins/rlinux.pin ] || die "pin file not found: tools/pins/rlinux.pin"
RLINUX_URL=$(sed -n 's/^RLINUX_URL=//p' tools/pins/rlinux.pin | head -n1)
RLINUX_SHA=$(sed -n 's/^RLINUX_SHA=//p' tools/pins/rlinux.pin | head -n1)
[ -n "$RLINUX_URL" ] || die "RLINUX_URL missing from tools/pins/rlinux.pin"
case "$RLINUX_SHA" in
    *[!0-9a-f]*|"") die "RLINUX_SHA in tools/pins/rlinux.pin is not a hex commit SHA: '${RLINUX_SHA:-}'" ;;
esac
[ "${#RLINUX_SHA}" -eq 40 ] || die "RLINUX_SHA is not a full 40-hex commit SHA: $RLINUX_SHA"

# --- environment -------------------------------------------------------------
# bindgen (the kernel's Rust binding generator, checked by the kernel's
# rust_is_available.sh) comes from the machine toolchain area, and so does
# any cargo helper; neither ships inside the kernel source tree.
for dir in /opt/aether-shared/bin "$HOME/.cargo/bin"; do
    [ -d "$dir" ] || continue
    case ":$PATH:" in
        *":$dir:"*) ;;
        *) PATH="$PATH:$dir" ;;
    esac
done
export PATH
command -v bindgen >/dev/null 2>&1 \
    || die "bindgen not found in PATH (expected /opt/aether-shared/bin/bindgen)"

# The kernel's rust_is_available.sh requires the rust-src component of the
# selected toolchain (it compiles core from std sources). Pick the first
# installed rustup toolchain that has it - on this machine Aether pins the
# 1.99.1 toolchain without rust-src, while `stable` (same version) has it.
rust_toolchain_has_src() {
    tc=$1 # rustup toolchain name, empty = rustc on PATH
    if [ -n "$tc" ]; then
        sysroot=$(rustup run "$tc" rustc --print sysroot 2>/dev/null) \
            || return 1
    else
        sysroot=$(rustc --print sysroot 2>/dev/null) || return 1
    fi
    test -f "$sysroot/lib/rustlib/src/rust/library/core/src/lib.rs"
}

if command -v rustup >/dev/null 2>&1; then
    for tc in stable $(rustup toolchain list 2>/dev/null | awk '{print $1}'); do
        [ -n "$tc" ] || continue
        if rust_toolchain_has_src "$tc"; then
            RUSTUP_TOOLCHAIN=$tc
            break
        fi
    done
fi
if [ -z "${RUSTUP_TOOLCHAIN:-}" ]; then
    rust_toolchain_has_src "" \
        || die "no usable Rust toolchain: the kernel build needs a rustc with the rust-src component"
    RUSTUP_TOOLCHAIN=
else
    export RUSTUP_TOOLCHAIN
    echo "kernel rust toolchain: ${RUSTUP_TOOLCHAIN} (rustc + rust-src)"
fi

RLINUX_LLVM=${RLINUX_LLVM:--21}
case "$RLINUX_LLVM" in
    1 | -* | */*) ;;
    *) die "RLINUX_LLVM must be 1, a path prefix ending in /, or a -N suffix (kernel llvm.rst): got $RLINUX_LLVM" ;;
esac

RLINUX_JOBS=${RLINUX_JOBS:-$(nproc 2>/dev/null || echo 4)}
echo "kernel build: LLVM=$RLINUX_LLVM jobs=$RLINUX_JOBS"

# --- source ------------------------------------------------------------------
# RLINUX_MIRROR defaults to the shared mirror when it exists. A non-empty
# value that does not name an existing directory (e.g. /nonexistent) means
# "no mirror available" and selects the archive download (issue #185).
if [ -z "${RLINUX_MIRROR:-}" ] && [ -d /opt/aether-shared/rlinux-cache/rlinux.git ]; then
    RLINUX_MIRROR=/opt/aether-shared/rlinux-cache/rlinux.git
fi
use_mirror=no
if [ -n "${RLINUX_MIRROR:-}" ] && [ -d "${RLINUX_MIRROR}" ]; then
    use_mirror=yes
fi

existing=""
if [ -f "$SRC/.aether-rlinux-sha" ]; then
    existing=$(cat "$SRC/.aether-rlinux-sha")
fi
if [ "$existing" = "$RLINUX_SHA" ] && [ -f "$SRC/Makefile" ]; then
    echo "rlinux source: reused (build/rlinux/src @ $RLINUX_SHA)"
else
    rm -rf "$SRC"
    mkdir -p build/rlinux
    if [ "$use_mirror" = "yes" ]; then
        fetched=""
        if ! git -C "$RLINUX_MIRROR" cat-file -e "${RLINUX_SHA}^{commit}" 2>/dev/null; then
            # The one permitted write to the mirror (issue #185): fetch the
            # missing object, never clone, re-clone, or delete it.
            git -C "$RLINUX_MIRROR" fetch -q origin
            fetched=" (fetched)"
        fi
        mkdir -p "$SRC"
        git -C "$RLINUX_MIRROR" archive "$RLINUX_SHA" | tar -x -C "$SRC"
        echo "rlinux source: mirror$fetched"
    else
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        curl -fsSL --output "$tmp/rlinux.tar.gz" "$RLINUX_URL/archive/$RLINUX_SHA.tar.gz" \
            || die "archive download failed: $RLINUX_URL/archive/$RLINUX_SHA.tar.gz"
        tar -xf "$tmp/rlinux.tar.gz" -C "$tmp"
        archive_root=$(find "$tmp" -mindepth 1 -maxdepth 1 -type d | sort | head -n1)
        [ -n "$archive_root" ] && [ -f "$archive_root/Makefile" ] \
            || die "archive did not contain a kernel tree (Makefile): $RLINUX_URL/archive/$RLINUX_SHA.tar.gz"
        mv "$archive_root" "$SRC"
        rm -rf "$tmp"
        trap - EXIT
        echo "rlinux source: download"
    fi
    [ -f "$SRC/Makefile" ] || die "extraction did not produce $SRC/Makefile"
    echo "$RLINUX_SHA" >"$SRC/.aether-rlinux-sha"
fi
[ -f "$FRAG" ] || die "config fragment not found: tools/rlinux/aether.config"

# --- configure, verify, build -------------------------------------------------
cd "$SRC"

kmake() {
    make "LLVM=$RLINUX_LLVM" "$@"
}

# Fail the build if any symbol from the fragment is missing from the final
# .config or not at the value the fragment asked for: Kconfig silently
# drops symbols whose dependencies are unmet, so every symbol is checked
# (issue #185 acceptance), not a sample.
verify_fragment() {
    bad=0
    while IFS= read -r line; do
        case "$line" in
            CONFIG_*) ;;
            *) continue ;;
        esac
        sym=${line%%=*}
        want=${line#*=}
        if grep -q "^${sym}=${want}$" .config; then
            continue
        fi
        state=$(sed -n -e "s/^${sym}=.*$/&/p" -e "s/^# ${sym} is not set$/&/p" .config)
        [ -n "$state" ] || state="symbol absent"
        echo "error: fragment requested ${sym}=${want}, final .config has: $state" >&2
        bad=1
    done <"$FRAG"
    if [ "$bad" -ne 0 ]; then
        die "tools/rlinux/aether.config is not satisfied by the final .config in $SRC (see errors above)"
    fi
}

kmake x86_64_defconfig
sh scripts/kconfig/merge_config.sh -m .config "$FRAG"
kmake olddefconfig
verify_fragment

kmake rustavailable

kmake -j"$RLINUX_JOBS" bzImage
[ -f arch/x86/boot/bzImage ] || die "kernel build finished but arch/x86/boot/bzImage is missing"
cp -f arch/x86/boot/bzImage "$ROOT/build/rlinux/bzImage"
echo "rlinux-kernel: bzImage ready: $ROOT/build/rlinux/bzImage"

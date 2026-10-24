#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Phase 0 of docs/rlinux-migration.md (issue #186): build the minimal
# initramfs the rlinux kernel boot test boots into.
#
# Product: build/rlinux/initramfs.cpio.gz
#
# The image contains only what a "boot to a shell" test needs:
#   - the host's /bin/busybox (must be statically linked - the kernel cannot
#     load dynamic binaries without a libc, and Aether userspace is explicitly
#     out of scope for phase 0), verified before use;
#   - the applet symlinks the busybox build provides (from `busybox --list`);
#   - /init: mounts proc, sys and devtmpfs, prints AETHER-RLINUX: init, then
#     execs a busybox shell on the serial console (the kernel's /dev/console
#     is ttyS0, so the shell's stdin/stdout are the QEMU serial chardev).
#
# RLINUX_INITRAMFS_NO_INIT=1 omits /init (and the shell it starts): used by
# the deliberate negative run where the boot must FAIL, not pass (issue #186
# acceptance). Without any init the kernel panics with "No working init
# found".
#
# Environment:
#   RLINUX_BUSYBOX  busybox binary to embed (default: /bin/busybox)
set -eu

die() {
    echo "error: $*" >&2
    exit 1
}

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
OUT="$ROOT/build/rlinux/initramfs.cpio.gz"
BUSYBOX=${RLINUX_BUSYBOX:-/bin/busybox}
NO_INIT=${RLINUX_INITRAMFS_NO_INIT:-0}

[ -x "$BUSYBOX" ] || die "busybox not found or not executable: $BUSYBOX (set RLINUX_BUSYBOX)"

# The kernel cannot resolve dynamic relocations: verify the binary is
# statically linked before it ends up inside an initramfs that will be used
# on a kernel with no libc.
busybox_static=no
if command -v file >/dev/null 2>&1; then
    if file "$BUSYBOX" 2>/dev/null | grep -q "statically linked"; then
        busybox_static=yes
    fi
fi
if [ "$busybox_static" = "no" ] && command -v readelf >/dev/null 2>&1; then
    if readelf -d "$BUSYBOX" >/dev/null 2>&1 && readelf -d "$BUSYBOX" 2>/dev/null | grep -q "NEEDED"; then
        : # dynamic: the DT_NEEDED entries prove a libc is required
    else
        busybox_static=yes
    fi
fi
[ "$busybox_static" = "yes" ] \
    || die "busybox is not statically linked: $BUSYBOX (the rlinux boot test cannot load a dynamic busybox)"

STAGE=$(mktemp -d)
TMP=$(mktemp -d)
trap 'rm -rf "$STAGE" "$TMP"' EXIT

mkdir -p "$STAGE/bin" "$STAGE/proc" "$STAGE/sys" "$STAGE/dev"
install -m 755 "$BUSYBOX" "$STAGE/bin/busybox"

# One symlink per applet the binary actually provides. Skip `busybox` itself
# (a bin/busybox -> busybox link would be a self-loop over the real binary).
for applet in $("$STAGE/bin/busybox" --list); do
    [ "$applet" = "busybox" ] && continue
    ln -s busybox "$STAGE/bin/$applet"
done

if [ "$NO_INIT" = "1" ]; then
    echo "rlinux-initramfs: building WITHOUT /init (deliberate negative image)"
else
    cat >"$STAGE/init" <<'EOF'
#!/bin/busybox sh
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
echo AETHER-RLINUX: init
exec busybox setsid cttyhack sh
EOF
    chmod 755 "$STAGE/init"
fi

mkdir -p build/rlinux
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc --quiet | gzip >"$TMP/initramfs.cpio.gz" )
mv -f "$TMP/initramfs.cpio.gz" "$OUT"

# Verify what we just built: /init must be present (unless deliberately
# omitted), and the busybox binary must be there. (cpio listings may or may
# not carry the `./` prefix depending on the cpio version.)
listing=$(gzip -dc "$OUT" | cpio -t)
printf '%s\n' "$listing" | grep -Eq '^(\./)?bin/busybox$' || die "initramfs is missing bin/busybox"
if [ "$NO_INIT" != "1" ]; then
    printf '%s\n' "$listing" | grep -Eq '^(\./)?init$' || die "initramfs is missing /init"
fi

if [ -f "$STAGE/init" ]; then
    printf 'init:\n%s\n' "$(cat "$STAGE/init")"
fi
echo "rlinux-initramfs: $OUT ($(wc -c <"$OUT" | tr -d ' ') bytes)"

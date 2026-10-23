#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Add the Firefox headless discovery payload to the Gate 3 runtime root.
#
# This is test scaffolding for `make firefox-headless-test` (issue #107), not
# a supported runtime profile:
#
# * The kernel keeps only the selected program from an imported AppImage; the
#   rest of the tree is not visible to the guest yet. Until it is, the staged
#   Firefox tree is also copied into the runtime root at the same path
#   (/opt/firefox), so libxul.so, omni.ja and dependentlibs.list resolve
#   next to the program.
# * libxul.so links GTK 3, X11, fontconfig, glib, dbus and libstdc++. The Gate 3
#   root carries only glibc, so the host copies of those libraries are added
#   (the same host-derived approach the Gate 3 glibc pin uses).
# * A local test page and a default-prefs file that asks for one process.
#
# The caller re-signs the root with build-runtime-rootfs.sh afterwards.
#
# Usage: firefox-headless-root.sh <staged-firefox-root> <runtime-root>
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: firefox-headless-root.sh <staged-firefox-root> <runtime-root>" >&2
    exit 2
fi
STAGED=$1
ROOT=$2
FIREFOX=$STAGED/opt/firefox
LIBDIR=/lib/x86_64-linux-gnu

if [ ! -f "$FIREFOX/libxul.so" ]; then
    echo "error: $FIREFOX/libxul.so not found; run make firefox-stage first" >&2
    exit 1
fi
if [ ! -d "$ROOT/lib/x86_64-linux-gnu" ]; then
    echo "error: $ROOT is not a Gate 3 runtime root" >&2
    exit 1
fi

rm -rf "$ROOT/opt/firefox"
mkdir -p "$ROOT/opt"
cp -a "$FIREFOX" "$ROOT/opt/firefox"

# Host libraries that libxul.so and firefox-bin need, excluding Firefox's own.
FIREFOX_ABS=$(cd "$FIREFOX" && pwd)
LD_LIBRARY_PATH=$FIREFOX_ABS ldd "$FIREFOX_ABS/libxul.so" "$FIREFOX_ABS/firefox-bin" \
    > "$ROOT/.ldd" 2>&1 || true
if grep -q "not found" "$ROOT/.ldd"; then
    grep "not found" "$ROOT/.ldd" >&2
    echo "error: the host lacks libraries libxul.so needs (install GTK 3)" >&2
    exit 1
fi
count=0
for library in $(awk '/=>/ { print $3 }' "$ROOT/.ldd" | sort -u); do
    case $library in
        "$FIREFOX_ABS"/*) continue ;;
    esac
    name=$(basename "$library")
    if [ ! -e "$ROOT/lib/x86_64-linux-gnu/$name" ]; then
        cp -L "$library" "$ROOT/lib/x86_64-linux-gnu/$name"
        count=$((count + 1))
    fi
done
rm -f "$ROOT/.ldd"
# glibc's split-out stubs that dlopen-based code still names.
for library in libdl.so.2 libpthread.so.0 librt.so.1 libresolv.so.2; do
    if [ -e "$LIBDIR/$library" ] && [ ! -e "$ROOT/lib/x86_64-linux-gnu/$library" ]; then
        cp -L "$LIBDIR/$library" "$ROOT/lib/x86_64-linux-gnu/$library"
    fi
done

# Single-process headless defaults. Firefox reads <install>/defaults/pref/*.js
# as default prefs, so no writable profile seed is needed.
mkdir -p "$ROOT/opt/firefox/defaults/pref"
cat > "$ROOT/opt/firefox/defaults/pref/aether-headless.js" <<'EOF'
// Aether headless discovery run (issue #107).
pref("browser.tabs.remote.autostart", false);
pref("fission.autostart", false);
pref("browser.shell.checkDefaultBrowser", false);
pref("app.update.enabled", false);
pref("datareporting.policy.dataSubmissionEnabled", false);
pref("toolkit.telemetry.enabled", false);
pref("browser.startup.homepage_override.mstone", "ignore");
pref("gfx.webrender.software", true);
pref("layers.acceleration.disabled", true);
EOF

mkdir -p "$ROOT/usr/share/firefox-test"
cat > "$ROOT/usr/share/firefox-test/index.html" <<'EOF'
<!doctype html>
<html><head><meta charset="utf-8"><title>Aether</title>
<style>
body { margin: 0; background: #1d3557; color: #f1faee; font: 48px sans-serif; }
.band { height: 120px; background: linear-gradient(90deg, #e63946, #ffb703, #2a9d8f); }
</style></head>
<body><div class="band"></div><p>AETHER_FIREFOX_PAGE_OK</p></body></html>
EOF

# A minimal fontconfig setup with the host's DejaVu faces (issue #109 owns
# the real, pinned font set). Written, not copied: the host conf.d holds
# symlinks with `..`, which the importer rejects.
mkdir -p "$ROOT/etc/fonts" "$ROOT/usr/share/fonts/dejavu"
cat > "$ROOT/etc/fonts/fonts.conf" <<'EOF'
<?xml version="1.0"?>
<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
<fontconfig>
  <dir>/usr/share/fonts</dir>
  <cachedir>/tmp/fontconfig</cachedir>
</fontconfig>
EOF
for face in DejaVuSans.ttf DejaVuSerif.ttf DejaVuSansMono.ttf; do
    for directory in /usr/share/fonts/truetype/dejavu /usr/share/fonts/dejavu; do
        if [ -f "$directory/$face" ]; then
            cp -L "$directory/$face" "$ROOT/usr/share/fonts/dejavu/$face"
            break
        fi
    done
done

echo "firefox headless root: copied $count host libraries into $ROOT"

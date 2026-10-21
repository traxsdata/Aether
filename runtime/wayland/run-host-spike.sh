#!/bin/sh
# Host-only smoke test: the app's wayland-client opens a dummy Unix socket, then
# the bridge ignores that fd and uses its in-process backend. Not Aether evidence.
set -eu
ROOT=${1:-/tmp/opencode/gridcraft-extract/usr/bin/gridcraft}
BUILD=${BUILD:-/tmp/opencode/g4-shim-build}
export LD_LIBRARY_PATH="$BUILD:/usr/lib/x86_64-linux-gnu"
export WAYLAND_DISPLAY=wayland-0
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp/opencode/g4-xdg}
export VK_ICD_FILENAMES=${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}
export WGPU_BACKEND=vulkan
export GRIDCRAFT_NO_PREFS=1
exec "$ROOT" --control "${CONTROL_PORT:-7979}" "$@"

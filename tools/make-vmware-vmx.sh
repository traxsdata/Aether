#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Emit a VMware Workstation VM definition that boots the newest Aether ISO.
set -eu

cd "$(dirname "$0")/.."

BUILD="${BUILD:-build}"
VMX="$BUILD/aether-vmware.vmx"
ISO=$(ls -1t "$BUILD/ISOs"/*.iso 2>/dev/null | head -n 1 || true)

if [ -z "${ISO:-}" ]; then
    echo "error: no ISO under $BUILD/ISOs; run 'make iso' first" >&2
    exit 2
fi

case "$ISO" in
    /*) ISO_PATH="$ISO" ;;
    *) ISO_PATH="$(cd "$(dirname "$ISO")" && pwd)/$(basename "$ISO")" ;;
esac

cat >"$VMX" <<EOF
.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "20"
displayName = "Aether"
guestOS = "other-64"
firmware = "efi"
uefi.secureBoot.enabled = "FALSE"
memsize = "2048"
numvcpus = "2"
cpuid.coresPerSocket = "2"

# Boot the generated El Torito image as a virtual CD-ROM.
ide1:0.present = "TRUE"
ide1:0.deviceType = "cdrom-image"
ide1:0.fileName = "$ISO_PATH"
ide1:0.startConnected = "TRUE"
ide1:0.autodetect = "FALSE"

# Aether's prototype has no networking, audio, or USB stack.
ethernet0.present = "FALSE"
sound.present = "FALSE"
usb.present = "FALSE"

# Keep the virtual SVGA adapter in linear framebuffer mode.
svga.autodetect = "FALSE"
svga.vramSize = "134217728"
mks.enable3d = "FALSE"
mks.enableVulkan = "FALSE"

# The emulated PS/2 controller supplies both the keyboard and the mouse.
mouse.vusb.enable = "FALSE"
keyboard.vusb.enable = "FALSE"
EOF

echo "$VMX"

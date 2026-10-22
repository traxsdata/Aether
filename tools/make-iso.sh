#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Build a UEFI El Torito ISO from the current loader image.
set -eu

cd "$(dirname "$0")/.."

BUILD=build
ISO_DIR="$BUILD/ISOs"
STAGE="$BUILD/iso-root"
EFI_IMAGE="$BUILD/efiboot.img"
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
ISO_PREFIX=${ISO_PREFIX:-aether}
ISO="$ISO_DIR/$ISO_PREFIX-$STAMP.iso"
# The default ISO carries the standard fixture. A demo or test target can pass
# the artifacts it already staged into the disk image; skipping the rebuild
# guarantees the ISO carries exactly those bytes.
STAGED_APPIMAGE=${STAGED_APPIMAGE:-$BUILD/linux-user.AppImage}
STAGED_APPIMAGE_RECORD=${STAGED_APPIMAGE_RECORD:-$BUILD/linux-user.import}
STAGED_RUNTIME_ROOTFS=${STAGED_RUNTIME_ROOTFS:-$BUILD/linux-runtime.sqfs}
STAGED_RUNTIME_RECORD=${STAGED_RUNTIME_RECORD:-$BUILD/linux-runtime.import}
# Optional directory produced by tools/stage-craft-apps.py. It carries
# apps.pack, apps.index, and manifest.txt; the pack replaces the single-app
# artifacts so the kernel can import any bundled Craft app on demand.
STAGED_CRAFT_APPS_DIR=${STAGED_CRAFT_APPS_DIR:-}
AETHER_ISO_SKIP_IMAGE=${AETHER_ISO_SKIP_IMAGE:-0}
# The standard fixture fits in 32 MiB. A staged GUI runtime plus its AppImage
# needs more room, so callers can size the El Torito FAT image explicitly.
EFI_IMAGE_SIZE=${EFI_IMAGE_SIZE:-32M}

for artifact in "$STAGED_RUNTIME_ROOTFS" "$STAGED_RUNTIME_RECORD"; do
    if [ ! -f "$artifact" ]; then
        echo "error: staged boot artifact missing: $artifact" >&2
        exit 2
    fi
done
if [ -n "$STAGED_CRAFT_APPS_DIR" ]; then
    for artifact in "$STAGED_CRAFT_APPS_DIR/apps.pack" \
        "$STAGED_CRAFT_APPS_DIR/apps.index"; do
        if [ ! -f "$artifact" ]; then
            echo "error: staged Craft app artifact missing: $artifact" >&2
            exit 2
        fi
    done
else
    for artifact in "$STAGED_APPIMAGE" "$STAGED_APPIMAGE_RECORD"; do
        if [ ! -f "$artifact" ]; then
            echo "error: staged boot artifact missing: $artifact" >&2
            exit 2
        fi
    done
fi

if ! command -v xorriso >/dev/null 2>&1; then
    echo "error: xorriso is required to build an ISO" >&2
    exit 2
fi

if [ "$AETHER_ISO_SKIP_IMAGE" != "1" ]; then
    make image >/dev/null
fi
mkdir -p "$ISO_DIR" "$STAGE/EFI/BOOT"
rm -f "$STAGE/EFI/BOOT/BOOTX64.EFI" "$STAGE/efiboot.img"
cp "$BUILD/BOOTX64.EFI" "$STAGE/EFI/BOOT/BOOTX64.EFI"

# El Torito UEFI entries boot a FAT image, not a bare PE application. Build a
# small EFI system image and point the catalog at it.
truncate -s "$EFI_IMAGE_SIZE" "$EFI_IMAGE"
# Let mformat choose FAT12/16 for this small image. Forcing FAT32 (-F) creates
# fewer clusters than the FAT32 minimum, which UEFI firmware refuses to mount.
mformat -i "$EFI_IMAGE" -v AETHER ::
mmd -i "$EFI_IMAGE" ::/EFI ::/EFI/BOOT
mcopy -i "$EFI_IMAGE" "$BUILD/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
# The loader stages the signed AppImage from the boot volume; the ISO must
# carry the same artifacts as the disk image.
mmd -i "$EFI_IMAGE" ::/linux
if [ -n "$STAGED_CRAFT_APPS_DIR" ]; then
    mcopy -i "$EFI_IMAGE" "$STAGED_CRAFT_APPS_DIR/apps.pack" ::/linux/apps.pack
    mcopy -i "$EFI_IMAGE" "$STAGED_CRAFT_APPS_DIR/apps.index" ::/linux/apps.index
    if [ -f "$STAGED_CRAFT_APPS_DIR/manifest.txt" ]; then
        mcopy -i "$EFI_IMAGE" "$STAGED_CRAFT_APPS_DIR/manifest.txt" \
            ::/linux/apps.manifest
    fi
else
    mcopy -i "$EFI_IMAGE" "$STAGED_APPIMAGE" ::/linux/app.AppImage
    mcopy -i "$EFI_IMAGE" "$STAGED_APPIMAGE_RECORD" ::/linux/app.import
fi
mcopy -i "$EFI_IMAGE" "$STAGED_RUNTIME_ROOTFS" ::/linux/runtime.sqfs
mcopy -i "$EFI_IMAGE" "$STAGED_RUNTIME_RECORD" ::/linux/runtime.import
cp "$EFI_IMAGE" "$STAGE/efiboot.img"

xorriso -as mkisofs \
    -R -J \
    -V AETHER \
    -e efiboot.img \
    -no-emul-boot \
    -boot-load-size 4 \
    -isohybrid-gpt-basdat \
    -o "$ISO" \
    "$STAGE" >/dev/null

echo "$ISO"

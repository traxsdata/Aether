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
ISO="$ISO_DIR/aether-$STAMP.iso"

if ! command -v xorriso >/dev/null 2>&1; then
    echo "error: xorriso is required to build an ISO" >&2
    exit 2
fi

make image >/dev/null
mkdir -p "$ISO_DIR" "$STAGE/EFI/BOOT"
rm -f "$STAGE/EFI/BOOT/BOOTX64.EFI" "$STAGE/efiboot.img"
cp "$BUILD/BOOTX64.EFI" "$STAGE/EFI/BOOT/BOOTX64.EFI"

# El Torito UEFI entries boot a FAT image, not a bare PE application. Build a
# small EFI system image and point the catalog at it.
truncate -s 8M "$EFI_IMAGE"
# Let mformat choose FAT12/16 for this small image. Forcing FAT32 (-F) creates
# fewer clusters than the FAT32 minimum, which UEFI firmware refuses to mount.
mformat -i "$EFI_IMAGE" -v AETHER ::
mmd -i "$EFI_IMAGE" ::/EFI ::/EFI/BOOT
mcopy -i "$EFI_IMAGE" "$BUILD/BOOTX64.EFI" ::/EFI/BOOT/BOOTX64.EFI
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

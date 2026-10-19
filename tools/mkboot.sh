#!/bin/sh
# mkboot.sh - build the small FAT EFI System Partition image the installer
# writes to a target disk (and the netboot ISO uses).
#
# The image only carries the UEFI loader and, for netboot, the live kernel,
# the installer package and a `netboot` marker. Everything else is a system
# image downloaded from a GitHub release, so this image changes only when the
# loader or the netboot path itself changes.
set -eu
cd "$(dirname "$0")/.."

mode=${1:-installed}
out=${2:-build/boot.img}
loader=${LOADER:-build/BOOTX64.EFI}
kernel=${KERNEL:-build/kernel.elf}
installer=${INSTALLER:-build/apps/installer.app}
size_mib=${ESP_MIB:-16}

[ -f "$loader" ] || { echo "mkboot: $loader is missing (run make)" >&2; exit 1; }
[ -f "$kernel" ] || { echo "mkboot: $kernel is missing (run make)" >&2; exit 1; }

rm -f "$out"
mkdir -p "$(dirname "$out")"
dd if=/dev/zero of="$out" bs=1M count="$size_mib" status=none
# 16 MiB defaults to FAT16 in mtools, which is fine for an ESP this small.
mformat -i "$out" -v AETHER ::
mmd -i "$out" ::/EFI ::/EFI/BOOT
mcopy -i "$out" "$loader" ::/EFI/BOOT/BOOTX64.EFI

if [ "$mode" = netboot ]; then
    [ -f "$installer" ] || { echo "mkboot: $installer is missing (run make rust-apps)" >&2; exit 1; }
    mmd -i "$out" ::/apps
    mcopy -i "$out" "$kernel" ::/kernel.elf
    mcopy -i "$out" "$installer" ::/apps/installer.app
    printf 'netboot\n' > build/netboot.marker
    mcopy -i "$out" build/netboot.marker ::/netboot
fi

echo "mkboot: $out ($mode, ${size_mib} MiB)" >&2
#!/bin/sh
# sign-secureboot.sh - sign the UEFI loader for a Secure Boot machine.
#
#   tools/sign-secureboot.sh /secure/db.key /secure/db.crt
#
# The loader is the only piece firmware starts directly. It verifies the
# signed system image (kernel + apps) with the release key before mapping any
# code, so signing the loader extends the same chain of trust through
# firmware. Rebuild the boot image and ISO after this script runs.
#
# Enrolling the certificate in the machine's db is a deliberate owner action;
# this script never touches firmware variables.
set -eu
cd "$(dirname "$0")/.."

if [ $# -ne 2 ]; then
    echo "usage: $0 <db-key.pem> <db-cert.pem>" >&2
    exit 2
fi
KEY=$1
CERT=$2
for f in "$KEY" "$CERT"; do
    [ -f "$f" ] || { echo "sign-secureboot: $f is missing" >&2; exit 1; }
done
command -v sbsign >/dev/null 2>&1 || {
    echo "sign-secureboot: sbsign not found (install sbsigntool)" >&2
    exit 1
}
[ -f build/BOOTX64.EFI ] || {
    echo "sign-secureboot: build/BOOTX64.EFI is missing; run make first" >&2
    exit 1
}

sbsign --key "$KEY" --cert "$CERT" --output build/BOOTX64.EFI.signed build/BOOTX64.EFI
mv build/BOOTX64.EFI.signed build/BOOTX64.EFI
rm -f build/boot.img build/esp.img build/aether.iso
echo "sign-secureboot: loader signed; rebuild the boot image and ISO with make" >&2

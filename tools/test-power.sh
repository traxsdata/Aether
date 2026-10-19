#!/bin/sh
# Shut down from the æ menu and check that the kernel reached the ACPI path.
set -eu
cd "$(dirname "$0")/.."
AETHER_DISK=none tools/qemu-drive.py tests/power.txt build/test-power

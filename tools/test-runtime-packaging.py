#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Verify the pinned runtime package and the signed user AppImage fixture.

This is a host test for the packaging boundary the kernel relies on: the
pinned manifest digest, the signed runtime rootfs record, the signed AppImage
record, and rejection of a mutated AppImage. It never boots the kernel.
"""

from __future__ import annotations

import hashlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
PIN = BUILD / "linux-runtime-pinned.pin"
MANIFEST = BUILD / "linux-runtime-pinned.manifest"
ROOTFS = BUILD / "linux-runtime.sqfs"
ROOTFS_RECORD = BUILD / "linux-runtime.import"
APPIMAGE = BUILD / "linux-user.AppImage"
APPIMAGE_RECORD = BUILD / "linux-user.import"
PUBLIC_KEY = (ROOT / "tests/fixtures/keys/test.pub").read_text().strip()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str) -> int:
    print(f"FAIL: {message}", file=sys.stderr)
    return 1


def verify(record: Path, image: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "-p",
            "aether-appimage",
            "--bin",
            "appimage-manifest",
            "--",
            "verify",
            str(record),
            str(image),
            f"Aether Test Publisher:{PUBLIC_KEY}",
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )


def main() -> int:
    for path in (PIN, MANIFEST, ROOTFS, ROOTFS_RECORD, APPIMAGE, APPIMAGE_RECORD):
        if not path.is_file():
            return fail(f"missing package artifact {path}")

    fields = {}
    for line in MANIFEST.read_text().splitlines():
        key, value = line.split("=", 1)
        fields[key] = value
    if fields.get("profile") != "pinned-host-glibc":
        return fail("runtime profile is not the pinned host glibc profile")
    if not fields.get("glibc", "").startswith("glibc-"):
        return fail("runtime manifest does not record the glibc version")
    if fields.get("mesa") != "absent" or fields.get("wayland") != "absent":
        return fail("runtime manifest must record absent Mesa/Wayland honestly")
    if digest(ROOTFS) != fields.get("rootfs_sha256"):
        return fail("rootfs digest does not match the pinned manifest")
    packaged = BUILD / "linux-runtime-pinned.manifest.packaged"
    if not packaged.is_file() or digest(packaged) != fields.get("manifest_sha256"):
        return fail("packaged runtime manifest digest does not match the pin")
    if PIN.read_text().strip() != fields.get("manifest_sha256"):
        return fail("kernel pin file does not match the runtime manifest digest")

    result = verify(ROOTFS_RECORD, ROOTFS)
    if result.returncode != 0:
        return fail(f"runtime rootfs record rejected: {result.stderr.strip()}")

    result = verify(APPIMAGE_RECORD, APPIMAGE)
    if result.returncode != 0:
        return fail(f"AppImage record rejected: {result.stderr.strip()}")
    if "id=org.example.aether-appimage-demo" not in result.stdout:
        return fail("AppImage record does not name the imported user fixture")
    if "id=org.aether.linux-runtime" in result.stdout:
        return fail("AppImage identity is the embedded runtime identity")

    inspected = subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "-p",
            "aether-appimage",
            "--features",
            "std",
            "--bin",
            "appimage-inspect",
            "--",
            str(APPIMAGE),
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if inspected.returncode != 0:
        return fail(f"AppImage container rejected: {inspected.stderr.strip()}")
    if "appimage=type-2" not in inspected.stdout:
        return fail("AppImage container is not type 2")
    if "squashfs_compression=zstd" not in inspected.stdout:
        return fail("AppImage payload is not zstd SquashFS")

    with tempfile.TemporaryDirectory(prefix="aether-appimage-") as temporary:
        tampered = Path(temporary) / "tampered.AppImage"
        shutil.copyfile(APPIMAGE, tampered)
        with tampered.open("r+b") as handle:
            handle.seek(-1, 2)
            last = handle.read(1)
            handle.seek(-1, 2)
            handle.write(bytes([last[0] ^ 0xFF]))
        result = verify(APPIMAGE_RECORD, tampered)
        if result.returncode == 0:
            return fail("a mutated AppImage was accepted")

    print(
        "PASS: pinned runtime and signed AppImage verified; mutated AppImage rejected"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

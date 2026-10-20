#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Host verification for the Gate 4 runtime rootfs.

Checks the packaged manifest, every file digest, the signed linux-rootfs
record, and the dynamic-library closure before the image is booted. This is the
packaging gate; the QEMU boot adds the kernel-owned marker.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PUBLIC_KEY = (ROOT / "tests/fixtures/keys/test.pub").read_text().strip()
NEEDED = re.compile(r"\(NEEDED\)\s+Shared library: \[(.*?)\]")
SONAME = re.compile(r"\(SONAME\)\s+Library soname: \[(.*?)\]")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str) -> int:
    print(f"FAIL: {message}", file=sys.stderr)
    return 1


def verify_record(record: Path, image: Path) -> subprocess.CompletedProcess[str]:
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


def elf_dynamic(path: Path) -> tuple[set[str], str | None]:
    result = subprocess.run(
        ["readelf", "-d", str(path)],
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        return set(), None
    needed = set(NEEDED.findall(result.stdout))
    soname = SONAME.search(result.stdout)
    return needed, soname.group(1) if soname else None


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--record", type=Path, required=True)
    parser.add_argument("--pin", type=Path, required=True)
    args = parser.parse_args()

    for path in (args.root, args.manifest, args.rootfs, args.record, args.pin):
        if not path.exists():
            return fail(f"missing artifact {path}")

    fields: dict[str, str] = {}
    file_digests: dict[str, str] = {}
    for line in args.manifest.read_text().splitlines():
        key, value = line.split("=", 1)
        if key == "file_sha256":
            path, expected = value.split(":", 1)
            file_digests[path] = expected
        else:
            fields[key] = value

    if fields.get("profile") != "gate4-host-lavapipe":
        return fail("runtime profile is not gate4-host-lavapipe")
    if not fields.get("glibc", "").startswith("glibc-"):
        return fail("runtime manifest does not record the glibc version")
    if not fields.get("mesa", "").startswith("lavapipe-"):
        return fail("runtime manifest does not record the lavapipe version")
    if fields.get("mesa") == "lavapipe-host":
        return fail("runtime manifest did not find a host Mesa package version")
    if fields.get("wayland") in (None, "", "absent", "host"):
        return fail("runtime manifest does not record a Wayland version")

    if args.pin.read_text().strip() != digest(args.manifest):
        return fail("kernel pin file does not match the runtime manifest digest")

    if not file_digests:
        return fail("runtime manifest has no file_sha256 entries")
    packaged = {
        str(path.relative_to(args.root)).lstrip("/")
        for path in args.root.rglob("*")
        if path.is_file() and path.name != "aether-runtime.manifest"
    }
    listed = {path.lstrip("/") for path in file_digests}
    missing = sorted(packaged - listed)
    if missing:
        return fail(f"unhashed packaged files: {missing[:8]}")
    unexpected = sorted(listed - packaged)
    if unexpected:
        return fail(f"manifest names files absent from the root: {unexpected[:8]}")
    for path, expected in file_digests.items():
        actual = digest(args.root / path.lstrip("/"))
        if actual != expected:
            return fail(f"file digest mismatch: {path}")
        if not fields.get("ld_so_sha256") or not fields.get("libc_sha256"):
            return fail("manifest lacks the loader/libc digests")
    if file_digests.get("/lib64/ld-linux-x86-64.so.2") != fields.get("ld_so_sha256"):
        return fail("ld.so digest is not the manifest ld_so_sha256")
    if (
        file_digests.get("/lib/x86_64-linux-gnu/libc.so.6")
        != fields.get("libc_sha256")
    ):
        return fail("libc digest is not the manifest libc_sha256")

    if (args.root / "usr/bin/gridcraft").exists():
        return fail("GridCraft GUI must come from the AppImage, not the rootfs")

    icd = args.root / "usr/share/vulkan/icd.d/lvp_icd.json"
    if not icd.is_file():
        return fail("lavapipe ICD configuration is missing")
    import json

    icd_data = json.loads(icd.read_text())["ICD"]
    icd_library = icd_data["library_path"]
    if not (args.root / "lib/x86_64-linux-gnu" / icd_library).is_file():
        return fail(f"ICD library_path is not packaged: {icd_library}")

    library_dir = args.root / "lib/x86_64-linux-gnu"
    available: set[str] = set()
    elf_files: list[Path] = []
    for path in library_dir.iterdir():
        if not path.is_file():
            continue
        needed, soname = elf_dynamic(path)
        available.add(path.name)
        if soname:
            available.add(soname)
        elf_files.append(path)
    for path in (args.root / "lib64").iterdir():
        if path.is_file():
            available.add(path.name)
            elf_files.append(path)
    for required in (
        "libwayland-client.so.0",
        "libwayland-cursor.so.0",
        "libxkbcommon.so.0",
        "libvulkan.so.1",
        "libvulkan_lvp.so",
        "libLLVM.so.21.1",
        "libdrm.so.2",
        "libxcb.so.1",
        "libxshmfence.so.1",
        "libdisplay-info.so.3",
        "libz.so.1",
        "libzstd.so.1",
        "libexpat.so.1",
        "libstdc++.so.6",
        "libffi.so.8",
        "libedit.so.2",
        "libxml2.so.16",
        "libtinfo.so.6",
        "libbsd.so.0",
        "libmd.so.0",
        "libXau.so.6",
        "libXdmcp.so.6",
    ):
        if required not in available:
            return fail(f"required runtime library is not packaged: {required}")
    for path in elf_files:
        needed, _ = elf_dynamic(path)
        unresolved = sorted(
            name
            for name in needed
            if name not in available
            and name != "ld-linux-x86-64.so.2"
            and not name.startswith("linux-vdso")
        )
        if unresolved:
            return fail(f"{path.name} has unresolved dependencies: {unresolved}")

    result = verify_record(args.record, args.rootfs)
    if result.returncode != 0:
        return fail(f"runtime rootfs record rejected: {result.stderr.strip()}")

    compressed = args.rootfs.stat().st_size
    uncompressed = sum(
        path.stat().st_size for path in args.root.rglob("*") if path.is_file()
    )
    large = sorted(
        (
            (path.stat().st_size, str(path.relative_to(args.root)))
            for path in args.root.rglob("*")
            if path.is_file()
        ),
        reverse=True,
    )[:6]
    print(f"PASS: Gate 4 runtime manifest and {len(file_digests)} file digests verified")
    print(f"PASS: runtime closure verified ({len(available)} library names)")
    print(f"PASS: signed linux-rootfs record verified")
    print(f"rootfs: {args.rootfs} compressed={compressed} bytes uncompressed={uncompressed} bytes")
    print(
        "profile: "
        f"{fields['profile']} glibc={fields['glibc']} "
        f"mesa={fields['mesa']} wayland={fields['wayland']}"
    )
    for size, path in large:
        print(f"  {size:>12} {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

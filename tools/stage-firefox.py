#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stage the pinned Firefox ESR release as a signed Aether AppImage import.

Mozilla publishes Firefox for Linux as a tarball, not an AppImage. This script
turns the pinned tarball into the container shape the in-OS importer accepts
(``apps/linux/appimage``): an x86-64 ELF runtime stub carrying ``AI\\x02`` in
its identity padding, followed by a zstd SquashFS image. The image holds

    AppRun                    shell launcher: exec opt/firefox/firefox
    firefox.desktop, firefox.png, .DirIcon
    opt/firefox/...           the unmodified Mozilla tarball contents

``/opt/firefox`` is Mozilla's conventional install path and satisfies the
launch policy (``linuxd`` only starts programs under ``/usr/bin/`` or
``/opt/``). The record version is the numeric part (``153.5.1`` for
``153.5.1esr``), because ``linuxd`` accepts only dotted numeric versions; the
app id and metadata keep the ESR channel. The image is signed into a ``linux-appimage`` record with ``memory=jit`` (the
SpiderMonkey JIT needs writable+executable mappings; see
``linux_jit_allowed``). Aether never executes the runtime stub or AppRun: the
kernel loads the selected program from the imported tree.

The tarball is downloaded once into ``build/firefox/cache`` and verified
against the SHA-256 in ``tools/pins/firefox-esr.pin`` before it is used, so a
changed upstream file fails closed. ``make ci`` never calls this script.

Outputs (default ``build/firefox``):

    firefox.AppImage    the container
    firefox.import      signed import record (memory=jit)
    firefox.metadata    id, version, digests, and sizes
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_PIN = ROOT / "tools" / "pins" / "firefox-esr.pin"
DEFAULT_OUT = ROOT / "build" / "firefox"
DEFAULT_SEED = ROOT / "tests" / "fixtures" / "keys" / "test.seed"
PUBLISHER = "Aether Test Publisher"
PIN_KEYS = (
    "FIREFOX_VERSION",
    "FIREFOX_FILE",
    "FIREFOX_URL",
    "FIREFOX_SHA256",
    "FIREFOX_APP_ID",
)

APPRUN = """#!/bin/sh
# AppImage entry point for hosts that execute AppRun. Aether loads the program
# selected by the import policy directly and does not run this script.
HERE=$(dirname "$(readlink -f "$0")")
exec "$HERE/opt/firefox/firefox" "$@"
"""

DESKTOP = """[Desktop Entry]
Type=Application
Name=Firefox ESR
GenericName=Web Browser
Exec=firefox %u
Icon=firefox
Terminal=false
Categories=Network;WebBrowser;
MimeType=text/html;text/xml;application/xhtml+xml;x-scheme-handler/http;x-scheme-handler/https;
StartupWMClass=firefox-esr
"""

STUB_MESSAGE = (
    b"This AppImage is staged for Aether and has no FUSE runtime.\n"
    b"Extract it with: unsquashfs -o <offset> <file>, where <offset> is the\n"
    b"size of this ELF stub (the end of its section header table).\n"
)


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_pin(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, separator, value = line.partition("=")
        if not separator:
            fail(f"{path}: malformed line {line!r}")
        values[key.strip()] = value.strip()
    missing = [key for key in PIN_KEYS if not values.get(key)]
    if missing:
        fail(f"{path}: missing {', '.join(missing)}")
    digest = values["FIREFOX_SHA256"]
    if len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        fail(f"{path}: FIREFOX_SHA256 must be 64 lowercase hexadecimal characters")
    if "/" in values["FIREFOX_FILE"] or not values["FIREFOX_URL"].startswith("https://"):
        fail(f"{path}: FIREFOX_FILE must be a file name and FIREFOX_URL an https URL")
    return values


def record_version(release: str) -> str:
    """The signed record version: ``153.5.1esr`` -> ``153.5.1``."""
    version = release.removesuffix("esr")
    parts = version.split(".")
    if not 2 <= len(parts) <= 4 or not all(part.isdigit() for part in parts):
        fail(f"release {release!r} has no dotted numeric version")
    return version


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(pin: dict[str, str], cache: Path, offline: bool) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    tarball = cache / pin["FIREFOX_FILE"]
    expected = pin["FIREFOX_SHA256"]
    if tarball.exists():
        if sha256(tarball) == expected:
            print(f"cached: {tarball}")
            return tarball
        print(f"warning: {tarball} does not match the pin; downloading again", file=sys.stderr)
        tarball.unlink()
    if offline:
        fail(f"{tarball} is not cached and --offline was given")
    partial = tarball.with_name(tarball.name + ".part")
    print(f"downloading {pin['FIREFOX_URL']}")
    with urllib.request.urlopen(pin["FIREFOX_URL"], timeout=60) as response:
        with partial.open("wb") as handle:
            shutil.copyfileobj(response, handle, 1024 * 1024)
    actual = sha256(partial)
    if actual != expected:
        partial.unlink()
        fail(
            f"downloaded {pin['FIREFOX_FILE']} does not match the pin\n"
            f"  expected: {expected}\n  actual:   {actual}"
        )
    partial.rename(tarball)
    return tarball


def extract(tarball: Path, destination: Path) -> None:
    """Extract ``firefox/`` from the tarball into ``destination``."""
    destination.mkdir(parents=True)
    with tarfile.open(tarball, "r:*") as archive:
        members = []
        for member in archive.getmembers():
            name = member.name
            if name == "firefox" or not name.startswith("firefox/"):
                continue
            member.name = name[len("firefox/") :]
            members.append(member)
        if not members:
            fail(f"{tarball}: no firefox/ directory in the tarball")
        # The data filter rejects absolute paths, escapes, and device nodes.
        archive.extractall(destination, members=members, filter="data")
    for required in ("firefox", "firefox-bin", "libxul.so", "omni.ja", "application.ini"):
        if not (destination / required).is_file():
            fail(f"{tarball}: firefox/{required} is missing")


def build_tree(tarball: Path, root: Path) -> None:
    firefox = root / "opt" / "firefox"
    extract(tarball, firefox)
    # No usr/bin/firefox symlink: the importer rejects symlinks with `..`.
    apprun = root / "AppRun"
    apprun.write_text(APPRUN)
    apprun.chmod(0o755)
    (root / "firefox.desktop").write_text(DESKTOP)
    icon = firefox / "browser" / "chrome" / "icons" / "default" / "default128.png"
    if icon.is_file():
        shutil.copyfile(icon, root / "firefox.png")
        os.symlink("firefox.png", root / ".DirIcon")


def runtime_stub() -> bytes:
    """A minimal static ELF64 that prints a notice and exits with status 127.

    The importer only needs a valid x86-64 ELF header with ``AI\\x02`` at
    offset 8. A one-entry section header table ends the stub so tools that
    locate the SquashFS at ``e_shoff + e_shnum * e_shentsize`` find it too.
    """
    base = 0x400000
    code_offset = 64 + 56
    code_length = 36
    message_offset = code_offset + code_length
    rip_after_lea = code_offset + 5 + 5 + 7
    code = (
        b"\xb8\x01\x00\x00\x00"  # mov eax, 1 (write)
        + b"\xbf\x02\x00\x00\x00"  # mov edi, 2
        + b"\x48\x8d\x35"
        + struct.pack("<i", message_offset - rip_after_lea)  # lea rsi, [rip+msg]
        + b"\xba"
        + struct.pack("<I", len(STUB_MESSAGE))  # mov edx, len
        + b"\x0f\x05"  # syscall
        + b"\xb8\x3c\x00\x00\x00"  # mov eax, 60 (exit)
        + b"\xbf\x7f\x00\x00\x00"  # mov edi, 127
        + b"\x0f\x05"  # syscall
    )
    assert len(code) == code_length
    loaded = message_offset + len(STUB_MESSAGE)
    section_offset = (loaded + 7) & ~7
    ident = b"\x7fELF" + bytes([2, 1, 1, 0]) + b"AI\x02" + bytes(5)
    header = ident + struct.pack(
        "<HHIQQQIHHHHHH",
        2,  # ET_EXEC
        62,  # EM_X86_64
        1,
        base + code_offset,
        64,  # e_phoff
        section_offset,
        0,
        64,
        56,
        1,
        64,
        1,
        0,
    )
    program_header = struct.pack(
        "<IIQQQQQQ", 1, 5, 0, base, base, loaded, loaded, 0x1000
    )
    body = header + program_header + code + STUB_MESSAGE
    body += bytes(section_offset - len(body))
    return body + bytes(64)


def run_quiet(command: list[str]) -> str:
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        fail(f"command failed: {' '.join(command)}")
    return result.stdout


def cargo_bin(package: str, binary: str | None, *arguments: str) -> str:
    command = ["cargo", "run", "-q", "--locked", "-p", package]
    if binary:
        command += ["--bin", binary]
    return run_quiet(command + ["--", *arguments])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--pin", type=Path, default=DEFAULT_PIN)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--cache", type=Path, help="tarball cache (default: <out>/cache)")
    parser.add_argument("--seed", type=Path, default=DEFAULT_SEED)
    parser.add_argument("--offline", action="store_true", help="never download")
    parser.add_argument(
        "--keep-root",
        action="store_true",
        help="keep the staged tree in <out>/root (the harness reuses it)",
    )
    args = parser.parse_args()

    for tool in ("mksquashfs", "cargo"):
        if shutil.which(tool) is None:
            fail(f"{tool} not found (mksquashfs is in squashfs-tools)")
    if not args.seed.is_file():
        fail(f"signing seed not found: {args.seed}")

    pin = read_pin(args.pin)
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    tarball = fetch(pin, (args.cache or out / "cache").resolve(), args.offline)

    root = out / "root"
    if root.exists():
        shutil.rmtree(root)
    build_tree(tarball, root)

    appimage = out / "firefox.AppImage"
    record = out / "firefox.import"
    with tempfile.TemporaryDirectory(dir=out) as scratch:
        squashfs = Path(scratch) / "firefox.sqfs"
        # Fixed owners and timestamps keep the image a function of the pinned
        # tarball and the mksquashfs version.
        run_quiet(
            [
                "mksquashfs",
                str(root),
                str(squashfs),
                "-comp",
                "zstd",
                "-noappend",
                "-all-root",
                "-no-xattrs",
                "-no-progress",
                "-mkfs-time",
                "0",
                "-all-time",
                "0",
            ]
        )
        partial = appimage.with_name(appimage.name + ".part")
        with partial.open("wb") as handle:
            handle.write(runtime_stub())
            with squashfs.open("rb") as payload:
                shutil.copyfileobj(payload, handle, 1024 * 1024)
        partial.chmod(0o755)
        partial.rename(appimage)

        # The host importer uses the kernel's Limits::default(), so this proves
        # the in-OS import will accept the tree before a VM is booted.
        cargo_bin("aether-squashfs", "squashfs-extract", str(appimage), str(Path(scratch) / "x"))

    if not args.keep_root:
        shutil.rmtree(root)

    app_id = pin["FIREFOX_APP_ID"]
    version = record_version(pin["FIREFOX_VERSION"])
    cargo_bin(
        "aether-appimage",
        "appimage-manifest",
        "sign",
        str(args.seed.resolve()),
        str(appimage),
        app_id,
        version,
        PUBLISHER,
        str(record),
        "linux-appimage",
        "memory=jit",
    )
    public = cargo_bin("aether-admin", None, "key", "public", "--seed", str(args.seed.resolve()))
    public = public.strip()
    if not public.startswith("public-key="):
        fail("could not derive the publisher key from the seed")
    cargo_bin(
        "aether-appimage",
        "appimage-manifest",
        "verify",
        str(record),
        str(appimage),
        f"{PUBLISHER}:{public.removeprefix('public-key=')}",
    )

    digest = sha256(appimage)
    size = appimage.stat().st_size
    (out / "firefox.metadata").write_text(
        f"id={app_id}\nversion={version}\nrelease={pin['FIREFOX_VERSION']}\n"
        f"tarball={pin['FIREFOX_FILE']}\ntarball_sha256={pin['FIREFOX_SHA256']}\n"
        f"appimage_sha256={digest}\nappimage_bytes={size}\nmemory=jit\n"
    )
    print(f"firefox {version}: {appimage} ({size} bytes)")
    print(f"appimage sha256: {digest}")
    print(f"import record: {record}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

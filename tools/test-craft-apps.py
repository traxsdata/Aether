#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Download and test the published Craft apps in Aether.

For each app this script verifies the upstream SHA-256, extracts the AppImage
with Aether's own SquashFS reader to discover the desktop identity and
binaries, then runs the generic Aether smoke targets:

  make craft-smoke-test ... CRAFT_ARGS=--version
  make craft-gui-test   ...

The GUI run launches the app's windowed binary with no arguments, waits for
the kernel-owned AETHER_GATE4_PRESENT_OK marker, and takes a QEMU screenshot.
Results and logs are written under build/craft-apps/<app>/.

This is an external-artifact test, not part of `make ci`: it downloads large
upstream files and needs the host's QEMU/OVMF and Mesa tools. The digests it
records identify exactly what ran.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build" / "craft-apps"
DEFAULT_CACHE = Path("/tmp/opencode/craft-apps")

# Pinned upstream releases. Update deliberately when re-running the suite.
APPS = {
    "gridcraft": "0.10.0",
    "photocraft": "0.12.0",
    "pdfcraft": "0.11.0",
    "filmcraft": "0.10.0",
    "vectorcraft": "0.14.0",
    "lightcraft": "0.11.0",
    "designcraft": "0.11.0",
    "effectcraft": "0.14.0",
    "wordcraft": "0.9.0",
    "deckcraft": "0.9.0",
    "soundcraft": "0.11.0",
    "cadcraft": "0.10.0",
    "figcraft": "0.9.0",
}

# FilmCraft's CLI can print its version and then leave a worker thread blocked.
# Use the windowed binary for that app's headless version check.
HEADLESS_BINARY = {"filmcraft": "filmcraft"}

RELEASE = "https://github.com/storytold/{app}/releases/download/v{version}"
ASSET = "{app}-{version}-linux-x86_64.AppImage"


def run(command: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(command, cwd=ROOT, text=True, **kwargs)


def download(url: str, destination: Path) -> None:
    if destination.exists():
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".part")
    print(f"  download {url}")
    result = run(["curl", "-fsSL", "-o", str(temporary), url])
    if result.returncode != 0:
        temporary.unlink(missing_ok=True)
        raise RuntimeError(f"download failed: {url}")
    temporary.replace(destination)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(app: str, version: str, cache: Path) -> Path:
    image = cache / f"{app}-{version}.AppImage"
    checksums = cache / f"{app}-{version}.SHA256SUMS"
    base = RELEASE.format(app=app, version=version)
    download(f"{base}/{ASSET.format(app=app, version=version)}", image)
    download(f"{base}/SHA256SUMS.txt", checksums)
    expected = None
    wanted = ASSET.format(app=app, version=version)
    for line in checksums.read_text().splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == wanted:
            expected = parts[0]
            break
    if expected is None:
        raise RuntimeError(f"{app}: {wanted} absent from SHA256SUMS")
    actual = sha256(image)
    if actual != expected:
        raise RuntimeError(f"{app}: digest mismatch ({actual} != {expected})")
    return image


def ensure_extractor() -> Path:
    tool = ROOT / "target" / "debug" / "squashfs-extract"
    if not tool.exists():
        result = run(
            ["cargo", "build", "--locked", "-q", "-p", "aether-squashfs", "--bin", "squashfs-extract"]
        )
        if result.returncode != 0:
            raise RuntimeError("could not build squashfs-extract")
    return tool


def inspect(image: Path, extractor: Path, work: Path) -> dict:
    root = work / "root"
    if root.exists():
        shutil.rmtree(root)
    result = run([str(extractor), str(image), str(root)], capture_output=True)
    if result.returncode != 0:
        raise RuntimeError(f"extract failed: {result.stderr.strip()}")
    desktops = sorted((root / "usr/share/applications").glob("*.desktop"))
    if not desktops:
        raise RuntimeError("no .desktop file in AppImage")
    identity = desktops[0].stem
    binaries = sorted(
        entry.name
        for entry in (root / "usr/bin").iterdir()
        if entry.is_file() and os.access(entry, os.X_OK)
    )
    if not binaries:
        raise RuntimeError("no executable in usr/bin")
    cli = next((name for name in binaries if name.endswith("-cli")), None)
    gui = next((name for name in binaries if not name.endswith("-cli")), None)
    if gui is None:
        gui = cli
    return {
        "identity": identity,
        "binaries": binaries,
        "cli": cli,
        "gui": gui,
        "extracted_bytes": sum(
            entry.stat().st_size for entry in root.rglob("*") if entry.is_file()
        ),
    }


def host_version(root: Path, binary: str) -> str:
    try:
        result = run(
            [str(root / "usr/bin" / binary), "--version"],
            capture_output=True,
            timeout=30,
            env={**os.environ, "HOME": tempfile.mkdtemp(prefix="craft-home-")},
        )
    except subprocess.TimeoutExpired:
        return "timeout"
    text = (result.stdout + result.stderr).strip()
    first = text.splitlines()[0] if text else ""
    return first[:160]


def make(command: list[str], log: Path) -> int:
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("w") as handle:
        result = subprocess.run(command, cwd=ROOT, stdout=handle, stderr=subprocess.STDOUT)
    return result.returncode


def test_one(
    app: str,
    version: str,
    cache: Path,
    extractor: Path,
    *,
    headless: bool,
    gui: bool,
    image_size: str,
) -> dict:
    print(f"\n=== {app} {version}")
    out = BUILD / app
    out.mkdir(parents=True, exist_ok=True)
    record: dict = {"app": app, "version": version}
    work = Path(tempfile.mkdtemp(prefix=f"craft-{app}-", dir=cache))
    try:
        image = fetch(app, version, cache)
        record["image"] = str(image)
        record["sha256"] = sha256(image)
        info = inspect(image, extractor, work)
        record.update(info)
        print(
            f"  id={info['identity']} gui={info['gui']} cli={info['cli']} "
            f"bytes={record['sha256'][:12]}"
        )
        root = work / "root"
        if info["gui"]:
            record["host_gui_version"] = host_version(root, info["gui"])
        if info["cli"]:
            record["host_cli_version"] = host_version(root, info["cli"])
        common = [
            f"CRAFT_ID={info['identity']}",
            f"CRAFT_VERSION={version}",
            f"CRAFT_APPIMAGE={image}",
            f"CRAFT_PIN={record['sha256']}",
        ]
        if headless:
            program = HEADLESS_BINARY.get(app) or info["cli"] or info["gui"]
            command = [
                "make",
                "craft-smoke-test",
                *common,
                f"CRAFT_PROGRAM=/usr/bin/{program}",
                "CRAFT_ARGS=--version",
                "CRAFT_IMAGE_SIZE=192M",
                f"CRAFT_OUT={out}",
            ]
            code = make(command, out / "headless.log")
            record["headless_exit"] = code
            record["headless"] = "pass" if code == 0 else "fail"
            print(f"  headless: {record['headless']} (log {out / 'headless.log'})")
        if gui:
            command = [
                "make",
                "craft-gui-test",
                *common,
                f"CRAFT_PROGRAM=/usr/bin/{info['gui']}",
                "CRAFT_ARGS=",
                f"CRAFT_IMAGE_SIZE={image_size}",
                f"CRAFT_OUT={out}",
            ]
            code = make(command, out / "gui.log")
            record["gui_exit"] = code
            record["gui"] = "pass" if code == 0 else "fail"
            print(f"  gui: {record['gui']} (log {out / 'gui.log'})")
        return record
    finally:
        shutil.rmtree(work, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apps", nargs="*", help="subset of app names (default: all)")
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    parser.add_argument("--no-headless", action="store_true")
    parser.add_argument("--no-gui", action="store_true")
    parser.add_argument("--image-size", default="320M")
    args = parser.parse_args()
    selected = args.apps or list(APPS)
    unknown = [app for app in selected if app not in APPS]
    if unknown:
        parser.error(f"unknown app(s): {', '.join(unknown)}; known: {', '.join(APPS)}")
    args.cache.mkdir(parents=True, exist_ok=True)
    BUILD.mkdir(parents=True, exist_ok=True)
    extractor = ensure_extractor()
    results = []
    for app in selected:
        try:
            results.append(
                test_one(
                    app,
                    APPS[app],
                    args.cache,
                    extractor,
                    headless=not args.no_headless,
                    gui=not args.no_gui,
                    image_size=args.image_size,
                )
            )
        except Exception as error:
            print(f"  ERROR: {error}", file=sys.stderr)
            results.append({"app": app, "version": APPS[app], "error": str(error)})
        (BUILD / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print("\n=== summary ===")
    for record in results:
        state = record.get("error") or (
            f"headless={record.get('headless', '-')} gui={record.get('gui', '-')}"
        )
        print(f"{record['app']:12} {record['version']:8} {state}")
    return 0 if all("error" not in record for record in results) else 1


if __name__ == "__main__":
    sys.exit(main())

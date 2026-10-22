#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Stage every published Craft app for the multi-app ISO.

The artifacts are the same pinned upstream releases that
``tools/test-craft-apps.py`` exercises. Each AppImage is verified against the
upstream ``SHA256SUMS.txt`` and signed into a ``linux-appimage`` import record
with the checked-in test key. The result is one directory per app:

    build/craft-apps/<app>/app.AppImage   (symlink to the verified image)
    build/craft-apps/<app>/app.import     (signed import record)
    build/craft-apps/<app>/metadata.txt

``build/craft-apps/manifest.txt`` lists every staged app for the ISO builder.
Existing signed records are reused only when their recorded digest still
matches the image, so the stage is cheap after the first run and never accepts
a stale artifact.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CACHE = Path("/tmp/opencode/craft-apps")
DEFAULT_OUT = ROOT / "build" / "craft-apps"
SEED = ROOT / "tests" / "fixtures" / "keys" / "test.seed"


def load_suite():
    path = ROOT / "tools" / "test-craft-apps.py"
    spec = importlib.util.spec_from_file_location("craft_suite", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_record(path: Path) -> dict[str, str]:
    if not path.exists():
        return {}
    data = path.read_bytes()
    return {
        key.decode(): value.decode()
        for key, value in re.findall(rb"(id|version|sha256)=([^\n]+)", data)
    }


def stage_one(
    suite,
    app: str,
    version: str,
    cache: Path,
    out: Path,
    extractor: Path | None,
) -> dict[str, str]:
    image = suite.fetch(app, version, cache)
    digest = sha256(image)
    directory = out / app
    directory.mkdir(parents=True, exist_ok=True)
    record = directory / "app.import"
    fields = parse_record(record)

    if (
        fields.get("sha256") == digest
        and fields.get("version") == version
        and fields.get("id")
    ):
        identity = fields["id"]
    else:
        if extractor is None:
            extractor = suite.ensure_extractor()
        work = cache / f".stage-{app}"
        try:
            info = suite.inspect(image, extractor, work)
        finally:
            shutil.rmtree(work, ignore_errors=True)
        identity = info["identity"]

    # Sign every run even when the record was reusable: the signing tool also
    # verifies the record against the image, so a stale or damaged record
    # cannot be carried into the ISO.
    result = subprocess.run(
        [
            "sh",
            "tools/build-craft-appimage.sh",
            str(image),
            str(record),
            str(SEED),
            identity,
            version,
            digest,
            "memory=jit",
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"{app}: signing failed\n{result.stdout}{result.stderr}".rstrip()
        )
    print(f"  signed record: {identity} {version}")

    link = directory / "app.AppImage"
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to(image.resolve())

    (directory / "metadata.txt").write_text(
        "\n".join(
            [
                f"name={app}",
                f"id={identity}",
                f"version={version}",
                f"sha256={digest}",
                f"image=/linux/apps/{app}/app.AppImage",
                f"record=/linux/apps/{app}/app.import",
                "",
            ]
        )
    )
    return {
        "name": app,
        "id": identity,
        "version": version,
        "sha256": digest,
        "image": str(image),
    }


def write_pack(out: Path, staged: list[dict[str, str]]) -> None:
    """Writes the boot-volume app pack and its plain-text index.

    The index is small enough for the loader's record bound. Images and their
    signed records are concatenated into one pack so the loader publishes a
    single physical extent; the kernel verifies each selected app's record and
    digest before importing it.
    """
    pack = out / "apps.pack"
    index = out / "apps.index"
    lines = ["AETHER_APPS 1"]
    with pack.open("wb") as handle:
        for entry in staged:
            image = Path(entry["image"])
            record = out / entry["name"] / "app.import"
            image_offset = handle.tell()
            with image.open("rb") as source:
                shutil.copyfileobj(source, handle, 1024 * 1024)
            image_length = handle.tell() - image_offset
            record_offset = handle.tell()
            with record.open("rb") as source:
                shutil.copyfileobj(source, handle)
            record_length = handle.tell() - record_offset
            pad = (-handle.tell()) % 8
            if pad:
                handle.write(b"\0" * pad)
            lines.append(
                " ".join(
                    [
                        entry["name"],
                        entry["id"],
                        entry["version"],
                        entry["sha256"],
                        str(image_offset),
                        str(image_length),
                        str(record_offset),
                        str(record_length),
                    ]
                )
            )
    index.write_text("\n".join(lines) + "\n")
    print(f"packed {len(staged)} Craft apps into {pack} ({pack.stat().st_size} bytes)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apps", nargs="*", help="subset of app names (default: all)")
    parser.add_argument("--cache", type=Path, default=DEFAULT_CACHE)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args()

    suite = load_suite()
    selected = args.apps or list(suite.APPS)
    unknown = [app for app in selected if app not in suite.APPS]
    if unknown:
        parser.error(
            f"unknown app(s): {', '.join(unknown)}; known: {', '.join(suite.APPS)}"
        )

    args.cache.mkdir(parents=True, exist_ok=True)
    args.out.mkdir(parents=True, exist_ok=True)
    extractor = None
    staged = []
    for app in selected:
        version = suite.APPS[app]
        print(f"=== {app} {version}")
        try:
            staged.append(stage_one(suite, app, version, args.cache, args.out, extractor))
        except Exception as error:  # noqa: BLE001 - report and keep going
            print(f"  ERROR: {error}", file=sys.stderr)
            return 1

    write_pack(args.out, staged)
    manifest = args.out / "manifest.txt"
    manifest.write_text(
        "".join(
            "\t".join(
                [
                    entry["name"],
                    entry["id"],
                    entry["version"],
                    entry["sha256"],
                ]
            )
            + "\n"
            for entry in staged
        )
    )
    print(f"\nstaged {len(staged)} Craft apps in {args.out}")
    print(manifest)
    return 0


if __name__ == "__main__":
    sys.exit(main())

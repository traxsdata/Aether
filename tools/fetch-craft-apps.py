#!/usr/bin/env python3
"""fetch-craft-apps.py - download prebuilt Craft app packages for the ISO.

    tools/fetch-craft-apps.py                 # every app in the release
    tools/fetch-craft-apps.py gridcraft       # just these
    make                                      # then picks up build/apps/*.app

The packages are built from github.com/storytold sources by the workflow in
github.com/traxsdata/Aether-CraftApps and published to its craft-apps-latest
release, together with a craft-apps.json manifest. Each package is checked
against both GitHub's SHA-256 digest for the asset and the manifest's
package_sha256 before it is written to build/apps/<name>.app; the kernel
verifies the package header, SHA-256 and ELF image again when it loads it.

CRAFT_REPO (default traxsdata/Aether-CraftApps) and CRAFT_TAG (default
craft-apps-latest) pick another release. Only the Python standard library is
used.
"""

import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request

REPO = os.environ.get("CRAFT_REPO", "traxsdata/Aether-CraftApps")
TAG = os.environ.get("CRAFT_TAG", "craft-apps-latest")
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "build", "apps")
MAX_PACKAGE = 128 * 1024 * 1024  # APP_MAX_BYTES in kernel/appstore.h


def get(url, accept="application/octet-stream", limit=MAX_PACKAGE):
    req = urllib.request.Request(url, headers={"Accept": accept, "User-Agent": "aether-fetch-craft-apps"})
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            data = resp.read(limit + 1)
    except urllib.error.HTTPError as e:
        if e.code == 404 and "/releases/tags/" in url:
            sys.exit(f"{REPO} has no {TAG} release yet; run its Build Craft apps workflow first")
        sys.exit(f"{url}: HTTP {e.code}")
    if len(data) > limit:
        sys.exit(f"{url}: larger than {limit} bytes")
    return data


def main():
    wanted = sys.argv[1:]
    for name in wanted:
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
            sys.exit(f"bad app name: {name}")
    release = json.loads(get(f"https://api.github.com/repos/{REPO}/releases/tags/{TAG}",
                             "application/vnd.github+json", 4 * 1024 * 1024))
    assets = {a["name"]: a for a in release.get("assets", [])}
    if "craft-apps.json" not in assets:
        sys.exit(f"{REPO}@{TAG} has no craft-apps.json manifest")
    manifest = json.loads(get(assets["craft-apps.json"]["browser_download_url"], limit=1024 * 1024))
    apps = {a["name"]: a for a in manifest.get("apps", [])}
    missing = [n for n in wanted if n not in apps]
    if missing:
        sys.exit(f"not in {REPO}@{TAG}: {' '.join(missing)} (has: {' '.join(sorted(apps))})")
    os.makedirs(OUT, exist_ok=True)
    for name in wanted or sorted(apps):
        app = apps[name]
        if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
            sys.exit(f"manifest has a bad app name: {name!r}")
        asset = assets.get(app.get("package_asset", ""))
        if asset is None:
            sys.exit(f"{name}: release has no package asset {app.get('package_asset')!r}")
        digest = asset.get("digest") or ""
        if not digest.startswith("sha256:"):
            sys.exit(f"{name}: GitHub publishes no SHA-256 digest for {asset['name']}; refusing it")
        data = get(asset["browser_download_url"])
        got = hashlib.sha256(data).hexdigest()
        if got != digest[len("sha256:"):] or got != app.get("package_sha256"):
            sys.exit(f"{name}: {asset['name']} doesn't match its SHA-256 (got {got})")
        path = os.path.join(OUT, f"{name}.app")
        with open(path + ".tmp", "wb") as f:
            f.write(data)
        os.replace(path + ".tmp", path)
        print(f"{name} {app.get('version', '?')}: {path} ({len(data)} bytes, sha256 {got})")
        # The app_header's icon_format (offset 272) is 0 when the package was
        # built without an icon; the dock then falls back to a coloured letter.
        if len(data) >= 276 and data[:8] == b"AETHAPP1" and data[272:276] == b"\0\0\0\0":
            print(f"warning: {name} has no icon (built from an Aether commit before 0.7 icons?); "
                  f"re-run {REPO}'s Build Craft apps workflow", file=sys.stderr)


if __name__ == "__main__":
    main()

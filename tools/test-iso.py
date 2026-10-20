#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Boot and interact with the distribution ISO on two UEFI graphics adapters.

The FAT-image boot test proves the kernel; this test proves the *distribution
artifact*. It exercises the same El Torito path VMware Workstation uses when an
ISO is attached to a UEFI virtual machine.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

from boot_support import find_iso
from desktop_smoke import exercise_iso

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
TIMEOUT = int(os.environ.get("AETHER_ISO_TIMEOUT", "120"))


def main() -> int:
    try:
        iso = find_iso(BUILD, os.environ.get("AETHER_ISO"))
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    out = BUILD / "iso-test"
    out.mkdir(parents=True, exist_ok=True)
    with iso.open("rb") as image:
        digest = hashlib.file_digest(image, "sha256").hexdigest()

    def git_output(*arguments):
        try:
            return subprocess.check_output(
                ["git", *arguments], cwd=ROOT, text=True, stderr=subprocess.DEVNULL
            ).strip()
        except (OSError, subprocess.CalledProcessError):
            return None

    status = git_output("status", "--porcelain")
    report = {
        "iso": str(iso),
        "sha256": digest,
        "commit": git_output("rev-parse", "HEAD"),
        "worktree_dirty": bool(status) if status is not None else None,
        "adapters": ["VGA", "vmware-svga"],
        "status": "running",
    }
    record = out / "artifact.json"
    record.write_text(json.dumps(report, indent=2) + "\n")
    try:
        # Both adapters use the *same* ISO. This is not a Workstation claim.
        for device in ("VGA", "vmware-svga"):
            exercise_iso(iso, device, BUILD / "iso-test" / device, TIMEOUT)
        print(f"PASS: ISO {iso.name} boots and operates the desktop through UEFI El Torito")
        report["status"] = "PASS"
        return 0
    except (RuntimeError, OSError, ValueError) as error:
        report["status"] = "FAIL"
        report["error"] = str(error)
        print(f"FAIL: {error}; see build/iso-test/<adapter>/", file=sys.stderr)
        return 1
    finally:
        record.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check workspace package metadata and the dependency license inventory.

Aether is currently dependency-free outside the workspace. If an external
crate is added, this check reports it and requires explicit license metadata;
it does not silently accept an unknown dependency.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXPECTED_WORKSPACE_LICENSE = "Apache-2.0"


def main() -> int:
    try:
        output = subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=ROOT,
            text=True,
            stderr=subprocess.STDOUT,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"error: cargo metadata failed: {error}", file=sys.stderr)
        return 2

    metadata = json.loads(output)
    workspace_members = set(metadata["workspace_members"])
    problems = []
    external = []

    for package in metadata["packages"]:
        package_id = package["id"]
        name = package["name"]
        if package_id in workspace_members:
            license_id = package.get("license")
            if license_id != EXPECTED_WORKSPACE_LICENSE:
                problems.append(
                    f"{name}: workspace license is {license_id!r}, "
                    f"expected {EXPECTED_WORKSPACE_LICENSE!r}"
                )
            if not package.get("description"):
                problems.append(f"{name}: missing package description")
            if not package.get("repository"):
                problems.append(f"{name}: missing repository URL")
        else:
            external.append(name)
            if not package.get("license") and not package.get("license_file"):
                problems.append(f"{name}: external dependency has no license metadata")

    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        print(f"FAIL: {len(problems)} package metadata problem(s)", file=sys.stderr)
        return 1

    print(
        f"PASS: {len(workspace_members)} workspace package(s) under "
        f"{EXPECTED_WORKSPACE_LICENSE}; external dependencies: "
        f"{', '.join(sorted(external)) if external else 'none'}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

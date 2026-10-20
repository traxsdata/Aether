#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Run the full release qualification gate and write a report.

Qualification is stricter than the pull-request gate: it runs the same checks
plus the compatibility suite, package/update policy tests, and a signed release
bundle build and verification using the checked-in test key. The report records
the exact commit, toolchain, firmware, step results, required boot markers, and
artifact digests so a release claim is reproducible.

A failure stops the run and is reported; the report always captures what ran.
"""

from __future__ import annotations

import datetime
import hashlib
import os
import platform
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
OUT = BUILD / "qualification"
LOGS = OUT / "logs"
BOOT_LOG = BUILD / "serial.log"

STEPS = [
    ("format-and-lint", ["make", "lint"]),
    ("documentation-links", ["make", "docs-check"]),
    ("licence-metadata", ["make", "license-check"]),
    ("host-unit-tests", ["make", "unittest"]),
    ("compatibility-suite", ["make", "compat-test"]),
    ("package-update-policy", ["make", "update-test"]),
    ("boot", ["make", "test"]),
    ("iso-boot", ["make", "iso-test"]),
    ("vmware-definition", ["make", "vmware-vmx"]),
    ("shell", ["make", "shell-test"]),
    ("fault-path", ["make", "fault-test"]),
    ("rollback-path", ["make", "rollback-test"]),
    ("negative-boot", ["make", "negative-test"]),
    ("signed-release-demo", ["make", "release-demo"]),
]


@dataclass
class Result:
    name: str
    command: list[str]
    status: str = "not run"
    seconds: float = 0.0
    detail: str = ""
    log: Path | None = field(default=None)


def command_output(command: list[str]) -> str:
    try:
        return subprocess.check_output(
            command, cwd=ROOT, text=True, stderr=subprocess.STDOUT
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(65536), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def run_step(name: str, command: list[str]) -> Result:
    result = Result(name=name, command=command)
    log = LOGS / f"{name}.log"
    result.log = log
    started = time.monotonic()
    print(f"== {name}: {' '.join(command)}", flush=True)
    with log.open("w") as output:
        try:
            process = subprocess.run(
                command,
                cwd=ROOT,
                stdout=output,
                stderr=subprocess.STDOUT,
                text=True,
            )
        except OSError as error:
            result.status = "FAIL"
            result.detail = f"could not run: {error}"
            return result
    result.seconds = time.monotonic() - started
    result.status = "PASS" if process.returncode == 0 else "FAIL"
    if process.returncode != 0:
        tail = log.read_text(errors="replace").strip().splitlines()[-5:]
        result.detail = " | ".join(tail)
    return result


def boot_markers(text: str) -> list[str]:
    return [
        line
        for line in text.splitlines()
        if line.startswith("AETHER_") and "FAILED" not in line
    ]


def write_report(results: list[Result], started: str) -> Path:
    commit = command_output(["git", "rev-parse", "HEAD"])
    describe = command_output(["git", "describe", "--always", "--dirty"])
    rustc = command_output(["rustc", "--version"])
    qemu = command_output(["qemu-system-x86_64", "--version"])
    qemu = qemu.splitlines()[0] if qemu != "unknown" else qemu
    kernel = BUILD / "aether-kernel.elf"
    loader = BUILD / "BOOTX64.EFI"

    lines = [
        "# Aether qualification report",
        "",
        f"- Started: {started}",
        f"- Finished: {datetime.datetime.now(datetime.UTC).strftime('%Y-%m-%dT%H:%M:%SZ')}",
        f"- Commit: `{commit}` (`{describe}`)",
        f"- Toolchain: {rustc}",
        f"- Host: {platform.platform()}",
        f"- Emulator: {qemu}",
    ]
    if kernel.exists():
        lines.append(f"- Kernel ELF SHA-256: `{digest(kernel)}`")
    if loader.exists():
        lines.append(f"- Loader EFI SHA-256: `{digest(loader)}`")

    release_dir = BUILD / "release-demo"
    sums = release_dir / "SHA256SUMS"
    info = release_dir / "release-info.txt"
    if info.exists():
        fields = dict(
            line.split("=", 1) for line in info.read_text().splitlines() if "=" in line
        )
        lines.append(
            "- Release bundle: version {version}, counter {counter}, channel "
            "{channel}, image SHA-256 `{image_sha}`".format(
                version=fields.get("version", "?"),
                counter=fields.get("counter", "?"),
                channel=fields.get("channel", "?"),
                image_sha=fields.get("image.sha256", "?"),
            )
        )

    lines += ["", "## Steps", "", "| Step | Result | Duration | Command |", "|---|---|---:|---|"]
    for result in results:
        command = " ".join(result.command)
        detail = f" — {result.detail}" if result.detail else ""
        lines.append(
            f"| {result.name} | {result.status}{detail} | {result.seconds:.1f}s | `{command}` |"
        )

    lines += ["", "## Boot markers", ""]
    marker_source = LOGS / "boot.log"
    if marker_source.exists():
        markers = boot_markers(marker_source.read_text(errors="replace"))
        if markers:
            lines.append("```")
            lines += markers
            lines.append("```")
        else:
            lines.append("No markers captured from the boot step.")
    else:
        lines.append("The boot step did not run.")

    lines += [
        "",
        "## Signed release bundle",
        "",
        "```",
        sums.read_text().strip() if sums.exists() else "No release bundle was produced.",
        "```",
        "",
        "## Not covered",
        "",
        "- External security review; this gate is automated testing, not assurance.",
        "- Physical hardware, Secure Boot, TPM rollback counters, and IPv6/network paths.",
        "- Manual desktop/accessibility review beyond the scripted interaction paths.",
        "- VMware Workstation itself; QEMU SVGA/OVMF is emulator evidence only.",
        "- Windows or other compatibility runtimes; see the compatibility database.",
        "",
        f"Logs are in `{LOGS.relative_to(ROOT)}/`.",
        "",
    ]

    report = OUT / "report.md"
    report.write_text("\n".join(lines))
    return report


def main() -> int:
    if os.environ.get("AETHER_QUALIFY_QUICK") == "1":
        print("error: quick qualification is not supported; a release needs every step", file=sys.stderr)
        return 2

    if shutil.which("cargo") is None:
        print("error: cargo is not on PATH; source the Rust environment first", file=sys.stderr)
        return 2

    OUT.mkdir(parents=True, exist_ok=True)
    LOGS.mkdir(parents=True, exist_ok=True)
    # The serial log belongs to the boot step; remove a stale capture so the
    # report cannot quote an earlier run.
    BOOT_LOG.unlink(missing_ok=True)

    started = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    results: list[Result] = []
    failed = False
    for name, command in STEPS:
        result = run_step(name, command)
        results.append(result)
        print(f"   {result.status} ({result.seconds:.1f}s)", flush=True)
        if result.status != "PASS":
            failed = True
            # Later steps depend on earlier ones; stop before wasting a full
            # QEMU run on a broken tree, but still record the failure.
            break

    report = write_report(results, started)
    print(f"\nqualification report: {report.relative_to(ROOT)}")
    if failed:
        print("QUALIFICATION FAILED", file=sys.stderr)
        return 1
    print("QUALIFICATION PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())

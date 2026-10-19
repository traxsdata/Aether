#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Boot the Aether image under QEMU/OVMF and verify serial markers.

This is the automated acceptance test for the first sprint: the image must
reach the kernel and print AETHER_BOOT_OK. Loader failures are reported with
the full serial log.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
IMAGE = BUILD / "aether.img"
SERIAL = BUILD / "serial.log"
VARS = BUILD / "OVMF_VARS.fd"

SUCCESS_MARKERS = ("AETHER_MM_OK", "AETHER_BOOT_OK")
FAULT_MARKERS = ("KERNEL EXCEPTION: breakpoint (3)", "AETHER_BOOT_FAILED")
FAILURES = ("AETHER_LOADER_FAILED", "AETHER_LOADER_PANIC", "AETHER_KERNEL_PANIC", "AETHER_BOOT_FAILED")
EXPECT_FAULT = os.environ.get("AETHER_EXPECT_FAULT") == "1"
TIMEOUT = int(os.environ.get("AETHER_BOOT_TIMEOUT", "90"))

CODE_CANDIDATES = (
    Path("/usr/share/OVMF/OVMF_CODE_4M.fd"),
    Path("/usr/share/OVMF/OVMF_CODE.fd"),
    Path("/usr/share/edk2/ovmf/OVMF_CODE.fd"),
    Path("/usr/share/edk2/ovmf/OVMF_CODE_4M.fd"),
    Path("/usr/share/qemu/OVMF.fd"),
)
VARS_CANDIDATES = (
    Path("/usr/share/OVMF/OVMF_VARS_4M.fd"),
    Path("/usr/share/OVMF/OVMF_VARS.fd"),
    Path("/usr/share/edk2/ovmf/OVMF_VARS.fd"),
    Path("/usr/share/edk2/ovmf/OVMF_VARS_4M.fd"),
)


def find_firmware() -> tuple[Path, Path | None]:
    code_env = os.environ.get("OVMF_CODE")
    vars_env = os.environ.get("OVMF_VARS")
    if code_env:
        code = Path(code_env)
        variables = Path(vars_env) if vars_env else None
        return code, variables

    code = next((path for path in CODE_CANDIDATES if path.exists()), None)
    if code is None:
        raise SystemExit("error: OVMF firmware not found; set OVMF_CODE and OVMF_VARS")
    variables = next((path for path in VARS_CANDIDATES if path.exists()), None)
    return code, variables


def qemu_command(code: Path, variables: Path | None) -> list[str]:
    command = [
        "qemu-system-x86_64",
        "-machine",
        "q35,accel=tcg",
        "-cpu",
        "max",
        "-m",
        "1024M",
        "-display",
        "none",
        "-monitor",
        "none",
        "-no-reboot",
        "-serial",
        f"file:{SERIAL}",
        "-drive",
        f"file={IMAGE},format=raw,if=virtio",
    ]
    if variables is None:
        command += ["-bios", str(code)]
    else:
        shutil.copyfile(variables, VARS)
        command += [
            "-drive",
            f"if=pflash,format=raw,readonly=on,file={code}",
            "-drive",
            f"if=pflash,format=raw,file={VARS}",
        ]
    return command


def read_serial() -> str:
    if not SERIAL.exists():
        return ""
    return SERIAL.read_text(errors="replace")


def main() -> int:
    if not IMAGE.exists():
        print("error: build/aether.img missing; run `make image`", file=sys.stderr)
        return 2

    code, variables = find_firmware()
    BUILD.mkdir(exist_ok=True)
    SERIAL.unlink(missing_ok=True)

    process = subprocess.Popen(
        qemu_command(code, variables),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    deadline = time.monotonic() + TIMEOUT
    try:
        while time.monotonic() < deadline:
            if process.poll() is not None:
                break
            log = read_serial()
            if EXPECT_FAULT:
                if all(marker in log for marker in FAULT_MARKERS):
                    print(log)
                    print("PASS: fault path reported breakpoint and halted")
                    return 0
                if any(marker in log for marker in FAILURES[:3]):
                    print(log)
                    print("FAIL: fault self-test hit an unexpected failure", file=sys.stderr)
                    return 1
            else:
                if all(marker in log for marker in SUCCESS_MARKERS):
                    print(log)
                    print(f"PASS: {' '.join(SUCCESS_MARKERS)}")
                    return 0
                if any(marker in log for marker in FAILURES):
                    print(log)
                    print("FAIL: boot reported an error", file=sys.stderr)
                    return 1
            time.sleep(0.25)
        print(read_serial())
        expected = FAULT_MARKERS if EXPECT_FAULT else SUCCESS_MARKERS
        print(f"FAIL: timed out after {TIMEOUT}s waiting for {expected}", file=sys.stderr)
        return 1
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    sys.exit(main())

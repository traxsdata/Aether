#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Boot the Aether image under QEMU/OVMF and verify serial markers.

This is the automated acceptance test for the first sprint: the image must
reach the kernel and print AETHER_BOOT_OK. Loader failures are reported with
the full serial log.
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from boot_support import BASE_MARKERS, find_firmware, has_failure, socket_path
from qemu_accel import accel_args, describe

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
IMAGE = BUILD / "aether.img"
SERIAL = BUILD / "serial.log"
VARS = BUILD / "OVMF_VARS.fd"

# Feature-specific self-tests append their own markers through the environment
# (comma separated, for example AETHER_EXTRA_MARKERS=AETHER_RECOVERY_ROLLBACK_OK)
# so `make rollback-test` can require the evidence its build produces.
EXTRA_MARKERS = tuple(
    marker.strip()
    for marker in os.environ.get("AETHER_EXTRA_MARKERS", "").split(",")
    if marker.strip()
)
SKIP_MARKERS = {
    marker.strip()
    for marker in os.environ.get("AETHER_SKIP_MARKERS", "").split(",")
    if marker.strip()
}
SUCCESS_MARKERS = tuple(
    marker for marker in BASE_MARKERS + EXTRA_MARKERS if marker not in SKIP_MARKERS
)
# Additional literal serial text a successful boot must contain. The `|`
# separator allows substrings that themselves contain commas (for example the
# Gate 3 CSV line `total,3`). This checks output content, not only markers.
REQUIRE_TEXT = tuple(
    text.strip()
    for text in os.environ.get("AETHER_REQUIRE_TEXT", "").split("|")
    if text.strip()
)
FAULT_MARKERS = ("KERNEL EXCEPTION: breakpoint (3)", "AETHER_BOOT_FAILED")
FAILURES = (
    "AETHER_LOADER_FAILED",
    "AETHER_LOADER_PANIC",
    "AETHER_KERNEL_PANIC",
    "AETHER_BOOT_FAILED",
    "AETHER_WM_FAILED",
    "AETHER_POINTER_FAILED",
)
EXPECT_FAULT = os.environ.get("AETHER_EXPECT_FAULT") == "1"
EXPECT_BOOT_FAILURE = os.environ.get("AETHER_EXPECT_BOOT_FAILURE") == "1"
# Optional substring that a deliberately failing boot must contain. This proves
# the intended fail-closed branch ran instead of some unrelated failure.
EXPECT_FAILURE_TEXT = os.environ.get("AETHER_EXPECT_FAILURE_TEXT", "")
# Require the full-screen crash report after the expected failure and check
# QEMU's displayed framebuffer: mostly report blue, with text drawn on it.
EXPECT_CRASH_SCREEN = os.environ.get("AETHER_EXPECT_CRASH_SCREEN") == "1"
CRASH_MARKER = "AETHER_CRASH_SCREEN shown=1"
CRASH_BLUE = (0x0F, 0x3C, 0x8C)
# Unix socket paths are capped at 108 bytes (SUN_PATH_MAX); boot_support
# returns a short path so the harness works from any checkout depth.
QMP = socket_path("qmp.sock")
SCREENSHOT = BUILD / "crash-screen.ppm"
TIMEOUT = int(os.environ.get("AETHER_BOOT_TIMEOUT", "90"))
# VNC display number for the headless q35 boot. Parallel worktrees can collide
# on the default display, so callers may select another one explicitly.
VNC_DISPLAY = os.environ.get("AETHER_VNC_DISPLAY", "9")
# Optional NIC model (for example e1000 or e1000e) on a QEMU user-mode
# network. Unset keeps the machine's default NIC.
NIC_MODEL = os.environ.get("AETHER_NIC_MODEL", "")

def qemu_command(code: Path, variables: Path | None) -> list[str]:
    command = [
        "qemu-system-x86_64",
        *accel_args(),
        "-m",
        os.environ.get("AETHER_MEMORY", "1024M"),
        "-display",
        f"vnc=127.0.0.1:{VNC_DISPLAY}",
        "-device",
        "VGA",
        "-monitor",
        "none",
        "-no-reboot",
        "-serial",
        f"file:{SERIAL}",
        "-drive",
        f"file={IMAGE},format=raw,if=virtio",
    ]
    if NIC_MODEL:
        command += ["-nic", f"user,model={NIC_MODEL}"]
    if EXPECT_CRASH_SCREEN:
        command += ["-qmp", f"unix:{QMP},server=on,wait=off"]
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


def screendump(path: Path) -> tuple[int, int, bytes]:
    """Saves what QEMU's display shows and returns its RGB pixels."""
    path.unlink(missing_ok=True)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(10)
        client.connect(str(QMP))
        stream = client.makefile("rwb", buffering=0)
        json.loads(stream.readline())
        for name, arguments in (
            ("qmp_capabilities", {}),
            ("screendump", {"filename": str(path)}),
        ):
            stream.write(json.dumps({"execute": name, "arguments": arguments}).encode() + b"\n")
            while True:
                reply = json.loads(stream.readline())
                if "error" in reply:
                    raise RuntimeError(f"QMP {name}: {reply['error']}")
                if "return" in reply:
                    break
    # QEMU writes a binary P6 PPM with a fixed three-line header.
    magic, dimensions, maximum, pixels = path.read_bytes().split(b"\n", 3)
    width, height = map(int, dimensions.split())
    if magic != b"P6" or maximum != b"255" or len(pixels) != width * height * 3:
        raise RuntimeError(f"invalid QEMU screenshot: {path}")
    return width, height, pixels


def check_crash_screen() -> str | None:
    """Returns an error, or None when the display shows the crash report."""
    width, height, pixels = screendump(SCREENSHOT)
    blue = white = 0
    for offset in range(0, len(pixels), 3):
        pixel = tuple(pixels[offset : offset + 3])
        if pixel == CRASH_BLUE:
            blue += 1
        elif pixel == (0xFF, 0xFF, 0xFF):
            white += 1
    total = width * height
    print(f"crash screen {width}x{height}: blue={blue / total:.1%} text={white / total:.2%}")
    if blue < total * 0.6:
        return "the display is not the blue crash report"
    if white < total * 0.002:
        return "the crash report has no text"
    return None


def main() -> int:
    if not IMAGE.exists():
        print("error: build/aether.img missing; run `make image`", file=sys.stderr)
        return 2

    print(f"qemu: {describe()}")
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
            elif EXPECT_BOOT_FAILURE:
                if (
                    has_failure(log)
                    and (not EXPECT_FAILURE_TEXT or EXPECT_FAILURE_TEXT in log)
                    and (not EXPECT_CRASH_SCREEN or CRASH_MARKER in log)
                ):
                    print(log)
                    if EXPECT_CRASH_SCREEN:
                        error = check_crash_screen()
                        if error:
                            print(f"FAIL: {error} (see {SCREENSHOT})", file=sys.stderr)
                            return 1
                        print(f"PASS: crash report on screen ({SCREENSHOT})")
                    print("PASS: harness detected the expected boot failure")
                    return 0
                if all(marker in log for marker in SUCCESS_MARKERS):
                    print(log)
                    print("FAIL: expected a boot failure, but the boot succeeded", file=sys.stderr)
                    return 1
            else:
                # Check failure markers first: a kernel that prints the
                # success markers and then fails must not be accepted.
                if has_failure(log):
                    print(log)
                    print("FAIL: boot reported an error", file=sys.stderr)
                    return 1
                if all(marker in log for marker in SUCCESS_MARKERS) and all(
                    text in log for text in REQUIRE_TEXT
                ):
                    print(log)
                    print(f"PASS: {' '.join(SUCCESS_MARKERS)}")
                    if REQUIRE_TEXT:
                        print(f"PASS: required text {REQUIRE_TEXT!r}")
                    return 0
            time.sleep(0.25)
        log = read_serial()
        print(log)
        if not EXPECT_FAULT and not EXPECT_BOOT_FAILURE:
            missing = [marker for marker in SUCCESS_MARKERS if marker not in log]
            if missing:
                print(f"missing markers: {missing}", file=sys.stderr)
            missing = [text for text in REQUIRE_TEXT if text not in log]
            if missing:
                print(f"missing required text: {missing}", file=sys.stderr)
        if EXPECT_FAULT:
            expected = FAULT_MARKERS
        elif EXPECT_BOOT_FAILURE:
            expected = "a boot failure marker"
        else:
            expected = (SUCCESS_MARKERS, REQUIRE_TEXT)
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

#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Gate 4 interactive smoke test for the windowed Linux application.

This harness boots the raw Aether image (not the release ISO), waits for the
kernel-owned Gate 4 markers, takes a QEMU screenshot, and (optionally) drives
real PS/2 input through QMP. It deliberately does not accept guest output as
evidence: every required marker is emitted by the kernel after validating the
operation.

Environment:
  AETHER_GATE4_TIMEOUT   boot/run timeout in seconds (default 300)
  AETHER_GATE4_SCREENSHOT path for the PPM screenshot (default build/gate4.ppm)
  AETHER_GATE4_CLICK     "x,y" pointer click before typing (optional)
  AETHER_GATE4_KEYS      comma-separated QMP sendkey names (optional)
  AETHER_GATE4_WAIT_SAVE wait for SAVE/REVOKE markers (default 1)
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

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
# The image and the run directory can be redirected so that two harnesses (or a
# harness and a concurrent build) do not share the same QEMU image lock or
# serial log. The defaults are unchanged.
IMAGE = Path(os.environ.get("AETHER_GATE4_IMAGE", BUILD / "aether.img"))
OUT = Path(os.environ.get("AETHER_GATE4_OUT", BUILD / "gate4"))
TIMEOUT = int(os.environ.get("AETHER_GATE4_TIMEOUT", "300"))
SCREENSHOT = Path(os.environ.get("AETHER_GATE4_SCREENSHOT", OUT / "gate4.ppm"))
CLICK = os.environ.get("AETHER_GATE4_CLICK", "")
KEYS = [key for key in os.environ.get("AETHER_GATE4_KEYS", "").split(",") if key]
WAIT_SAVE = os.environ.get("AETHER_GATE4_WAIT_SAVE", "1") != "0"

REQUIRED = ["AETHER_GATE4_PRESENT_OK"]
if CLICK or KEYS:
    REQUIRED.append("AETHER_GATE4_INPUT_OK")
if WAIT_SAVE:
    REQUIRED += ["AETHER_GATE4_SAVE_OK", "AETHER_GATE4_REVOKE_OK"]
FAILURES = ("AETHER_BOOT_FAILED", "AETHER_KERNEL_PANIC", "AETHER_LOADER_FAILED")


def firmware() -> tuple[Path, Path | None]:
    candidates = [
        (Path("/usr/share/OVMF/OVMF_CODE_4M.fd"), Path("/usr/share/OVMF/OVMF_VARS_4M.fd")),
        (Path("/usr/share/OVMF/OVMF_CODE.fd"), Path("/usr/share/OVMF/OVMF_VARS.fd")),
        (Path("/usr/share/ovmf/OVMF.fd"), None),
    ]
    for code, variables in candidates:
        if code.exists() and (variables is None or variables.exists()):
            return code, variables
    raise RuntimeError("OVMF firmware not found")


def connect(path: Path, process: subprocess.Popen, timeout: float = 20) -> socket.socket:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            client.connect(str(path))
            client.settimeout(0.1)
            return client
        except OSError:
            client.close()
            if process.poll() is not None:
                break
            time.sleep(0.05)
    raise RuntimeError(f"QEMU socket unavailable: {path}")


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    magic, dimensions, maximum, pixels = path.read_bytes().split(b"\n", 3)
    width, height = map(int, dimensions.split())
    if magic != b"P6" or maximum != b"255" or len(pixels) != width * height * 3:
        raise RuntimeError(f"invalid QEMU screenshot: {path}")
    return width, height, pixels


def main() -> int:
    if not IMAGE.exists():
        print(f"error: {IMAGE} missing; run `make image` first", file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    serial = OUT / "serial.log"
    serial.unlink(missing_ok=True)
    SCREENSHOT.unlink(missing_ok=True)
    qmp_path = OUT / "qmp.sock"
    qmp_path.unlink(missing_ok=True)
    code, variables = firmware()
    variables_copy = OUT / "OVMF_VARS.fd"
    if variables:
        shutil.copyfile(variables, variables_copy)

    command = [
        "qemu-system-x86_64",
        "-machine", "q35,accel=tcg",
        "-cpu", "max",
        "-m", "4G",
        "-display", "none",
        "-device", "VGA",
        "-no-reboot",
        "-serial", f"file:{serial}",
        "-drive", f"file={IMAGE},format=raw,if=virtio",
        "-qmp", f"unix:{qmp_path},server=on,wait=off",
    ]
    if variables:
        command += [
            "-drive", f"if=pflash,format=raw,readonly=on,file={code}",
            "-drive", f"if=pflash,format=raw,file={variables_copy}",
        ]
    else:
        command += ["-bios", str(code)]
    qemu_err = (OUT / "qemu.err").open("w")
    process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=qemu_err)
    qmp_socket = None
    qmp_file = None
    try:
        qmp_socket = connect(qmp_path, process)
        qmp_socket.settimeout(20)
        qmp_file = qmp_socket.makefile("rwb", buffering=0)
        json.loads(qmp_file.readline())
        send_qmp(qmp_file, "qmp_capabilities")

        deadline = time.monotonic() + TIMEOUT
        while time.monotonic() < deadline:
            text = serial.read_text(errors="replace") if serial.exists() else ""
            if any(marker in text for marker in FAILURES):
                print(text)
                print("FAIL: boot reported a failure", file=sys.stderr)
                return 1
            if all(marker in text for marker in REQUIRED):
                break
            if process.poll() is not None:
                print(text)
                print("FAIL: QEMU exited before the Gate 4 markers", file=sys.stderr)
                return 1
            time.sleep(0.25)
        else:
            text = serial.read_text(errors="replace") if serial.exists() else ""
            print(text)
            missing = [marker for marker in REQUIRED if marker not in text]
            print(f"FAIL: timed out after {TIMEOUT}s; missing {missing}", file=sys.stderr)
            return 1

        text = serial.read_text(errors="replace")
        if "AETHER_GATE4_PRESENT_OK" in text:
            # Let the compositor finish the frame before the dump.
            time.sleep(1.0)
            send_qmp(qmp_file, "screendump", {"filename": str(SCREENSHOT)})
            width, height, pixels = read_ppm(SCREENSHOT)
            nonblack = sum(
                1
                for index in range(0, len(pixels), 3)
                if pixels[index] or pixels[index + 1] or pixels[index + 2]
            )
            if nonblack < 1000:
                print(f"FAIL: screenshot is blank ({nonblack} non-black pixels)", file=sys.stderr)
                return 1
            print(f"PASS: screenshot {width}x{height} nonblack={nonblack} path={SCREENSHOT}")

        if CLICK:
            # The PS/2 driver starts the pointer at the screen centre. Move up
            # and left in bounded relative packets, then click twice: the first
            # click focuses the Linux window, the second selects a cell.
            for _ in range(10):
                send_hmp(qmp_file, "mouse_move -30 30")
                time.sleep(0.02)
            send_hmp(qmp_file, "mouse_button 1")
            send_hmp(qmp_file, "mouse_button 0")
            time.sleep(0.05)
            send_hmp(qmp_file, "mouse_button 1")
            send_hmp(qmp_file, "mouse_button 0")
        for key in KEYS:
            send_hmp(qmp_file, f"sendkey {key} 20")
            time.sleep(0.08)

        if WAIT_SAVE:
            # Save first; the app stays open until the harness closes it. The
            # revoke marker is emitted on the post-exit path.
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                text = serial.read_text(errors="replace")
                if "AETHER_GATE4_SAVE_OK" in text:
                    break
                time.sleep(0.25)
            else:
                print(text)
                print("FAIL: save marker did not appear", file=sys.stderr)
                return 1
            send_hmp(qmp_file, "sendkey alt-f4 20")
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                text = serial.read_text(errors="replace")
                if "AETHER_GATE4_REVOKE_OK" in text:
                    print("PASS: Gate 4 save and revocation markers present")
                    break
                time.sleep(0.25)
            else:
                print(text)
                print("FAIL: revoke marker did not appear", file=sys.stderr)
                return 1

        print(text)
        print("PASS: Gate 4 markers", " ".join(REQUIRED))
        return 0
    except Exception as error:  # pragma: no cover - diagnostic path
        text = serial.read_text(errors="replace") if serial.exists() else ""
        print(text)
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    finally:
        if qmp_file is not None:
            qmp_file.close()
        if qmp_socket is not None:
            qmp_socket.close()
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        qemu_err.close()
        qmp_path.unlink(missing_ok=True)


def send_qmp(qmp_file, name: str, arguments: dict | None = None):
    qmp_file.write(json.dumps({"execute": name, "arguments": arguments or {}}).encode() + b"\n")
    while True:
        line = qmp_file.readline()
        if not line:
            raise RuntimeError("QMP disconnected")
        reply = json.loads(line)
        if "error" in reply:
            raise RuntimeError(f"QMP error: {reply['error']}")
        if "return" in reply:
            return reply["return"]


def send_hmp(qmp_file, command: str):
    reply = send_qmp(qmp_file, "human-monitor-command", {"command-line": command})
    if reply.strip():
        raise RuntimeError(f"monitor command {command!r}: {reply}")




if __name__ == "__main__":
    sys.exit(main())

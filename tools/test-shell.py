#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Drive the Aether console shell over a QEMU serial socket.

The boot test proves that the kernel reaches its markers. This test proves that
the console accepts input, that filesystem commands observe the seeded volume,
and that the service and application views are reachable without a user task.
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
IMAGE = BUILD / "aether.img"
SOCKET = BUILD / "shell.sock"
VARS = BUILD / "OVMF_VARS.fd"

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
        "vnc=127.0.0.1:9",
        "-device",
        "VGA",
        "-monitor",
        "none",
        "-no-reboot",
        "-serial",
        f"unix:{SOCKET},server=on,wait=off",
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


def connect(timeout: float) -> socket.socket:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            client.connect(str(SOCKET))
            client.settimeout(0.5)
            return client
        except OSError:
            client.close()
            time.sleep(0.1)
    raise SystemExit("error: could not connect to the QEMU serial socket")


def read_until(client: socket.socket, marker: bytes, timeout: float) -> str:
    deadline = time.monotonic() + timeout
    data = bytearray()
    while time.monotonic() < deadline:
        try:
            chunk = client.recv(4096)
        except socket.timeout:
            continue
        if not chunk:
            break
        data.extend(chunk)
        if marker in data:
            return data.decode(errors="replace")
    return data.decode(errors="replace")


def send_command(client: socket.socket, command: str, timeout: float = 5.0) -> str:
    client.sendall(command.encode() + b"\n")
    return read_until(client, b"aether> ", timeout)


def main() -> int:
    if not IMAGE.exists():
        print("error: build/aether.img missing; run `make image`", file=sys.stderr)
        return 2

    code, variables = find_firmware()
    BUILD.mkdir(exist_ok=True)
    SOCKET.unlink(missing_ok=True)
    process = subprocess.Popen(
        qemu_command(code, variables),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    transcript = ""
    try:
        client = connect(20.0)
        transcript += read_until(client, b"AETHER_BOOT_OK", 90.0)
        if "AETHER_BOOT_OK" not in transcript:
            print(transcript)
            print("FAIL: boot markers not reached before shell test", file=sys.stderr)
            return 1
        transcript += read_until(client, b"aether> ", 10.0)

        failures = []
        for command, expected in (
            ("help", "commands:"),
            ("ls /", "system\td"),
            ("cat /system/policy.txt", "aether policy v1"),
            ("apps", "app demo"),
            ("security", "security: open_files="),
            ("services", "service storaged"),
            ("recovery", "IMAGES: CURRENT + PREVIOUS"),
            ("desktop", "desktop: focus="),
            ("input", "input: dropped="),
            ("uptime", "uptime:"),
        ):
            output = send_command(client, command)
            transcript += output
            if expected not in output:
                failures.append((command, expected, output))

        print(transcript)
        if failures:
            for command, expected, output in failures:
                print(
                    f"FAIL: command {command!r} did not contain {expected!r}\n{output}",
                    file=sys.stderr,
                )
            return 1
        print("PASS: shell input, filesystem, service, and application views")
        return 0
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        SOCKET.unlink(missing_ok=True)


if __name__ == "__main__":
    sys.exit(main())

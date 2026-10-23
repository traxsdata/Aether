#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Drive the Aether console shell over a QEMU serial socket.

The boot test proves that the kernel reaches its markers. This test proves that
the console accepts input, that filesystem commands observe the seeded volume,
and that the service and application views are reachable without a user task.
"""

from __future__ import annotations

import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from boot_support import find_firmware, has_failure
from qemu_accel import accel_args, describe

ROOT = Path(__file__).resolve().parent.parent
BUILD = ROOT / "build"
IMAGE = BUILD / "aether.img"
SOCKET = BUILD / "shell.sock"
VARS = BUILD / "OVMF_VARS.fd"

def qemu_command(code: Path, variables: Path | None) -> list[str]:
    command = [
        "qemu-system-x86_64",
        *accel_args(),
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

    print(f"qemu: {describe()}")
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
        if has_failure(transcript) or "AETHER_BOOT_OK" not in transcript:
            print(transcript)
            print("FAIL: boot markers not reached before shell test", file=sys.stderr)
            return 1
        transcript += read_until(client, b"aether> ", 10.0)

        failures = []
        for command, expected in (
            ("help", "commands:"),
            ("ls /", "system\td"),
            ("cat /system/policy.txt", "aether policy v1"),
            ("write /home/shell-a.txt shell-copy", "write: ok"),
            ("cp /home/shell-a.txt /home/shell-b.txt", "cp: ok"),
            ("cat /home/shell-b.txt", "shell-copy"),
            ("mv /home/shell-b.txt /home/shell-c.txt", "mv: ok"),
            ("cat /home/shell-c.txt", "shell-copy"),
            ("write /home/shell-r.txt first-version", "write: ok"),
            ("write /home/shell-r.txt second-version", "write: ok"),
            ("restore /home/shell-r.txt", "restore: ok"),
            ("cat /home/shell-r.txt", "first-version"),
            ("rm /home/shell-r.txt", "rm: ok"),
            ("rm /home/shell-a.txt", "rm: ok"),
            ("rm /home/shell-c.txt", "rm: ok"),
            ("apps", "app org.aether.demo pid=4"),
            ("security", "security: open_files="),
            ("mem", "mem: free="),
            ("services", "service storaged"),
            ("portal", "portal: grants=1"),
            ("portal approve 0", "portal: approved request 0"),
            ("portal", "portal: grants=2"),
            ("portal revoke 1", "portal: revoked grant 1"),
            ("portal", "portal: grants=1"),
            ("recovery", "IMAGES: CURRENT + PREVIOUS"),
            ("packages", "package org.aether.demo"),
            ("runtime", "runtime org.aether.demo"),
            ("updates", "updates: installed=0.1.0"),
            ("desktop", "desktop: space="),
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

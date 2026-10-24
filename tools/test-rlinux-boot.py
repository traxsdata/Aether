#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Boot the pinned rlinux kernel to a shell under QEMU/OVMF (issue #186).

Phase 0 of docs/rlinux-migration.md, step 2 of 3. Boots the bzImage built by
`make rlinux-kernel` (the commit from tools/pins/rlinux.pin) through the
kernel's EFI stub under OVMF: QEMU's `-kernel`/`-initrd` firmware handoff
(the command line goes via `-append`), the same device set the Aether boot
tests use (VGA, a small virtio disk, serial on ttyS0), and the accelerator
choice of tools/qemu_accel.py (KVM by default, AETHER_QEMU_ACCEL=tcg for
software emulation).

The image is not the Aether disk image: `build/rlinux/initramfs.cpio.gz`
(from tools/build-rlinux-initramfs.sh) carries a busybox /init that mounts
proc/sys/devtmpfs, prints AETHER-RLINUX: init, and hands over to a shell on
the serial console. The harness then talks to that shell through QEMU's
serial chardev: it sends `uname -r; echo "AETHER-RLINUX: shell-$((6 * 7))"`
and waits for `AETHER-RLINUX: shell-42`, which only a running shell can
print (the terminal's echo of the typed command doesn't contain it). It
fails on a timeout, a kernel panic, or a missing marker, and prints the
serial log tail on failure.

The serial chardev is a socket with a logfile, the same pattern as
tools/desktop_smoke.py, so the harness can send input (a plain
`-serial file:` target is read-only for the guest).
"""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

from boot_support import find_firmware, socket_path
from qemu_accel import accel_args, describe

ROOT = Path(__file__).resolve().parent.parent
RLINUX = ROOT / "build" / "rlinux"
BZIMAGE = RLINUX / "bzImage"
INITRAMFS = RLINUX / "initramfs.cpio.gz"
DISK = RLINUX / "disk.raw"
SERIAL = RLINUX / "serial.log"
QEMU_LOG = RLINUX / "qemu.log"
VARS = RLINUX / "OVMF_VARS.fd"

# The shell must *compute* part of the marker: the terminal echoes the typed
# command back, so a marker that appears literally in the command would
# "pass" as soon as the line is echoed, even if the shell never ran it.
SHELL_COMMAND = 'uname -r; echo "AETHER-RLINUX: shell-$((6 * 7))"'

INIT_MARKER = "AETHER-RLINUX: init"
SHELL_MARKER = "AETHER-RLINUX: shell-42"
KERNEL_VERSION_MARKER = "Linux version"
KERNEL_PANIC = "Kernel panic"

# TCG booting an upstream kernel is slow; the deadline covers the whole
# run (boot, shell handover, command round-trip).
TIMEOUT = int(os.environ.get("AETHER_RLINUX_BOOT_TIMEOUT", "240"))
# Aether's other harnesses default to VNC display 9; this one runs alongside
# `make ci` and takes display 10 by default.
VNC_DISPLAY = os.environ.get("AETHER_VNC_DISPLAY", "10")
MEMORY = os.environ.get("AETHER_MEMORY", "1024M")


class SerialDisconnected(Exception):
    """QEMU's serial chardev went away (no guest output will ever arrive)."""


def connect(path: Path, process: subprocess.Popen, timeout: float = 30) -> socket.socket:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            client.connect(str(path))
            client.settimeout(0.25)
            return client
        except OSError:
            client.close()
            if process.poll() is not None:
                break
            time.sleep(0.05)
    raise RuntimeError(f"QEMU serial socket unavailable: {path}")


def ensure_disk() -> None:
    # The rlinux shell never touches the disk; the issue only requires it in
    # the device set (like tools/test-boot.py). A small zero-filled raw image
    # is enough and avoids any host-state dependency.
    if not DISK.exists():
        DISK.parent.mkdir(parents=True, exist_ok=True)
        with DISK.open("wb") as handle:
            handle.truncate(32 * 1024 * 1024)


def qemu_command(serial_path: Path, code: Path, variables: Path | None) -> list[str]:
    command = [
        "qemu-system-x86_64",
        *accel_args(),
        "-m",
        MEMORY,
        "-display",
        f"vnc=127.0.0.1:{VNC_DISPLAY}",
        "-device",
        "VGA",
        "-monitor",
        "none",
        "-no-reboot",
        "-kernel",
        str(BZIMAGE),
        "-initrd",
        str(INITRAMFS),
        "-append",
        "console=ttyS0",
        "-chardev",
        f"socket,id=console,path={serial_path},server=on,wait=off,logfile={SERIAL},logappend=off",
        "-serial",
        "chardev:console",
        "-drive",
        f"file={DISK},format=raw,if=virtio",
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


def main() -> int:
    problems = []
    if not BZIMAGE.is_file():
        problems.append(f"missing {BZIMAGE}: run `make rlinux-kernel` first")
    if not INITRAMFS.is_file():
        problems.append(f"missing {INITRAMFS}: run `sh tools/build-rlinux-initramfs.sh` first")
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        return 2

    print(f"qemu: {describe()}")
    code, variables = find_firmware()
    ensure_disk()
    serial_path = socket_path("rlinux-serial.sock")
    serial_path.unlink(missing_ok=True)
    SERIAL.unlink(missing_ok=True)

    process = subprocess.Popen(
        qemu_command(serial_path, code, variables),
        stdout=subprocess.DEVNULL,
        stderr=QEMU_LOG.open("w"),
    )
    error_log = process.stderr
    serial = None
    transcript = bytearray()
    sent = False
    started = time.monotonic()
    outcome = "timeout"
    try:
        serial = connect(serial_path, process)

        def receive() -> None:
            try:
                data = serial.recv(65536)
            except socket.timeout:
                return
            except OSError:
                # QEMU side of the chardev went away (reset/closed).
                raise SerialDisconnected
            if not data:
                raise SerialDisconnected
            transcript.extend(data)

        deadline = started + TIMEOUT
        while time.monotonic() < deadline:
            if process.poll() is not None:
                outcome = "qemu exited before the shell marker"
                break
            try:
                receive()
            except SerialDisconnected:
                outcome = "serial console disconnected before the shell marker"
                break
            text = transcript.decode(errors="replace")
            if KERNEL_PANIC in text:
                outcome = "kernel panic"
                break
            if INIT_MARKER in text and not sent:
                # /init reached its shell handover: talk to the shell, exactly
                # the command the acceptance test asks for.
                serial.sendall(SHELL_COMMAND.encode() + b"\n")
                sent = True
                continue
            if sent and SHELL_MARKER in text:
                outcome = "success"
                break
            time.sleep(0.1)
    except RuntimeError as error:
        outcome = f"startup failed: {error}"
    finally:
        if serial is not None:
            serial.close()
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        if error_log is not None:
            error_log.close()
        serial_path.unlink(missing_ok=True)

    log = transcript.decode(errors="replace")
    elapsed = time.monotonic() - started
    if outcome == "success":
        if KERNEL_VERSION_MARKER not in log:
            print(log[-8000:])
            print("FAIL: serial log is missing the `Linux version` line", file=sys.stderr)
            return 1
        print(log)
        # Last matching line per marker (the shell's own output is last).
        for marker in (KERNEL_VERSION_MARKER, INIT_MARKER, SHELL_MARKER):
            lines = log.splitlines()
            line = next((entry for entry in reversed(lines) if marker in entry), None)
            print(f"evidence: {line}")
        print(f"PASS: booted rlinux to a shell in {elapsed:.1f}s ({describe()})")
        return 0

    # Failure path: the issue requires the serial log tail on failure. When
    # QEMU itself died early the serial log is short or empty, so show QEMU's
    # own stderr too (e.g. a VNC port collision in parallel runs).
    tail = "\n".join(log.splitlines()[-40:])
    if tail:
        print(tail)
    if QEMU_LOG.is_file():
        qemu_tail = "\n".join(QEMU_LOG.read_text(errors="replace").splitlines()[-10:])
        if qemu_tail:
            print(f"qemu.log (last lines):\n{qemu_tail}")
    print(f"FAIL: {outcome} (after {elapsed:.1f}s; {describe()})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())

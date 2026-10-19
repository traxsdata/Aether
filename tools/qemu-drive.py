#!/usr/bin/env python3
"""Boot æther in headless QEMU and drive it like a user: type, move the
mouse, click, take screenshots. Used for testing the desktop.

Usage: tools/qemu-drive.py SCRIPT [OUTDIR]
SCRIPT lines (blank lines and # comments ignored):
    wait <seconds>
    type <text>          (use \\n for Enter)
    key <qemu-key-name>  (e.g. ret, backspace, up, alt-tab, ctrl-alt-t)
    move <dx> <dy>       (relative mouse motion)
    scroll <n>           (mouse wheel notches; in QEMU, negative scrolls down)
    click                (left button down + up)
    down / up            (left button only, for dragging)
    shot <name>          (saves OUTDIR/<name>.png)
    expect <text>        (fail unless the serial log contains text)
    expect-count <n> <text> (fail unless text occurs exactly n times)
"""
import os
import socket
import subprocess
import sys
import time

from PIL import Image

OVMF = os.environ.get("OVMF_CODE", "/usr/share/OVMF/OVMF_CODE_4M.fd")
KEYS = {" ": "spc", "\n": "ret", "-": "minus", "=": "equal", ".": "dot", "/": "slash", ",": "comma",
        ";": "semicolon", "'": "apostrophe", "`": "grave_accent", "[": "bracket_left", "]": "bracket_right",
        "\\": "backslash", "*": "shift-8", "+": "shift-equal", "%": "shift-5", "(": "shift-9", ")": "shift-0",
        "_": "shift-minus", ":": "shift-semicolon", "<": "shift-comma", ">": "shift-dot", "?": "shift-slash",
        "!": "shift-1", "@": "shift-2", "#": "shift-3", "$": "shift-4", "^": "shift-6", "&": "shift-7"}


def keyname(c):
    if c in KEYS:
        return KEYS[c]
    if c.isupper():
        return "shift-" + c.lower()
    return c


def main():
    script, out = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else "build/test")
    os.makedirs(out, exist_ok=True)
    sock_path, serial = os.path.join(out, "mon.sock"), os.path.join(out, "serial.log")
    for p in (sock_path, serial):
        if os.path.exists(p):
            os.remove(p)
    # Test disk on the q35 ICH9 AHCI controller: $AETHER_DISK, else build/disk.img if present.
    disk = os.environ.get("AETHER_DISK") or ("build/disk.img" if os.path.exists("build/disk.img") else None)
    if disk == "none":  # no disk: æther runs on temporary in-memory storage
        disk = None
    # Network card: $AETHER_NIC (e1000e, e1000, none...) on QEMU's user-mode network,
    # where 10.0.2.2 is the host and 10.0.2.3 its DNS forwarder. Default: q35's e1000e.
    nic = os.environ.get("AETHER_NIC", "e1000e")
    # $AETHER_NIC_OPTS adds user-network options (tests/net.txt uses a guestfwd).
    extra = os.environ.get("AETHER_NIC_OPTS", "")
    nic_args = ["-nic", "none"] if nic == "none" else ["-nic", f"user,model={nic}" + (f",{extra}" if extra else "")]
    disk_args = ["-drive", f"file={disk},format=raw,if=none,id=d0", "-device", "ide-hd,drive=d0,bus=ide.0"] if disk else []
    qemu = subprocess.Popen([
        "qemu-system-x86_64", "-machine", "q35", "-cpu", os.environ.get("QEMU_CPU", "max"), "-m", "1G",
        "-display", "none", "-no-reboot",
        "-drive", f"if=pflash,format=raw,readonly=on,file={OVMF}", "-cdrom", "build/aether.iso",
        "-serial", f"file:{serial}", "-monitor", f"unix:{sock_path},server,nowait"] + disk_args + nic_args)
    for _ in range(50):
        if os.path.exists(sock_path):
            break
        time.sleep(0.1)
    mon = socket.socket(socket.AF_UNIX)
    mon.connect(sock_path)

    def cmd(c):
        mon.sendall((c + "\n").encode())
        time.sleep(0.08)

    ok = True
    try:
        for line in open(script):
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            op, _, arg = line.partition(" ")
            if op == "wait":
                time.sleep(float(arg))
            elif op == "type":
                for c in arg.replace("\\n", "\n"):
                    cmd("sendkey " + keyname(c))
            elif op == "key":
                cmd("sendkey " + arg)
            elif op == "move":
                dx, dy = map(int, arg.split())
                while dx or dy:  # PS/2 packets carry at most +-255
                    sx, sy = max(-100, min(100, dx)), max(-100, min(100, dy))
                    cmd(f"mouse_move {sx} {sy}")
                    dx, dy = dx - sx, dy - sy
            elif op == "scroll":  # wheel notches; QEMU: positive = up
                cmd(f"mouse_move 0 0 {int(arg)}")
            elif op == "click":
                cmd("mouse_button 1")
                time.sleep(0.15)
                try:
                    cmd("mouse_button 0")
                except OSError:
                    pass  # e.g. the Shut Down item powered the guest off on press
            elif op == "down":
                cmd("mouse_button 1")
            elif op == "up":
                cmd("mouse_button 0")
            elif op == "shot":
                ppm = os.path.join(out, arg + ".ppm")
                cmd(f"screendump {ppm}")
                time.sleep(0.6)
                Image.open(ppm).save(os.path.join(out, arg + ".png"))
                os.remove(ppm)
            elif op == "expect":
                text = open(serial, errors="replace").read()
                if arg not in text:
                    print(f"EXPECT FAILED: {arg!r}")
                    ok = False
            elif op == "expect-count":
                n, _, needle = arg.partition(" ")
                text = open(serial, errors="replace").read()
                if text.count(needle) != int(n):
                    print(f"EXPECT-COUNT FAILED: {needle!r} occurs {text.count(needle)} times, want {n}")
                    ok = False
    finally:
        try:
            cmd("quit")
        except OSError:
            pass  # the guest already powered off and QEMU exited
        try:
            qemu.wait(timeout=10)
        except subprocess.TimeoutExpired:
            qemu.kill()
    print(open(serial, errors="replace").read())
    print("PASS" if ok and "PANIC" not in open(serial, errors="replace").read() else "FAIL")
    sys.exit(0 if ok else 1)


main()

# SPDX-License-Identifier: Apache-2.0
"""Real PS/2, Files and compositor checks against a booted distribution ISO.

QEMU's VMware SVGA device exercises another GOP adapter, not VMware Workstation
itself. No synthetic kernel injection is used by these interactive checks.
"""

from __future__ import annotations

import json
import re
import shutil
import socket
import statistics
import subprocess
import time
from pathlib import Path

from boot_support import BASE_MARKERS, find_firmware, has_failure, socket_path
from qemu_accel import accel_args, describe

# Pixels per PS/2 count; mirrors MOUSE_GAIN in kernel/src/input.rs.
MOUSE_GAIN = 2


def connect(path: Path, process: subprocess.Popen, timeout: float = 15) -> socket.socket:
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


def parse_state(text: str) -> dict:
    state = re.search(r"desktop: state space=(\d+) focus=(Some\((\d+)\)|None) mode=(\w+)[^\r\n]* transition_active=(true|false)", text)
    render = re.search(r"desktop: render scene=(\d+) cursor=(\d+) last_ms=(\d+) max_ms=(\d+)", text)
    files = re.search(r"desktop: files path=(\S+) entries=(\d+) selected=(\d+) preview=(true|false) error=([^\r\n]+)", text)
    if not state or not render or not files:
        raise RuntimeError(f"incomplete desktop diagnostics:\n{text}")
    windows = {}
    for match in re.finditer(r"desktop: w(\d+) k=(\w+) ws(\d+) (\w+) v\w+ m\d+ (-?\d+),(-?\d+),(\d+),(\d+)", text):
        windows[match[2]] = {"id": int(match[1]), "workspace": int(match[3]), "state": match[4], "frame": tuple(map(int, match.groups()[4:]))}
    return {"workspace": int(state[1]), "focus": int(state[3]) if state[3] else None,
            "mode": state[4], "transition_active": state[5] == "true", "windows": windows, "scene_paints": int(render[1]),
            "cursor_paints": int(render[2]), "last_ms": int(render[3]), "max_ms": int(render[4]),
            "path": files[1], "entries": int(files[2]), "selected": int(files[3]),
            "preview": files[4] == "true", "files_error": files[5]}


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    # QEMU's screendump emits this fixed binary PPM header. Split only header
    # newlines, never strip whitespace-valued bytes from the pixel stream.
    magic, dimensions, maximum, pixels = path.read_bytes().split(b"\n", 3)
    width, height = map(int, dimensions.split())
    if magic != b"P6" or maximum != b"255" or len(pixels) != width * height * 3:
        raise RuntimeError(f"invalid QEMU screenshot: {path}")
    return width, height, pixels


class DesktopVM:
    def __init__(self, iso: Path, device: str, out: Path):
        print(f"qemu: {describe()}")
        self.out = out
        out.mkdir(parents=True, exist_ok=True)
        (out / "result.txt").unlink(missing_ok=True)
        # Unix socket paths are capped at 108 bytes (SUN_PATH_MAX), so long
        # checkout directories must not host them under build/.
        self.serial_path = socket_path("serial.sock")
        self.qmp_path = socket_path("qmp.sock")
        for path in (self.serial_path, self.qmp_path):
            path.unlink(missing_ok=True)
        self.transcript = bytearray()
        code, variables = find_firmware()
        command = ["qemu-system-x86_64", *accel_args(),
                   "-m", "2048M", "-smp", "2", "-display", "none", "-vga", "none",
                   "-device", device, "-monitor", "none", "-no-reboot", "-boot", "d",
                   "-cdrom", str(iso), "-qmp", f"unix:{self.qmp_path},server=on,wait=off",
                   "-chardev", f"socket,id=console,path={self.serial_path},server=on,wait=off,logfile={out / 'serial.log'},logappend=off",
                   "-serial", "chardev:console"]
        if variables is None:
            command += ["-bios", str(code)]
        else:
            target = out / "OVMF_VARS.fd"
            shutil.copyfile(variables, target)
            command += ["-drive", f"if=pflash,format=raw,readonly=on,file={code}",
                        "-drive", f"if=pflash,format=raw,file={target}"]
        self.error_log = (out / "qemu.log").open("w")
        self.process = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=self.error_log)
        self.serial = None
        self.qmp_socket = None
        self.qmp_file = None
        try:
            self.serial = connect(self.serial_path, self.process)
            self.qmp_socket = connect(self.qmp_path, self.process)
            self.qmp_socket.settimeout(10)
            self.qmp_file = self.qmp_socket.makefile("rwb", buffering=0)
            json.loads(self.qmp_file.readline())
            self.qmp("qmp_capabilities")
        except BaseException:
            self.close()
            raise

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        for stream in (self.serial, self.qmp_file, self.qmp_socket, self.error_log):
            if stream is not None:
                stream.close()
        (self.out / "interaction.log").write_bytes(self.transcript)
        for path in (self.serial_path, self.qmp_path):
            path.unlink(missing_ok=True)

    def qmp(self, name: str, arguments: dict | None = None):
        self.qmp_file.write(json.dumps({"execute": name, "arguments": arguments or {}}).encode() + b"\n")
        while True:
            line = self.qmp_file.readline()
            if not line:
                raise RuntimeError("QMP disconnected")
            reply = json.loads(line)
            if "error" in reply:
                raise RuntimeError(f"QMP error: {reply['error']}")
            if "return" in reply:
                return reply["return"]

    def hmp(self, command: str):
        reply = self.qmp("human-monitor-command", {"command-line": command})
        if reply.strip():
            raise RuntimeError(f"monitor command {command!r}: {reply}")

    def receive(self) -> bytes:
        try:
            data = self.serial.recv(65536)
        except socket.timeout:
            return b""
        if not data:
            raise RuntimeError("serial console disconnected")
        self.transcript.extend(data)
        return data

    def boot(self, timeout: int):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.receive()
            text = self.transcript.decode(errors="replace")
            if has_failure(text):
                raise RuntimeError("ISO reported a boot/subsystem failure")
            if all(marker in text for marker in BASE_MARKERS):
                return
            if self.process.poll() is not None:
                raise RuntimeError("QEMU exited before the required boot markers")
        raise RuntimeError(f"ISO boot timed out after {timeout}s")

    def console(self, command: str) -> str:
        self.serial.settimeout(0.005)
        while self.receive():
            pass
        self.serial.settimeout(0.1)
        self.serial.sendall(command.encode() + b"\n")
        response = bytearray()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            response.extend(self.receive())
            if b"aether> " in response:
                text = response.decode(errors="replace")
                if has_failure(text):
                    raise RuntimeError(text)
                return text
        raise RuntimeError(f"console command timed out: {command}")

    def state(self) -> dict:
        return parse_state(self.console("desktop"))

    def wait(self, predicate, description: str) -> dict:
        deadline = time.monotonic() + 8
        state = None
        while time.monotonic() < deadline:
            state = self.state()
            if predicate(state):
                return state
        raise RuntimeError(f"desktop did not {description}: {state}")

    # Modifier names from the `input` diagnostic, translated to HMP
    # sendkey tokens for explicit release re-sends. The left and right meta
    # tokens exercise independent device paths (left and right Super map to
    # distinct scancode pairs), so both are re-sent.
    MOD_TOKENS = {
        "shift": ("shift",),
        "ctrl": ("ctrl",),
        "alt": ("alt",),
        "super": ("meta_l", "meta_r", "menu"),
    }

    def modifiers(self) -> tuple[dict[str, bool], str]:
        text = self.console("input")
        match = re.search(r"shift=([01]) ctrl=([01]) alt=([01]) super=([01])", text)
        if not match:
            raise RuntimeError(f"missing modifier diagnostics: {text}")
        held = {"shift": match[1] == "1", "ctrl": match[2] == "1", "alt": match[3] == "1", "super": match[4] == "1"}
        return held, text

    def settle_modifiers(self, timeout: float = 15.0):
        """Wait until the guest reports no modifier keys held.

        In this QEMU/TCG stack a PS/2 break byte can reach the guest much
        later than the host sent it (a traced boot showed a Super break
        landing only after two console round-trips), so the guest may still
        read Super or Shift as held when the harness moves on. The kernel
        legitimately acts only on the bytes it receives; therefore the
        harness verifies the modifier state through the guest's own `input`
        diagnostic and re-sends explicit releases until the guest confirms
        a clean state before the next plain key is asserted.
        """
        deadline = time.monotonic() + timeout
        stuck: list[str] = []
        text = ""
        while time.monotonic() < deadline:
            held, text = self.modifiers()
            stuck = [name for name, is_held in held.items() if is_held]
            if not stuck:
                return
            for name in stuck:
                for token in self.MOD_TOKENS.get(name, (name,)):
                    self.hmp(f"sendkey {token}")
                    # Give the guest a chance to read the make/break bytes
                    # before the next command; correctness is confirmed by
                    # the next modifiers() read, not by this delay.
                    time.sleep(0.05)
        raise RuntimeError(f"modifier keys still held in the guest: {stuck}\n{text}")

    def key(self, key: str):
        # HMP sendkey keeps the down-up order the guest decoder relies on
        # and is the delivery path with a byte-level track record in this
        # stack; QMP input-send-event key-up events for modifiers have been
        # observed not to reach the guest. Break bytes can still land late,
        # so settle_modifiers() guarantees a clean modifier state before
        # the harness asserts on the next plain key.
        self.hmp(f"sendkey {key} 20")
        self.settle_modifiers()

    def pointer(self) -> tuple[int, int]:
        text = self.console("input")
        cursor = re.search(r"cursor=(-?\d+),(-?\d+)", text)
        if not cursor:
            raise RuntimeError(f"missing pointer diagnostics: {text}")
        return int(cursor[1]), int(cursor[2])

    def move_to(self, x: int, y: int):
        # The kernel multiplies PS/2 counts by MOUSE_GAIN, so only every
        # second pixel is reachable from a given start. Rounding the counts
        # down lands one pixel short in that case, consistently, so relative
        # moves such as the title-bar drag stay exact.
        for _ in range(40):
            old_x, old_y = self.pointer()
            if 0 <= x - old_x < MOUSE_GAIN and 0 <= y - old_y < MOUSE_GAIN:
                return
            dx = max(-40, min(40, (x - old_x) // MOUSE_GAIN))
            dy = max(-40, min(40, (y - old_y) // MOUSE_GAIN))
            self.hmp(f"mouse_move {dx} {dy}")
        raise RuntimeError(f"PS/2 pointer did not reach {x},{y}")

    def button(self, down: bool):
        self.hmp(f"mouse_button {1 if down else 0}")
        time.sleep(0.04)

    def click(self):
        self.button(True)
        self.button(False)

    def screenshot(self, name: str) -> tuple[int, int, bytes]:
        path = self.out / name
        self.qmp("screendump", {"filename": str(path)})
        return read_ppm(path)


def exercise_iso(iso: Path, device: str, out: Path, timeout: int) -> None:
    vm = DesktopVM(iso, device, out)
    try:
        vm.boot(timeout)
        state = vm.state()
        welcome = state["windows"]["Welcome"]
        x, y, width, height = welcome["frame"]
        # QEMU's HMP has no generic "super" token; meta_l is the Super key.
        vm.key("meta_l-right")  # E0 dedicated arrow, not the old keypad-only self-test
        state = vm.wait(lambda value: value["windows"]["Welcome"]["frame"][0] == x + 8, "move the window with Super+Right")
        vm.key("meta_l-alt-right")
        vm.wait(lambda value: value["workspace"] == 1 and not value["transition_active"], "finish switching to Space 2")
        vm.key("meta_l-alt-left")
        # Logical workspace/focus changes precede the slide animation. Wait for
        # the guest's actual transition state, not an arbitrary host delay,
        # before clicking coordinates from the final logical window frames.
        vm.wait(lambda value: value["workspace"] == 0 and not value["transition_active"], "finish returning to Space 1")

        # Drag and resize use actual IRQ12 packets and IRQ1 modifier scancodes.
        state = vm.state()
        x, y, width, height = state["windows"]["Welcome"]["frame"]
        vm.move_to(x + width // 2, y + 12)
        vm.button(True)
        vm.move_to(x + width // 2 + 40, y + 36)
        vm.button(False)
        vm.wait(lambda value: value["windows"]["Welcome"]["frame"][:2] == (x + 40, y + 24), "drag the title bar")
        vm.key("meta_l-shift-right")
        vm.wait(lambda value: value["windows"]["Welcome"]["frame"][2] == width + 8, "resize with Super+Shift+Right")

        # Partial repaints must produce the same pixels as an explicit full paint
        # (apart from the changing uptime clock in the menu bar).
        time.sleep(0.2)
        w, h, partial = vm.screenshot("partial.ppm")
        vm.console("desktop redraw")
        _, _, full = vm.screenshot("full.ppm")
        mismatches = sum(partial[index:index + 3] != full[index:index + 3]
                         for index in range(0, len(full), 3)
                         if not (index // 3 // w < 27 and index // 3 % w >= w - 80))
        if mismatches:
            raise RuntimeError(f"damage repaint differs from full paint at {mismatches} pixels")
        for color in (b"\xff\x6f\x6f", b"\x67\xe8\xd5"):
            if not any(full[index:index + 3] == color for index in range(0, len(full), 3)):
                raise RuntimeError(f"framebuffer byte-order/color check failed: {color.hex()}")

        # Cursor-only movement over a static window must use the small path.
        vm.move_to(x + 180, y + 150)
        before = vm.state()
        vm.move_to(x + 185, y + 155)
        vm.wait(lambda value: value["cursor_paints"] > before["cursor_paints"], "present a cursor-only move without redrawing the scene")

        # Files is no longer just painted chrome: both pointer and keyboard
        # navigate real folders and open a service-backed, read-only preview.
        vm.key("meta_l-n")
        state = vm.wait(lambda value: value["focus"] == value["windows"]["Files"]["id"], "focus Files")
        fx, fy, fw, fh = state["windows"]["Files"]["frame"]
        sidebar = max(92, min(138, (fw - 2) // 3))
        # Home is sidebar row 0 (LOCATIONS order since the unified-UX
        # rework); the click targets that row's center.
        vm.move_to(fx + sidebar // 2, fy + 31 + 46 + 11)
        vm.click()
        vm.wait(lambda value: value["path"] == "/home", "open Home from the Files sidebar")
        vm.move_to(fx + 1 + sidebar + 50, fy + 31 + 38 + 11)
        vm.click()
        vm.click()
        vm.wait(lambda value: value["preview"], "double-click welcome.txt into a preview")
        vm.key("backspace")
        vm.wait(lambda value: not value["preview"] and value["path"] == "/home", "close the preview")
        vm.key("backspace")
        vm.wait(lambda value: value["path"] == "/", "navigate to the parent")
        vm.key("down")
        vm.key("down")
        vm.key("ret")
        vm.wait(lambda value: value["path"] == "/home", "open a folder with dedicated arrows and Enter")
        vm.key("ret")
        vm.wait(lambda value: value["preview"], "open a file with Enter")
        vm.key("backspace")
        vm.key("backspace")
        vm.key("ret")  # root's first entry is the private application vaults
        vm.wait(lambda value: value["path"] == "/" and "Access" in value["files_error"], "deny browsing private application data")

        vm.key("meta_l-spc")
        vm.wait(lambda value: value["mode"] == "SEARCH", "open Aether Search")
        for letter in "portal":
            vm.key(letter)
        vm.key("ret")
        vm.wait(lambda value: "Portal" in value["windows"], "open the File Portal from search")
        portal = vm.state()["windows"]["Portal"]
        px, py, pw, ph = portal["frame"]
        # The consent controls are painted from the same content geometry used
        # by the pointer route: 50px cards, 56px gap, then a 34px offset.
        vm.move_to(px + 1 + 16 + 42, py + 31 + 140 + 11)
        vm.click()
        time.sleep(0.3)
        grants = vm.console("portal")
        if "grants=2" not in grants:
            raise RuntimeError(f"portal consent click did not install a grant:\n{grants}")
        vm.console("portal revoke 1")

        vm.key("meta_l-spc")
        vm.wait(lambda value: value["mode"] == "SEARCH", "reopen Aether Search")
        panel_width = min(520, w - 64)
        panel_height = min(312, h - 100)
        vm.move_to((w - panel_width) // 2 + 80, (h - panel_height) // 3 + 28)
        vm.click()
        vm.wait(lambda value: value["mode"] == "SEARCH", "keep search open after a field click")
        vm.key("esc")
        vm.key("f10")
        vm.wait(lambda value: value["mode"] == "MENU", "open menus without a pointer")
        vm.key("right")
        vm.key("esc")
        vm.wait(lambda value: value["mode"] == "WINDOW", "close the menu")

        # Report, but do not impose a host-speed-dependent absolute CI budget.
        durations = []
        for _ in range(5):
            started = time.monotonic()
            vm.console("desktop redraw")
            durations.append((time.monotonic() - started) * 1000)
        summary = (f"PASS: {device} {w}x{h}: real PS/2 arrows/drag/resize, Files folders/previews, "
                   f"private-data denial, portal consent click, search, menu, cursor-only path, "
                   f"RGB colors and damage/full equality; "
                   f"redraw+diagnostic median={statistics.median(durations):.1f}ms samples={[round(value, 1) for value in durations]}")
        (out / "result.txt").write_text(summary + "\n")
        print(summary)
    except Exception:
        print(vm.transcript.decode(errors="replace"))
        print((out / "qemu.log").read_text(errors="replace"))
        raise
    finally:
        vm.close()

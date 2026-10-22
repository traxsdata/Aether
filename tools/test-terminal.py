#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Drive the desktop Terminal and its Rust userland over the serial console.

The kernel console's `term` command opens the Terminal window, types into it
as the keyboard would, and dumps the character grid. This test runs the shell
and a spread of commands through that path and checks what reaches the
screen: files and text tools, pipes and redirection, the process tools and
their session boundary, Ctrl+C, and the full-screen htop and nano.
"""

from __future__ import annotations

import socket
import subprocess
import sys
import time

from boot_support import find_firmware, has_failure

import importlib

shell_test = importlib.import_module("test-shell")

PROMPT = "user@aether:"


class Terminal:
    def __init__(self, client: socket.socket) -> None:
        self.client = client
        self.transcript = ""

    def console(self, command: str, marker: bytes = b"aether> ", timeout: float = 10.0) -> str:
        self.client.sendall(command.encode() + b"\n")
        output = shell_test.read_until(self.client, marker, timeout)
        if marker != b"aether> ":
            output += shell_test.read_until(self.client, b"aether> ", 5.0)
        self.transcript += output
        return output

    def screen(self) -> tuple[str, list[str]]:
        """Returns the dump header and the 24 rows."""
        output = self.console("term dump", b"term: end")
        header = ""
        rows = []
        for line in output.splitlines():
            if line.startswith("term: screen"):
                header = line
                rows = []
            elif line.startswith("term| "):
                rows.append(line[6:].rstrip())
            elif line.startswith("term|"):
                rows.append("")
        return header, rows

    def type(self, text: str) -> None:
        # The console splits on spaces only for the verb, so text is sent as is.
        self.console(f"term type {text}")

    def wait_for(self, predicate, timeout: float = 15.0) -> tuple[str, list[str]]:
        deadline = time.monotonic() + timeout
        header, rows = self.screen()
        while time.monotonic() < deadline:
            if predicate(header, rows):
                return header, rows
            time.sleep(0.4)
            header, rows = self.screen()
        return header, rows

    def run(self, command: str, timeout: float = 15.0) -> list[str]:
        """Clears the screen, runs `command`, and returns its output rows."""
        self.type("clear\\n")
        self.wait_for(lambda _h, rows: rows and rows[0].startswith(PROMPT) and all(not r for r in rows[1:]))
        self.type(command + "\\n")

        def finished(_header: str, rows: list[str]) -> bool:
            prompts = [index for index, row in enumerate(rows) if row.startswith(PROMPT)]
            return len(prompts) >= 2

        _, rows = self.wait_for(finished, timeout)
        prompts = [index for index, row in enumerate(rows) if row.startswith(PROMPT)]
        if len(prompts) < 2:
            return rows
        return rows[prompts[0] + 1 : prompts[-1]]


def main() -> int:
    image = shell_test.IMAGE
    if not image.exists():
        print("error: build/aether.img missing; run `make image`", file=sys.stderr)
        return 2
    code, variables = find_firmware()
    shell_test.BUILD.mkdir(exist_ok=True)
    shell_test.SOCKET.unlink(missing_ok=True)
    process = subprocess.Popen(
        shell_test.qemu_command(code, variables),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )
    failures: list[str] = []
    terminal = None
    try:
        client = shell_test.connect(20.0)
        boot = shell_test.read_until(client, b"AETHER_BOOT_OK", 90.0)
        if has_failure(boot) or "AETHER_BOOT_OK" not in boot:
            print(boot)
            print("FAIL: boot markers not reached before terminal test", file=sys.stderr)
            return 1
        shell_test.read_until(client, b"aether> ", 10.0)
        terminal = Terminal(client)

        opened = terminal.console("term open")
        if "term: open leader=" not in opened or "leader=0" in opened:
            failures.append(f"terminal did not start a shell: {opened!r}")
            raise StopIteration
        _, rows = terminal.wait_for(lambda _h, rows: any(row.startswith(PROMPT) for row in rows), 20.0)
        if not any(row.startswith(PROMPT) for row in rows):
            failures.append("shell prompt never appeared:\n" + "\n".join(rows))
            raise StopIteration

        def expect(command: str, check, description: str, timeout: float = 15.0) -> None:
            output = terminal.run(command, timeout)
            text = "\n".join(output)
            print(f"$ {command}\n{text}", flush=True)
            if not check(output, text):
                failures.append(f"{command!r}: expected {description}, got:\n{text}")

        expect("uname -a", lambda _o, t: "Aether aether" in t and "x86_64" in t, "system name")
        expect("pwd", lambda o, _t: o == ["/"], "the home folder as /")
        expect("whoami", lambda o, _t: o == ["user"], "the ordinary user")
        expect("echo one two three | wc -w", lambda o, _t: o == ["3"], "a word count of 3")
        expect(
            "mkdir -p demo/inner && echo kept > demo/a.txt && cat demo/a.txt",
            lambda o, _t: o == ["kept"],
            "redirected text read back",
        )
        expect("echo more >> demo/a.txt; wc -l demo/a.txt", lambda _o, t: "2 demo/a.txt" in t, "two lines")
        expect("seq 1 5 | sort -r | head -n 2", lambda o, _t: o == ["5", "4"], "a three-stage pipe")
        expect("grep -c e < demo/a.txt", lambda o, _t: o == ["2"], "input redirection")
        expect("ls demo", lambda _o, t: "a.txt" in t and "inner" in t, "the new entries")
        expect(
            "echo replaced > demo/a.txt; cat demo/a.txt; ls demo",
            lambda o, _t: o == ["replaced", "a.txt  inner"],
            "an overwrite that leaves no scratch files",
        )
        expect(
            "echo kept > demo/a.txt; echo more >> demo/a.txt",
            lambda o, _t: o == [],
            "the file restored for the next step",
        )
        expect("cp demo/a.txt b.txt && mv b.txt c.txt && cat c.txt", lambda o, _t: o == ["kept", "more"], "copy and move")
        expect("rm -r demo c.txt && ls demo", lambda _o, t: "No such file" in t, "the folder removed")
        expect("cat /system/policy.txt", lambda _o, t: "No such file" in t, "files outside home hidden")
        expect("false || echo recovered", lambda o, _t: o == ["recovered"], "|| after a failure")
        expect("nosuchprogram", lambda _o, t: "command not found" in t, "an unknown command")
        expect("ps", lambda _o, t: "PID" in t and " sh" in t and " ps" in t, "the session's processes")
        expect("ps -e | grep -c kernel", lambda o, _t: o and o[0].isdigit() and int(o[0]) > 0, "kernel tasks listed")
        expect("kill 1", lambda _o, t: "not permitted" in t.lower(), "a refused kill outside the session")
        expect("free -h", lambda _o, t: "Mem:" in t, "a memory report")
        expect("df -h", lambda _o, t: "aetherfs" in t, "a filesystem report")
        expect("uptime", lambda _o, t: " up " in t, "an uptime line")

        # Ctrl+C stops a job that would otherwise run forever.
        terminal.type("clear\\n")
        terminal.type("yes\\n")
        time.sleep(1.0)
        terminal.type("\\c")
        terminal.type("clear\\n")
        terminal.type("echo after \"&&\" done\\n")
        _, rows = terminal.wait_for(lambda _h, rows: "after && done" in rows, 15.0)
        if "after && done" not in rows:
            failures.append("Ctrl+C did not return to the shell:\n" + "\n".join(rows))

        # htop takes over the alternate screen and leaves it on q.
        terminal.type("clear\\n")
        terminal.type("htop\\n")
        header, rows = terminal.wait_for(
            lambda header, rows: "alternate=true" in header and any("PID" in row and "Command" in row for row in rows),
            15.0,
        )
        if "alternate=true" not in header or not any("CPU[" in row for row in rows) or not any("F10Quit" in row for row in rows):
            failures.append("htop did not draw its screen:\n" + "\n".join(rows))
        terminal.type("q")
        header, rows = terminal.wait_for(lambda header, _rows: "alternate=false" in header, 10.0)
        if "alternate=false" not in header:
            failures.append("htop did not leave the alternate screen")

        # nano edits and saves a file.
        terminal.type("clear\\n")
        terminal.type("nano note.txt\\n")
        header, rows = terminal.wait_for(lambda header, rows: rows and "note.txt" in rows[0], 15.0)
        if not rows or "Aether nano" not in rows[0] or not any("^X Exit" in row for row in rows):
            failures.append("nano did not draw its screen:\n" + "\n".join(rows))
        terminal.type("written\\sin\\snano\\n")
        terminal.type("\\x0f")  # ^O
        terminal.wait_for(lambda _h, rows: any("File Name to Write: note.txt" in row for row in rows), 10.0)
        terminal.type("\\n")
        _, rows = terminal.wait_for(lambda _h, rows: any("Wrote 1 line" in row for row in rows), 10.0)
        if not any("Wrote 1 line" in row for row in rows):
            failures.append("nano did not save:\n" + "\n".join(rows))
        terminal.type("\\x18")  # ^X
        terminal.wait_for(lambda header, _rows: "alternate=false" in header, 10.0)
        expect("cat note.txt", lambda o, _t: o == ["written in nano"], "the text nano saved")

        # Closing the window ends the session.
        closed = terminal.console("term close")
        if "term: closed" not in closed:
            failures.append("terminal did not close")
    except StopIteration:
        pass
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        shell_test.SOCKET.unlink(missing_ok=True)

    if terminal is not None:
        print(terminal.transcript)
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print("PASS: terminal shell, Rust commands, pipes, Ctrl+C, htop, and nano")
    return 0


if __name__ == "__main__":
    sys.exit(main())

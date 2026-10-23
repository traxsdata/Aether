# SPDX-License-Identifier: Apache-2.0
"""Fast regression tests for artifact selection, VMX encoding, boot evidence and the Craft driver's exit code."""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import boot_support
import qemu_accel
from boot_support import (
    BASE_MARKERS,
    UNIX_SOCKET_PATH_LIMIT,
    find_firmware,
    find_iso,
    has_failure,
    socket_path,
)
from make_vmware_vmx import render, vmx_string
from desktop_smoke import parse_state, read_ppm


def _load_script(module_name: str, file_name: str):
    """Load a sibling tools/ script whose hyphenated name cannot be imported."""
    spec = importlib.util.spec_from_file_location(
        module_name, Path(__file__).with_name(file_name)
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


craft_apps = _load_script("_test_craft_apps", "test-craft-apps.py")


class ToolingTests(unittest.TestCase):
    def test_workspace_selection_is_distinct_from_animation_completion(self):
        diagnostics = (
            "desktop: state space=0 focus=Some(3) mode=WINDOW order=[1, 2, 3] transition_active=true\n"
            "desktop: w3 k=Welcome ws0 Normal vtrue m0 234,156,300,230\n"
            "desktop: render scene=1 cursor=0 last_ms=10 max_ms=10\n"
            "desktop: files path=/ entries=4 selected=0 preview=false error=None\n"
        )
        state = parse_state(diagnostics)
        self.assertEqual(state["workspace"], 0)
        self.assertTrue(state["transition_active"])
        self.assertFalse(parse_state(diagnostics.replace("transition_active=true", "transition_active=false"))["transition_active"])
        with self.assertRaises(RuntimeError):
            parse_state(diagnostics.replace(" transition_active=true", ""))

    def test_release_tamper_mutates_every_digest_prefix_in_multiline_sums(self):
        # Exercise the actual shell harness. The stub verifier isolates its
        # mutation logic; make release-demo still tests the real verifier.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            script = tools / "release-tamper-test.sh"
            shutil.copyfile(Path(__file__).with_name("release-tamper-test.sh"), script)
            (tools / "release-check.sh").write_text(
                '#!/bin/sh\nset -eu\n'
                'if ! cmp -s "$AETHER_RELEASE_DIR/SHA256SUMS" "$ORIGINAL_BUNDLE/SHA256SUMS"; then\n'
                '    echo "SHA256SUMS and release-info.txt disagree about the image"\n'
                '    exit 1\n'
                'fi\n'
                'if ! cmp -s "$AETHER_RELEASE_DIR/fixture.iso" "$ORIGINAL_BUNDLE/fixture.iso"; then\n'
                '    echo DigestMismatch\n'
                '    exit 1\n'
                'fi\n'
                'exit 0\n'
            )
            bundle = root / "bundle"
            bundle.mkdir()
            (bundle / "release-info.txt").write_text("image=fixture.iso\n")
            (bundle / "fixture.iso").write_bytes(b"\0" * (262144 + 1))
            (bundle / "fixture.iso.manifest").write_text("test fixture\n")
            environment = dict(os.environ, AETHER_TRUST="test-only:" + "a" * 64,
                               AETHER_BUNDLE=str(bundle), ORIGINAL_BUNDLE=str(bundle))
            for prefix in "0123456789abcdef":
                with self.subTest(prefix=prefix):
                    (bundle / "SHA256SUMS").write_text(
                        prefix + "1" * 63 + "  fixture.iso\n" + "b" * 64 + "  fixture.iso.manifest\n"
                    )
                    result = subprocess.run(["sh", str(script)], cwd=root, env=environment,
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("verify refused both mutations", result.stdout)

    def test_ppm_parser_does_not_strip_whitespace_pixel_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            image = Path(directory) / "screen.ppm"
            image.write_bytes(b"P6\n1 1\n255\n\x0a\x20\x09")
            self.assertEqual(read_ppm(image), (1, 1, b"\x0a\x20\x09"))
            image.write_bytes(b"P6\n1 1\n255\n\x00")
            with self.assertRaises(RuntimeError):
                read_ppm(image)

    def test_missing_desktop_diagnostics_cannot_pass_an_interaction(self):
        with self.assertRaises(RuntimeError):
            parse_state("AETHER_BOOT_OK\n")

    def test_subsystem_failures_cannot_hide_behind_boot_ok(self):
        for marker in ("AETHER_INPUT_FAILED", "AETHER_FILES_FAILED", "AETHER_GRAPHICS_FAILED", "AETHER_KERNEL_PANIC"):
            self.assertTrue(has_failure(f"{marker}\nAETHER_BOOT_OK\n"))
        self.assertFalse(has_failure("AETHER_INPUT_OK\nAETHER_BOOT_OK\n"))
        self.assertIn("AETHER_FILES_OK", BASE_MARKERS)
        self.assertIn("AETHER_INPUT_OK", BASE_MARKERS)
        self.assertIn("AETHER_LINUX_PROCESS_GROUP_OK", BASE_MARKERS)
        self.assertIn("AETHER_LINUX_APPIMAGE_STAGED_OK", BASE_MARKERS)
        self.assertIn("AETHER_CR3_OK", BASE_MARKERS)

    def test_iso_selection_and_explicit_override(self):
        with tempfile.TemporaryDirectory() as directory:
            build = Path(directory)
            with self.assertRaises(ValueError):
                find_iso(build)
            (build / "ISOs").mkdir()
            first = build / "ISOs" / "a.iso"
            second = build / "ISOs" / "b.iso"
            first.write_bytes(b"first")
            second.write_bytes(b"second")
            os.utime(first, ns=(1, 1))
            os.utime(second, ns=(2, 2))
            self.assertEqual(find_iso(build), second.resolve())
            self.assertEqual(find_iso(build, str(first)), first.resolve())
            first.write_bytes(b"")
            with self.assertRaises(ValueError):
                find_iso(build, str(first))

    def test_vmx_escapes_paths_and_keeps_required_hardware(self):
        text = render(Path('/a folder/quote"and|pipe.iso'), Path("/build"))
        self.assertIn('fileName = "/a folder/quote|22and|7Cpipe.iso"', text)
        for setting in ('firmware = "efi"', 'uefi.secureBoot.enabled = "FALSE"', 'mouse.vusb.enable = "FALSE"', 'keyboard.vusb.enable = "FALSE"', 'serial0.present = "TRUE"', 'mks.enable3d = "FALSE"', 'ethernet0.virtualDev = "e1000e"', 'ethernet0.connectionType = "nat"'):
            self.assertIn(setting, text)
        for path in ('bad\npath', 'bad\rpath', 'bad\x00path'):
            with self.assertRaises(ValueError):
                vmx_string(path)

    def test_qemu_socket_paths_fit_the_unix_limit_or_fail_explicitly(self):
        path = socket_path("shell.sock")
        self.assertLess(len(os.fsencode(path)), UNIX_SOCKET_PATH_LIMIT)
        self.assertTrue(path.parent.is_dir())
        self.assertEqual(socket_path("shell.sock"), path)
        self.assertNotEqual(socket_path("qmp.sock"), path)
        long_root = Path("/") / ("d" * 150)
        with patch.object(boot_support, "_socket_dir", long_root):
            with self.assertRaises(RuntimeError) as caught:
                socket_path("shell.sock")
        message = str(caught.exception)
        self.assertIn(str(long_root / "shell.sock"), message)
        self.assertIn(str(UNIX_SOCKET_PATH_LIMIT), message)

    def test_invalid_firmware_overrides_fail_before_qemu(self):
        with patch.dict(os.environ, {"OVMF_CODE": "/nonexistent/aether-code.fd"}, clear=True):
            with self.assertRaises(SystemExit):
                find_firmware()
        with patch.dict(os.environ, {"OVMF_VARS": "/nonexistent/vars.fd"}, clear=True):
            with self.assertRaises(SystemExit):
                find_firmware()

    def test_craft_driver_exit_code_counts_failed_checks(self):
        passed = {"app": "a", "version": "1.0.0", "headless": "pass", "gui": "pass"}
        skipped = {"app": "a", "version": "1.0.0", "headless": "pass"}  # gui not run
        headless_failed = {"app": "a", "version": "1.0.0", "headless": "fail", "gui": "pass"}
        gui_failed = {"app": "a", "version": "1.0.0", "headless": "pass", "gui": "fail"}
        errored = {"app": "a", "version": "1.0.0", "error": "download failed"}
        self.assertFalse(craft_apps.results_failed([passed]))
        self.assertFalse(craft_apps.results_failed([passed, skipped]))
        self.assertTrue(craft_apps.results_failed([headless_failed]))
        self.assertTrue(craft_apps.results_failed([passed, gui_failed]))
        self.assertTrue(craft_apps.results_failed([passed, errored]))
        self.assertTrue(craft_apps.results_failed([headless_failed, errored]))

    def test_craft_driver_main_exits_nonzero_when_a_check_fails(self):
        def run_main(record):
            with tempfile.TemporaryDirectory() as directory:
                with patch.object(craft_apps, "BUILD", Path(directory)), patch.object(
                    craft_apps, "ensure_extractor", lambda: Path("/bin/true")
                ), patch.object(
                    craft_apps, "test_one", return_value=dict(record)
                ), patch.object(sys, "argv", ["test-craft-apps.py", "gridcraft", "--cache", directory]):
                    return craft_apps.main()

        self.assertEqual(
            run_main({"app": "a", "version": "1.0.0", "headless": "fail", "gui": "pass"}), 1
        )
        self.assertEqual(
            run_main({"app": "a", "version": "1.0.0", "headless": "pass", "gui": "fail"}), 1
        )
        self.assertEqual(
            run_main({"app": "a", "version": "1.0.0", "error": "download failed"}), 1
        )
        self.assertEqual(
            run_main({"app": "a", "version": "1.0.0", "headless": "pass", "gui": "pass"}), 0
        )


class QemuAccelTests(unittest.TestCase):
    """The harness accelerator default (issue #83 unblocked KVM)."""

    def choose(self, forced, kvm):
        env = {} if forced is None else {"AETHER_QEMU_ACCEL": forced}
        with patch.dict(os.environ, env, clear=False):
            if forced is None:
                os.environ.pop("AETHER_QEMU_ACCEL", None)
            with patch.object(qemu_accel, "kvm_available", return_value=kvm):
                return qemu_accel.choose_accelerator()

    def test_default_prefers_kvm_when_usable(self):
        self.assertEqual(self.choose(None, True), ("kvm", "host"))

    def test_default_falls_back_to_tcg_without_kvm(self):
        self.assertEqual(self.choose(None, False), ("tcg", "max"))

    def test_tcg_override_wins_over_usable_kvm(self):
        self.assertEqual(self.choose("tcg", True), ("tcg", "max"))

    def test_forced_kvm_without_device_is_an_error(self):
        with self.assertRaises(SystemExit):
            self.choose("kvm", False)


if __name__ == "__main__":
    unittest.main()

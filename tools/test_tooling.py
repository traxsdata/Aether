# SPDX-License-Identifier: Apache-2.0
"""Fast regression tests for artifact selection, VMX encoding and boot evidence."""

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from boot_support import BASE_MARKERS, find_firmware, find_iso, has_failure
from make_vmware_vmx import render, vmx_string
from desktop_smoke import parse_state, read_ppm


class ToolingTests(unittest.TestCase):
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
        for setting in ('firmware = "efi"', 'uefi.secureBoot.enabled = "FALSE"', 'mouse.vusb.enable = "FALSE"', 'keyboard.vusb.enable = "FALSE"', 'serial0.present = "TRUE"', 'mks.enable3d = "FALSE"'):
            self.assertIn(setting, text)
        for path in ('bad\npath', 'bad\rpath', 'bad\x00path'):
            with self.assertRaises(ValueError):
                vmx_string(path)

    def test_invalid_firmware_overrides_fail_before_qemu(self):
        with patch.dict(os.environ, {"OVMF_CODE": "/nonexistent/aether-code.fd"}, clear=True):
            with self.assertRaises(SystemExit):
                find_firmware()
        with patch.dict(os.environ, {"OVMF_VARS": "/nonexistent/vars.fd"}, clear=True):
            with self.assertRaises(SystemExit):
                find_firmware()


if __name__ == "__main__":
    unittest.main()

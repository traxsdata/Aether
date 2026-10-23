# SPDX-License-Identifier: Apache-2.0
"""Shared QEMU accelerator choice for the acceptance harnesses.

By default KVM is used when /dev/kvm is readable and writable by the
current user, otherwise TCG (software emulation). AETHER_QEMU_ACCEL
forces the choice (`tcg` or `kvm`); forcing `kvm` on a host without a
usable /dev/kvm is a hard error instead of a silent fallback, because
tests then report timings no one can attribute. With KVM the machine
runs `-cpu host` (a superset of the SMEP/SMAP features the kernel
enables); with TCG it keeps `-cpu max`.
"""

from __future__ import annotations

import os
import sys

KVM_DEVICE = "/dev/kvm"


def kvm_available() -> bool:
    """Whether the current user can use KVM at all."""
    return os.access(KVM_DEVICE, os.R_OK | os.W_OK)


def choose_accelerator() -> tuple[str, str]:
    """Return the (accelerator, cpu-model) pair honoring AETHER_QEMU_ACCEL."""
    forced = os.environ.get("AETHER_QEMU_ACCEL", "").strip().lower()
    if forced not in ("", "tcg", "kvm"):
        sys.exit(f"error: AETHER_QEMU_ACCEL must be tcg or kvm (got {forced!r})")
    if forced == "kvm" and not kvm_available():
        sys.exit(f"error: AETHER_QEMU_ACCEL=kvm but {KVM_DEVICE} is not usable by this user")
    if forced == "tcg" or not kvm_available():
        return "tcg", "max"
    return "kvm", "host"


def accel_args() -> list[str]:
    """The `-machine`/`-cpu` arguments for qemu-system-x86_64."""
    accel, cpu = choose_accelerator()
    return ["-machine", f"q35,accel={accel}", "-cpu", cpu]


def describe() -> str:
    """One-line description for harness output and PR evidence."""
    accel, cpu = choose_accelerator()
    return f"accel={accel} cpu={cpu}"

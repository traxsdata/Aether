# SPDX-License-Identifier: Apache-2.0
"""Shared QEMU accelerator choice for the acceptance harnesses.

AETHER_QEMU_ACCEL selects the accelerator explicitly: `tcg` or `kvm`.
KVM runs `-cpu host` (a superset of the SMEP/SMAP features the kernel
enables); TCG keeps `-cpu max`. Requesting `kvm` without a usable
/dev/kvm is a hard error instead of a silent fallback, because tests
then report timings no one can attribute.

The default is TCG. KVM is deliberately not the default yet: delivery of
a signal to a non-current task runs the handler with borrowed register
state and the linux cross-process SIGUSR1 test faults in ~40% of KVM
boots (issue #83, clean under TCG). Revert the default to KVM once that
kernel bug is fixed.
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
    if forced == "kvm":
        if not kvm_available():
            sys.exit(f"error: AETHER_QEMU_ACCEL=kvm but {KVM_DEVICE} is not usable by this user")
        return "kvm", "host"
    # Unset or "tcg": software emulation. Deliberately the default until
    # the kernel signal-delivery bug in #83 is fixed, where KVM boots fail
    # the linux cross-process SIGUSR1 test (see module docstring).
    return "tcg", "max"


def accel_args() -> list[str]:
    """The `-machine`/`-cpu` arguments for qemu-system-x86_64."""
    accel, cpu = choose_accelerator()
    return ["-machine", f"q35,accel={accel}", "-cpu", cpu]


def describe() -> str:
    """One-line description for harness output and PR evidence."""
    accel, cpu = choose_accelerator()
    return f"accel={accel} cpu={cpu}"

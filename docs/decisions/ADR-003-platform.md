# ADR-003: Initial hardware platform

- **Status:** Accepted for the prototype
- **Date:** 2026-10-19
- **Supersedes:** none

## Context

Supporting arbitrary PC hardware, GPUs, and laptops before the kernel is stable
would consume the project's effort and make failures hard to reproduce. A
reproducible virtual target is needed for CI and for contributors without
specific hardware.

## Decision

Target x86-64 PCs with UEFI firmware. The initial development platform is QEMU
with a documented configuration:

- `q35` machine, TCG acceleration for environments without KVM;
- 1 GiB RAM for the boot prototype (8 GiB is the early desktop target);
- VirtIO block for the boot image;
- OVMF firmware;
- serial output as the primary diagnostic channel.

Physical hardware support is added after the virtual platform is stable.

## Consequences

- Boot and kernel tests are reproducible in CI and need no privileged access.
- Hardware-specific drivers are deferred; the first release does not promise
  broad PC or laptop support.
- Firmware behaviour is still part of the trust boundary even under QEMU.
- The boot image format and QEMU command are documented and versioned with the
  code.

## Alternatives considered

- **BIOS/legacy boot:** smaller initial scope, but UEFI is the realistic modern
  platform and is required for the boot-integrity roadmap.
- **A single physical laptop as the reference platform:** rejected for the first
  sprint because it is not reproducible for contributors or CI.
- **ARM64 first:** viable later, but x86-64 has the widest QEMU tooling and is
  the initial desktop target.

# Changelog

All notable changes are recorded here. This project follows a prototype
versioning scheme until the kernel ABI stabilises.

## [Unreleased]

### Added

- Initial repository structure and contribution/security/governance documents.
- Pinned Rust 1.99.1 toolchain with `x86_64-unknown-none` and
  `x86_64-unknown-uefi` targets.
- `no_std` UEFI loader with a defensive ELF64 parser, page loading, memory-map
  conversion, and boot-services exit.
- `no_std` x86-64 kernel skeleton with a dedicated stack, COM1 serial output,
  hand-off validation, and panic reporting.
- Versioned `BootInfo` ABI shared by the loader, kernel, and host tests.
- QEMU/OVMF boot harness (`make test`) and local CI gate (`make ci`).
- Architecture, threat-model, and ADR documentation.

### Known limitations

- No scheduler, thread/task objects, user mode, capability tables, IPC,
  filesystem, storage driver, networking, graphics, or desktop.
- The kernel has no IDT: an unexpected exception resets the machine instead of
  producing a structured fault report.
- The kernel image is embedded in the loader, so updating the kernel requires
  rebuilding the loader and boot image.
- Single-core only; no SMP startup.
- No Secure Boot signing, measured boot, or update/recovery path yet.

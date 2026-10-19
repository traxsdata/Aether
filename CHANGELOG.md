# Changelog

All notable changes are recorded here. This project follows a prototype
versioning scheme until the kernel ABI stabilises.

## [Unreleased]

### Added

- `aether-memory`: host-testable bitmap frame allocator and x86-64 page-table
  encoding, with tests for reservations, exhaustion, double free, and layout.
- Kernel physical frame allocator over the validated boot memory map, with
  explicit kernel/hand-off reservations and deferred release of boot-services
  memory until after the CR3 switch.
- Kernel-owned four-level page tables: 4 KiB W^X mappings for kernel text,
  rodata, and data, plus a 2 MiB identity map for the rest of RAM.
- Flat kernel GDT and an IDT covering all 32 architectural exception vectors,
  with vector/error/RIP/RFLAGS/CR2 diagnostics and a failure marker.
- `make fault-test`: a boot-time `int3` self-test proving exceptions reach the
  kernel handler after paging is active.
- Capability, IPC, vision, security-boundary, release-policy, system-ABI, and
  development documentation.
- `docs/architecture/memory.md`.

### Changed

- Boot tests require both `AETHER_MM_OK` and `AETHER_BOOT_OK`.
- `make ci` runs the exception-path test in addition to the boot test.

### Known limitations

- No scheduler, thread/task objects, user mode, capability tables, IPC,
  filesystem, storage driver, networking, graphics, or desktop.
- Identity map only; no higher-half kernel or per-address-space isolation.
- No IST/TSS, so a double fault or stack overflow can still reset the machine.
- The kernel image is embedded in the loader, so updating the kernel requires
  rebuilding the loader and boot image.
- The frame bitmap covers at most 16 GiB.
- Single-core only; no SMP startup.
- No Secure Boot signing, measured boot, or update/recovery path yet.

## [0.1.0] - 2026-10-19

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

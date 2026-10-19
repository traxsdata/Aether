# Supported Hardware

**Status:** One virtual platform is supported and tested. No physical computer
is supported.

This list is deliberately narrow. Aether has a framebuffer, a PS/2 keyboard
driver, and a volatile RAM filesystem; it has no storage controller drivers, no
USB, no networking, no audio, no SMP, and no installer.

## Tested platform

| Component | Supported value |
|---|---|
| Machine | QEMU `q35`, TCG (KVM is not required) |
| CPU | `-cpu max` on x86-64 with long mode, NX, supervisor write-protect, and legacy PIC/PIT |
| RAM | 1024 MiB tested; the frame bitmap covers at most 16 GiB |
| Firmware | OVMF 4 MiB images (`OVMF_CODE_4M.fd`, `OVMF_VARS_4M.fd`) |
| Display | VGA/bochs-compatible adapter exposing a linear GOP framebuffer (`1280x800`) |
| Input | Emulated PS/2 keyboard, IRQ1 |
| Boot media | El Torito UEFI ISO or a 64 MiB FAT image with `/EFI/BOOT/BOOTX64.EFI` |
| Serial | 16550-compatible COM1 at `0x3f8` |

The CI harness uses:

```text
qemu-system-x86-64 -machine q35,accel=tcg -cpu max -m 1024M
  -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE_4M.fd
  -drive if=pflash,format=raw,file=OVMF_VARS_4M.fd
  -drive file=build/aether.img,format=raw,if=virtio
  -display vnc=127.0.0.1:9 -device VGA
  -serial file:build/serial.log -no-reboot
```

Firmware paths can be overridden with `OVMF_CODE` and `OVMF_VARS`.

## Required CPU features

- x86-64 long mode with four-level paging.
- `IA32_EFER.NXE`; the kernel refuses to boot without NX.
- `CR0.WP` support (all x86-64 CPUs used for testing).
- Legacy 8259 PIC and 8254 PIT (the test platform uses them directly).

APIC, IOMMU, SMEP, SMAP, and AVX are not used or required.

## Not supported

- Physical PCs, laptops, and servers: no installer and no storage or chipset
  drivers.
- KVM-specific acceleration requirements beyond TCG.
- UEFI Secure Boot in enforcing mode; the images are unsigned.
- Multiple CPUs; the scheduler is single-core.
- GPUs, accelerated graphics, and multi-monitor setups.
- USB keyboards and pointing devices.
- NVMe, AHCI, SCSI, or USB storage.
- Networking, Wi-Fi, Bluetooth, and audio devices.
- Removable media, suspend/resume, and power management.

## Reporting a new platform

A new platform is "supported" only when:

1. it boots the release image without firmware workarounds,
2. `make qualify`-equivalent markers pass on that hardware or VM,
3. the compatibility database records the exact configuration, and
4. this document lists it with the test date.

A boot that happens to reach a prompt is not sufficient.

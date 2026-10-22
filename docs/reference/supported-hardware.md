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
| RAM | 1024 MiB FAT-image tests; 2048 MiB ISO interaction tests; frame bitmap covers at most 16 GiB |
| Firmware | OVMF 4 MiB images (`OVMF_CODE_4M.fd`, `OVMF_VARS_4M.fd`) |
| Display | VGA/bochs-compatible and QEMU VMware SVGA adapters exposing linear GOP; both get automated ISO interaction tests at `1280x800`; smaller VGA modes have layout/manual coverage |
| Input | Emulated PS/2 keyboard (IRQ1) and PS/2 mouse (IRQ12) |
| Boot media | El Torito UEFI ISO or a 64 MiB FAT image with `/EFI/BOOT/BOOTX64.EFI` |
| Serial | 16550-compatible COM1 at `0x3f8` |

The same UEFI El Torito ISO is the intended VMware Workstation artifact. A
generated `.vmx` and the required firmware settings are in
[`vmware-workstation.md`](vmware-workstation.md). The ISO path is exercised by
`make iso-test` under OVMF, including real PS/2 input and VGA/SVGA color/screenshot
checks; a VMware Workstation smoke test is still required
before VMware can be listed as a tested platform.

The scripted ISO interaction suite has also been invoked with VGA EDID modes
800×600, 1024×768, and 1920×1200, including drag/resize, Files navigation/previews,
and screenshot equality. The default CI profiles remain VGA and VMware SVGA at
1280×800; broader display modes are not a hardware-support promise.

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

SMEP and SMAP are enabled when CPUID reports them, but are not required; the
boot log prints which are on. APIC, IOMMU, and AVX are not used or required.

## Not supported

- Physical PCs, laptops, and servers: no installer and no storage or chipset
  drivers.
- KVM-specific acceleration requirements beyond TCG.
- UEFI Secure Boot in enforcing mode; the images are unsigned.
- Multiple CPUs; the scheduler is single-core.
- GPUs, accelerated graphics, and multi-monitor setups.
- USB keyboards and pointing devices (PS/2 is required).
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

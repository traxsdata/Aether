# VMware Workstation

Aether ships a UEFI El Torito ISO. The same image that `make iso-test` boots
through OVMF can be attached to a VMware Workstation virtual machine; the ISO
carries an 8 MiB FAT EFI system image containing `/EFI/BOOT/BOOTX64.EFI`.

The kernel is embedded in the loader, so the ISO does not need a disk. Storage,
networking, audio, USB HID, and GPU acceleration are not used by this prototype;
do not add virtual hardware the kernel does not drive.

## Build

```sh
. "$HOME/.cargo/env"
make iso
make vmware-vmx
```

`make iso` writes a timestamped image under `build/ISOs/`. `make vmware-vmx`
writes `build/aether-vmware.vmx` with an absolute `cdrom-image` path to the
newest ISO.

## Open the generated VM

1. Install VMware Workstation with UEFI virtual-machine support.
2. Open `build/aether-vmware.vmx` (or create a custom VM from the ISO).
3. Confirm **Firmware: UEFI**, not BIOS. Aether has no BIOS boot path.
4. Disable **Secure Boot**. The prototype ISO is unsigned.
5. Use a PS/2 mouse and keyboard. `mouse.vusb.enable` and
   `keyboard.vusb.enable` are set to `FALSE` in the generated VMX because the
   prototype has no USB HID stack.
6. Start the VM. The top menu bar, Welcome window, and dock should appear.

If the VM starts in BIOS mode, the firmware will not find
`/EFI/BOOT/BOOTX64.EFI`. Change the VM's firmware setting to UEFI and boot the
ISO again.

## Manually creating the VM

| Setting | Value |
|---|---|
| Guest OS | Other 64-bit |
| Firmware | UEFI, Secure Boot off |
| Memory | 2048 MiB |
| CPUs | 2 (the kernel is currently single-core; extra CPUs are ignored) |
| Disk | None; attach the ISO as a CD/DVD image |
| CD/DVD | SATA or IDE, start connected |
| Video | 128 MiB, 3D acceleration off |
| Network | Disconnected or not present |
| Audio | Not present |
| USB | Not present |
| Pointer | PS/2 |

The generated `.vmx` sets these values.

## Framebuffer behaviour

VMware's virtual SVGA adapter reports a GOP mode through UEFI. The kernel
accepts any linear 32-bit GOP mode, composes into a 1920×1200 back buffer, and
falls back to direct framebuffer painting above that size. The layout adapts to
800×600, 1024×768, 1280×800, 1440×900, and 1920×1080; the work area is derived
from the visible framebuffer rather than a hard-coded QEMU resolution.

VMware typically exposes a BGRX framebuffer. The graphics service writes
`0x00RRGGBB` and treats both linear GOP format codes as the common BGRX memory
layout. This was verified with QEMU/OVMF screenshots; if a VMware firmware
reports the opposite byte order on a particular host, the theme colours will
appear swapped while the desktop remains usable. A framebuffer byte-order
self-test is follow-up work.

## Evidence and limits

`make iso-test` boots the generated ISO with OVMF and requires the loader,
kernel, graphics, desktop, window-manager, pointer, shell, and boot markers. CI
runs it in addition to the FAT-image boot test.

The repository's automated environment does not contain VMware Workstation, so
the VMware path is documented and the identical UEFI El Torito artifact is
tested under OVMF. A VMware-specific smoke test must be recorded before this
document is presented as a tested platform. Do not attach a virtual disk or
store real data: storage and the installer are not implemented.

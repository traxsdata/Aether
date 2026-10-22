# VMware Workstation

Aether ships a UEFI El Torito ISO. The same image that `make iso-test` boots
through OVMF can be attached to a VMware Workstation virtual machine; the ISO
carries a FAT EFI system image containing `/EFI/BOOT/BOOTX64.EFI` (8 MiB for
the standard fixture, 128 MiB for the GridCraft demo).

The GridCraft demo needs room for a 1.5 GiB import heap in addition to the
164 MiB runtime rootfs and the imported AppImage. The kernel now reserves that
heap from its frame allocator after it takes over, so the UEFI loader only
allocates the small kernel image. Give the demo VM at least 4096 MiB; the
generated VMX uses 8192 MiB. A 2048 MiB guest is too small for the demo and can
fail during import. The standard fixture ISO remains usable with 2048 MiB.

The kernel is embedded in the loader, so the ISO does not need a disk. Storage,
audio, USB HID, and GPU acceleration are not used by this prototype; do not add
virtual hardware the kernel does not drive. Networking uses one Intel e1000e
adapter on NAT (see [Networking](#networking)).

## Build

```sh
. "$HOME/.cargo/env"
make iso
make iso-test
make vmware-vmx
```

For the GridCraft demo, build `make iso-gridcraft` instead of `make iso`, then
run `make vmware-vmx` to generate an 8 GiB VMX pointing at the newest ISO.

To run the Craft suite, build `make iso-craft-apps`. The ISO packs every
published Craft app; GridCraft starts automatically and the Applications tile
in the dock (or F4) opens the in-desktop launcher. Arrow keys select an app and
Enter launches it. Two app domains can be open at once, and the dock keeps the
running indicator for the active windows.

`make iso` writes a timestamped image under `build/ISOs/`. `make vmware-vmx`
writes `build/aether-vmware.vmx` with an absolute `cdrom-image` path to the
newest ISO. The VMX target does not rebuild it, so the image you just tested is
the image attached to the VM. To use a specific existing artifact:

```sh
AETHER_ISO=/absolute/path/aether.iso sh tools/make-vmware-vmx.sh
```

If you copy the ISO/VMX to a different host (for example from a Linux build host
to Windows Workstation), update `ide1:0.fileName` and `serial0.fileName` to paths
on that host, or create the VM manually using the table below.

## Open the generated VM

1. Install VMware Workstation 17 or later (the VMX uses hardware version 20).
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
| Memory | 8192 MiB for the GridCraft demo; 2048 MiB for the standard ISO |
| CPUs | 2 (the kernel is currently single-core; it parks extra CPUs with an INIT IPI at boot) |
| Disk | None; attach the ISO as a CD/DVD image |
| CD/DVD | SATA or IDE, start connected |
| Video | 128 MiB, 3D acceleration off |
| Network | One adapter, NAT, type e1000e (`ethernet0.virtualDev = "e1000e"`) |
| Audio | Not present |
| USB | Not present |
| Pointer | PS/2 |

The generated `.vmx` sets these values, limits the advertised display to
1920×1200 for the buffered path, disables the enhanced/USB pointer, and captures
COM1 in `build/aether-vmware-serial.log`.

Do not enable the VMware absolute pointing device or a USB mouse. Aether drives
the legacy PS/2 auxiliary port (IRQ12); the loader also initializes the mouse
sample rate and resolution. With an enabled `vmmouse` or USB pointer, Workstation
can present a device the prototype does not drive, so the cursor appears stuck.
Keep `vmmouse.present = "FALSE"`, `mouse.vusb.enable = "FALSE"`, and
`usb.present = "FALSE"`.

## Framebuffer behaviour

VMware's virtual SVGA adapter reports a GOP mode through UEFI. The kernel
accepts validated linear 32-bit GOP modes, uses a 1920×1200 back buffer plus a
wallpaper cache, repaints damaged regions, and uses a small cursor-only path.
Modes above that buffer size fall back to slower direct framebuffer painting.
The layout adapts to
800×600, 1024×768, 1280×800, 1440×900, and 1920×1080; the work area is derived
from the visible framebuffer rather than a hard-coded QEMU resolution.

The loader asks GOP for the largest 4:3-or-wider mode within its 1920×1200
back-buffer bound, then reads the actual stride (including padding) from the
standard 36-byte mode-information structure and rejects short
descriptors/apertures. Firmware that leaves the console at 1024×768 is upgraded
when its mode table offers something larger.
Boot ABI 3 follows UEFI: `0 = RGBX`, `1 = BGRX`. The graphics service converts
its `0x00RRGGBB` colors for the reported format; it does not assume that all
firmware is BGRX. Host tests exercise both formats and padded scan lines; the
ISO tests also check screenshot colors. Dimensions/stride are bounded to 8192
pixels and mapped apertures to 512 MiB. Rebuild loader and kernel together.

GOP exposes no vblank or page-flip API. Buffered drawing avoids showing each
intermediate paint operation, but the final copy is not atomic/guaranteed
tear-free and is not accelerated by VMware 3D settings.

## Evidence and limits

`make iso-test` boots the same generated ISO with OVMF on VGA and QEMU's VMware
SVGA adapter. It requires all boot/security markers, then drives real PS/2
window movement/resizing, Files navigation/previews, private-vault denial,
menus/search, and the cursor-only path. It compares damaged/full screenshots,
checks colors, and reports redraw times. Logs and screenshots are under
`build/iso-test/`; the artifact path and digest are in `artifact.json`.

The repository's automated environment does not contain VMware Workstation, so
the VMware path is documented and the identical UEFI El Torito artifact is
tested under OVMF. A VMware-specific smoke test must be recorded before this
document is presented as a tested platform. Do not attach a virtual disk or
store real data: persistent storage and the installer are not implemented.

## Workstation smoke-test checklist (still required)

Record the Workstation version, host OS, ISO SHA-256, dirty/commit identifier,
VMX settings, GOP mode/stride/format, and the COM1 transcript. Confirm:

1. `AETHER_BOOT_OK`, `AETHER_FILES_OK`, `AETHER_WM_OK`, `AETHER_POINTER_OK`,
   `AETHER_INPUT_OK`, and the other required markers appear with no failures.
2. Close/minimize/restore/zoom, title-bar drag, corner resize, Spaces, search and
   menus work using both the physical keyboard and grabbed PS/2 pointer.
3. Files opens Home and `welcome.txt` (double-click or Enter), Backspace returns,
   refresh works, and private app data is denied.
4. Traffic-light/accent colors are correct, corners remain rounded, and moving
   a window/cursor leaves no trails. Capture `desktop` render timings and a
   screenshot. Do not use QEMU timings as a VMware performance promise.

If a machine stops in firmware, check UEFI, Secure Boot, connected CD/DVD, and
the ISO path first. If it reaches the desktop but is sluggish, keep 3D off,
use at most 1920×1200, and inspect `desktop`/`input` counters and the serial log.

## Networking

The generated VMX gives the VM one Intel 82574L (`e1000e`) adapter on VMware's
NAT network. At boot the serial log shows the adapter, the link, and the DHCP
lease:

```text
net: 82574L 8086:10d3 at 03:00.0 mac=00:0c:29:..
AETHER_NET_LINK_OK model=82574L
AETHER_NET_DHCP_OK address=192.168.x.y/24 gateway=192.168.x.2 dns=192.168.x.2
```

The older `e1000` (82545EM) type also works. `vmxnet3` is not supported yet;
with it, or with no adapter, the system runs offline. The `net` command on the
serial console prints the address and each app's network answer.

Nothing can connect in: the kernel drops unsolicited inbound traffic and does
not answer pings. An app gets the network only if its signed record declares
`network=client` and you choose **Allow** when Security Center asks.

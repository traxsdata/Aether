# Running æther in VMware Workstation

There's a ready-made ISO attached to each release in the project thread if you want to see it boot
before setting up a toolchain. Skip to **Part 2** for that.

## Part 1: Build on your PC

The toolchain is plain clang/LLVM, so it builds the same everywhere. No
custom cross-compiler build is needed: clang targets UEFI (`x86_64-unknown-windows`)
and bare-metal ELF (`x86_64-unknown-elf`) out of the box.

### Windows (VMware Workstation's usual home): use WSL2

1. Open PowerShell as Administrator and run `wsl --install -d Ubuntu`, then reboot
   and finish the Ubuntu user setup.
2. In the Ubuntu terminal:
   ```sh
   sudo apt update
   sudo apt install -y clang lld make mtools xorriso qemu-system-x86 ovmf git
   ```
3. Clone the project inside WSL (faster builds than `/mnt/c`), then build:
   ```sh
   git clone https://github.com/traxsdata/Aether ~/aether
   cd ~/aether
   make
   ```
4. Copy the ISO somewhere Windows can see it:
   ```sh
   mkdir -p /mnt/c/Aether && cp build/aether.iso /mnt/c/Aether/
   ```
   (`\\wsl$\Ubuntu\home\<you>\aether\build\aether.iso` also works directly from Explorer.)

### Linux

Same `apt install` line as above (or your distro's equivalents), then `make`.

### Quick check before VMware

`make run` boots the ISO in QEMU. If you see the desktop with a Terminal,
the build is good. (On WSL this needs WSLg for the window; `make test`
runs headless instead and prints PASS/FAIL.)

## Part 2: Create the VM (one time)

VMware Workstation 17 (Pro is free for personal use). Menu names below are
from 17.x; older versions are close.

1. **File > New Virtual Machine... > Custom (advanced)** > Next.
2. Hardware compatibility: leave the default > Next.
3. **I will install the operating system later** > Next.
4. Guest operating system: **Other**, Version: **Other 64-bit** > Next.
5. Name: `æther` > Next.
6. Firmware type: **UEFI**. Leave **Secure Boot unchecked** (our loader isn't
   signed). > Next.
7. Processors: 1 processor, 1 core is plenty for now > Next.
8. Memory: **2 GB** or more > Next. (The Craft apps can each use up to 1 GB;
   if you made the VM for an older æther, raise it in VM > Settings > Memory.)
9. Network: **Use network address translation (NAT)** > Next. æther 0.5
   gets its address by DHCP from VMware's NAT network. (If you don't want
   networking, pick **Do not use a network connection**.)
10. I/O controller: default. Virtual disk type: **SATA** > Next. (æther
    drives SATA disks; it can't see NVMe or SCSI disks yet.)
11. Disk: **Create a new virtual disk**, 4 GB or more, single file > Next >
    Next > Finish. æther encrypts this disk with a passphrase you choose on
    the first boot.

Then attach the ISO and a serial log:

12. **VM > Settings > CD/DVD (SATA)**: **Use ISO image file** > Browse to
    `aether.iso`. Make sure **Connect at power on** is ticked.
13. Still in Settings: **Add... > Serial Port > Use output file** >
    `C:\Aether\serial.log` > Finish. Everything the kernel prints is also
    written here, which is handy when the screen isn't enough.
14. OK, then **Power on this virtual machine**.

You should briefly see `æther bootloader` and a few boot lines. On the
first boot with a blank disk, æther asks you to choose a passphrase (8+
characters, typed twice) and encrypts the disk; on later boots it asks for
the passphrase. Then the desktop opens with the Terminal.

### Networking (0.5)

æther drives Intel PRO/1000 cards, which is what VMware gives an "Other
64-bit" guest (the `e1000` or `e1000e` adapter). Type `net` in the Terminal:
it should show an address like `192.168.x.128` within a few seconds. If it
says there's no supported network card, the adapter is probably `vmxnet3`:
power off and set it in the `.vmx` file:
```
ethernet0.virtualDev = "e1000e"
```
If the firmware now tries network (PXE) boot before the CD, press **Esc**
during boot, or put the CD first: **VM > Power > Power On to Firmware >
Boot Maintenance Manager > Boot Options > Change Boot Order**.

### Already have the VM from an older æther?

If it has no network adapter: **VM > Settings > Add... > Network Adapter >
NAT** > Finish.


Its disk is probably NVMe or SCSI, which æther can't see yet, so it runs
with temporary storage. To keep files: **VM > Settings > Add... > Hard
Disk > SATA > Create a new virtual disk** (4 GB) > Finish. The new disk is
blank, so æther offers to set it up on the next boot. æther never formats a
disk that already has data on it. Click inside the VM window to give it the
mouse and keyboard; press **Ctrl+Alt** to get the mouse back.

If you'd rather edit the `.vmx` file, the important lines are:
```
firmware = "efi"
uefi.secureBoot.enabled = "FALSE"
guestOS = "other-64"
memsize = "2048"
ethernet0.present = "TRUE"
ethernet0.connectionType = "nat"
ethernet0.virtualDev = "e1000e"
```

## Part 3: The edit-build-test loop

1. Edit code.
2. `make` (in WSL/Linux). Re-copy the ISO if VMware reads it from `C:\Aether`.
3. In VMware: **VM > Power > Reset** (the ISO is re-read on each boot).

Tip: run `make run` in QEMU for most iterations (it's faster), and use VMware
to confirm on a second, different "machine".

## Troubleshooting

| You see | Meaning / fix |
|---------|---------------|
| Firmware boot menu, or "No bootable device" | CD/DVD not connected, or firmware is BIOS. Check step 6 and 12. In the boot manager pick the SATA CD-ROM entry. |
| Long pause then "PXE... no media" | The firmware tries network boot before the CD. Put the CD first (see "Networking"), or just wait for it. |
| `net` says no supported card | Set `ethernet0.virtualDev = "e1000e"` in the `.vmx` (see "Networking"). |
| `net` says "waiting for DHCP" | VM Settings > Network Adapter: check **Connected** and **NAT**. |
| "Secure Boot violation" | Untick Secure Boot: VM Settings > Options > Advanced. |
| Mouse doesn't move | Click inside the VM window first so it captures the mouse (Ctrl+Alt releases it). |
| "temporary files" in the top bar | No blank SATA disk was found. Add one (see "Already have the VM"). |
| "Wrong passphrase" | Type it again. There's no recovery: a forgotten passphrase means the files can't be read, by design. To start over, delete that virtual disk and add a new blank one. |
| Red ":(" crash screen | A kernel bug. Send a photo of it plus `serial.log` in the thread. |
| `[boot] ERROR: ...` anything else | The message names the step that failed; send it in the thread with the serial log. |
| Black screen after "starting..." | Check `serial.log`. If it shows the kernel output, it's a framebuffer issue; if empty, the kernel crashed before `serial_init`. |

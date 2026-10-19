# Installing æther 1.0

æther 1.0 has two boot media:

| Image | What it is |
|---|---|
| `aether-netboot-x86_64.iso` | A small, deliberately stable **installer**. It runs from RAM, leaves the target disk untouched until you press Install, and downloads the boot/system images from the latest GitHub release. |
| `aether-x86_64.iso` | The live desktop (the same kernel without the installer marker). Useful for testing; not the install path. |

The installed system has an A/B update path. It checks
`github.com/traxsdata/Aether/releases/latest`, downloads the **system**
image, checks the release's SHA-256 digest and the image's own payload hash,
and writes it to the slot that is not running. A reboot tries it. The running
slot and the encrypted data volume are never modified by an update.

## Flashing the netboot ISO

The ISO is hybrid and can be written to a USB stick as a raw image:

```sh
sudo dd if=aether-netboot-x86_64.iso of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

Use the whole device (`/dev/sdX`), not a partition. Secure Boot must be off;
æther's loader is not signed yet. Pick the USB device in the firmware boot
menu. On a machine with an existing ESP, the aether loader also adds a
firmware boot entry named **aether** the first time it boots an installed
system, so later boots do not need the USB stick.

## What the installer writes

The chosen disk is erased. The 0.8 layout is:

| Partition | Size | Contents |
|---|---|---|
| ESP | 64 MiB | FAT image with `\EFI\BOOT\BOOTX64.EFI` |
| boot control | 1 MiB | three CRC-checked copies of the A/B state |
| system A | 256 MiB | raw system image: kernel + system app packages |
| system B | 256 MiB | inactive slot / rollback target |
| data | the rest | encrypted AFS volume (your files) |

The ESP and the boot-control record are deliberately small and stable. The
system slots are what GitHub releases update.

### The passphrase

The installer asks for a passphrase of at least 8 characters. It is used the
same way as the first-boot lock screen: PBKDF2-SHA256 derives a key-encryption
key, which unwraps a random AFS volume key. The passphrase is copied into the
kernel request, used for the format, and wiped; it is never written anywhere.

If the passphrase is lost, the files on the data partition cannot be
recovered. Reinstalling æther preserves nothing. Once installed, Settings >
Security can change the passphrase without re-encrypting the files: the
random volume key is re-wrapped under the new one.

## Installing

1. Boot the netboot ISO with a supported network adapter and the target disk
   attached.
2. Choose the disk. Disks that already contain æther are marked; choosing one
   replaces both system slots and formats the data partition.
3. Enter the disk passphrase twice (the installer refuses a mismatch) and
   press **Install æther**.
4. The installer downloads `aether-boot-x86_64.img` and
   `aether-system-x86_64.img`, verifies both against GitHub's release digest,
   and hands them to `SYS_SYSTEM_INSTALL`. The kernel checks the system
   image's header, ranges, SHA-256 and RSA signature again, and requires the
   boot image's SHA-256 to match the hash inside that signed header. A boot
   image from a different download is refused.
5. When it says **Installed**, remove the netboot medium and reboot. Enter the
   passphrase at the lock screen.

No AFS volume is opened during installation. The netboot kernel runs on a
temporary RAM volume, and the target data partition is formatted only after
both images have passed their checks.

## Updates

Open **App Store → OS** on the installed system.

- **Check for OS updates** asks GitHub for the latest release.
- **Install …** downloads the system image and writes it to the inactive
  slot. It does not run the new kernel immediately.
- **Reboot** starts the new slot.

The loader records a boot attempt in the boot-control partition. If the new
kernel does not reach the 0.8 kernel/storage initialisation path twice, the
loader reverts to the last slot that did. Once the kernel is up, it marks the
slot good. An update never touches the AFS data partition or the running slot.

The system image is field-updatable. The UEFI loader in the ESP is not; a
loader change is a deliberate netboot/channel change. That keeps the
network-facing and disk-writing code in the installer small and stable.

## Release channels

- **OS releases** are normal tags such as `v0.8.0`. Their assets are
  `aether-system-x86_64.img`, `aether-boot-x86_64.img` and
  `aether-x86_64.iso` (plus `SHA256SUMS`).
- **Netboot releases** are pre-releases tagged `netboot-v*`. They are built
  only when the loader or installer changes, so the bootstrap ISO does not
  churn with every OS release. GitHub's `releases/latest` excludes
  pre-releases, so the installer still sees the latest OS release.

Both are built by GitHub Actions; [RELEASING.md](RELEASING.md) has the
signing-key setup and the steps for cutting a release.

GitHub's release asset `digest` is required. A release without `sha256:`
digests is refused rather than installed unverified. The system image itself
carries the RSA signature made with the offline release key.

## Security status

- System images are **RSA-2048 PKCS#1 v1.5 signed**. The public modulus in
  `common/release_key.h` is the repository owner's release trust anchor. The
  private key is offline, held by the owner and stored only in the protected
  `release` environment's `AETHER_SIGNING_KEY` secret. The loader verifies the
  signature and compares the image digest with the hash recorded in boot
  control; the kernel repeats both checks before writing an update. GitHub's
  asset digest is an additional transport check, not the trust anchor.
- The installer and App Store hold `CAP_UPDATE`. The Terminal does not.
  Destructive install/update requests open a kernel-drawn confirmation dialog;
  the requesting app cannot answer it.
- The loader reads the whole image once, hashes and verifies that buffer, and
  loads code and apps from the same bytes.
- The signed system image pins the boot image's SHA-256, so a fresh install
  cannot pair a signed kernel with an unrelated bootloader.
- There is no Secure Boot signature yet. You must turn Secure Boot off.

## Network and hardware support in 1.0

The installer's download uses the kernel's networking stack. In 1.0 that
stack has the **Intel PRO/1000 / 82574L wired driver** (`e1000`/`e1000e`)
used by QEMU, VMware and many desktops, **CDC-ECM USB Ethernet**, the Rust
**NVMe** driver, and an **I²C-HID touchpad/pen** driver for the Yoga's LPSS
I2C controllers. A laptop with only Wi-Fi (including the AX211 in the Yoga 9
14IAP7) still needs a Wi-Fi driver or a USB Ethernet dongle before the
netboot path can run on it. See [DRIVERS.md](DRIVERS.md).

# Review of the 0.8 work

This is a working review of the 0.8 branch. It starts from the merged 0.7
`main` (`bf0ddb0`) and adds the netboot installer, a writable A/B system
layout, self-updates from GitHub, and the first ring-0 driver written in Rust.

## What 0.8 changes

### Netboot installer

`build/aether-netboot.iso` is a separate, small bootstrap image:

- its ESP contains the UEFI loader, a live kernel and a `netboot` marker;
- the `netboot` marker makes the kernel use temporary RAM storage and start
  the installer instead of the Terminal;
- the target disk is not opened, probed or formatted until the user presses
  **Install**;
- the installer fetches `aether-boot-x86_64.img` and
  `aether-system-x86_64.img` from the latest normal GitHub release, requires
  GitHub's SHA-256 asset digest, and hands them to the kernel.

The ISO is built only on a deliberate `netboot-v*` pre-release tag. OS
releases build the system/boot assets, not a new netboot ISO, so the
bootstrap does not change with every kernel or app change.

### Disk layout and updates

The installer writes a GPT with:

| Partition | Size | Type |
|---|---|---|
| ESP | 64 MiB | EFI System |
| boot control | 1 MiB | `A0E7B001-…` |
| system A | 256 MiB | `A0E7B002-…` |
| system B | 256 MiB | `A0E7B003-…` |
| data | the rest | `A0E7B004-…` |

The ESP is a FAT image with `\EFI\BOOT\BOOTX64.EFI`. The system slots hold a
raw `AETHERS1` image (kernel + system app packages) with a SHA-256 over the
payload. The boot-control partition holds three CRC32-checked records with a
generation number. The loader:

1. reads the newest valid boot-control copy;
2. increments `attempts` when a pending slot is being tried;
3. after two failed attempts, reverts to the last good slot;
4. validates the system image header and whole payload hash;
5. loads the kernel and each app package into loader pages, then starts the
   kernel.

The kernel marks the running slot good after the storage path is up and before
the lock screen waits for a passphrase. A user who leaves the machine at the
lock screen therefore does not cause a rollback. A kernel that cannot reach
that point is rolled back on the next two boots.

The installed system's App Store OS tab downloads the system image and calls
`SYS_UPDATE_INSTALL`. The kernel writes the inactive slot and updates the
boot-control record; it never writes the running slot or the AFS data
partition. The UEFI loader itself is not rewritten by an OS update.

### Image signatures and destructive-action gating

- System images carry an RSA-2048 PKCS#1 v1.5 signature. The canonical digest
  covers the header (with the digest/signature fields zeroed) and the payload;
  the loader and kernel both verify it against the repository owner's public
  modulus in `common/release_key.h`. The loader also compares the digest with the hash
  recorded in boot control, and checks the image's numeric version against a
  monotonic `min_version` floor.
- The private key is offline and never committed. `tools/make-signing-key.py`
  creates/rotates it; `tools/mksysimage.py` refuses to sign with a key that
  does not match the compiled public key.
- `SYS_SYSTEM_INSTALL` is netboot-only. `SYS_UPDATE_INSTALL` and
  `SYS_SYSTEM_INSTALL` open a kernel-drawn confirmation dialog; the requesting
  app cannot answer it. Terminal no longer holds `CAP_UPDATE`.

### Rust NVMe driver

`drivers/nvme` is a `no_std` Rust static library linked into the kernel. It
covers controller reset, admin queues, Identify Controller/Namespace, I/O
queue creation, read/write/flush and registration as a `struct blkdev`.

Security properties:

- no allocator or Rust runtime; only `pmm_alloc_contig` pages are used for
  DMA, and they are zeroed and owned by the driver;
- CAP/MQES and doorbell stride are checked; queue depth is fixed at 64;
- every LBA range, transfer size, page count and PRP list index is checked;
- the PRP list is one owned page; the bounce buffer is 64 contiguous pages;
- the completion queue phase bit and status are checked;
- all polls have a PIT-based 15 s deadline;
- the release profile uses `overflow-checks = true`, `panic = "abort"` and a
  panic handler that only stops the CPU;
- `unsafe` is restricted to MMIO and the narrow C ABI; the crate is
  `#![forbid(unsafe_op_in_unsafe_fn)]`.

The C side is `kernel/drivers.c` (MMIO mapping, log, panic-free helpers) and
the build is `tools/build-kernel-drivers.sh` with the bare-metal
`x86_64-unknown-none` target.

### Installer UI and ABI

- `CAP_UPDATE` is a new capability held by Terminal, App Store and the
  netboot installer. Ordinary apps never get it.
- New calls: `SYS_DISK_LIST`, `SYS_SYSTEM_INSTALL`, `SYS_UPDATE_INFO`,
  `SYS_UPDATE_INSTALL`, `SYS_REBOOT`.
- The installer has an Install tab in netboot mode, a masked passphrase field,
  disk selection, GitHub check and install/reboot actions. It is usable with
  the keyboard alone (Tab, arrows, Enter), which matters on a laptop whose
  touchpad is not yet supported.
- The OS tab now installs into the inactive slot instead of saving an ISO.
- `os_info` keeps its 0.7 layout and size; the new fields live in
  `update_info`, so old native apps do not receive a larger struct than they
  allocated.

## Trust boundaries and checks

- **Installer input**: passphrase is copied into a kernel request, checked
  for 8..127 bytes, used for PBKDF2/AFS format, and wiped on every exit path
  (including allocation failures).
- **Images**: the app checks GitHub's digest; the kernel checks the system
  image magic, format, header size, image size, kernel range, each app range,
  and the payload SHA-256. It also checks that the boot image is a plausible
  FAT boot sector before writing it to the ESP.
- **Disk targets**: `SYS_SYSTEM_INSTALL` requires `CAP_UPDATE`, a valid whole
  disk index, at least 1 GiB, and a system image that fits the fixed slot.
  `SYS_UPDATE_INSTALL` additionally requires that the running system was
  booted from an A/B slot.
- **Boot state**: each record is 512 bytes with a CRC32 over the first 508
  bytes; invalid copies are ignored, the newest valid generation wins. The
  loader and kernel write all three copies.
- **GPT**: the parser validates header CRC, entry CRC, LBA bounds, entry size
  and count, and falls back to the backup table. The writer orders entries
  before headers and writes the primary and backup copies.
- **AFS install-time format**: `afs_format_partition` snapshots the live AFS
  state, formats the target through the normal format path, restores the live
  state, and drops every cache key so target blocks cannot collide with live
  plaintext. The installed data volume was unlocked on the next boot in the
  QEMU test.

## Evidence

Run on this machine (QEMU TCG; no KVM):

```text
make test                                      PASS
make unittest                                 54 kernel / 66 terminal, 0 failed
cargo test (system-ui, aether-host, eframe,
            installer, webnet, browser)        192 passed, 0 failed
```

QEMU NVMe:

- installed layout on `-device nvme` booted through the loader;
- Rust driver logged reset, admin queue, identify, I/O queues, `nvme0:
  4194304 sectors`;
- the kernel parsed the GPT from NVMe, formatted the encrypted data partition,
  unlocked it, and started the Terminal;
- an A/B update wrote the inactive slot, and the loader booted it;
- with a corrupted inactive slot, the loader fell back to slot A and the
  kernel marked it good.

Netboot install and self-update were exercised with temporary test packages
built around the same `SYS_SYSTEM_INSTALL` / `SYS_UPDATE_INSTALL` calls. A
blank 2 GiB disk was installed from the netboot ISO, booted, unlocked with the
chosen passphrase, and then updated to a second system image in slot B. The
temporary harness is not committed; the production installer's UI was booted
separately and is shown by the 0.8 screenshots/tests.

## Deliberate gaps

- **No Secure Boot signature.** System images are publisher-signed and
  verified, but the loader/kernel themselves are not Secure Boot signed.
- **Key management.** The private release key is offline and must be added to
  the `release` environment's `AETHER_SIGNING_KEY` secret; the release workflow
  fails rather than publishing unsigned images if it is missing. Setup steps
  are in [RELEASING.md](RELEASING.md).
- **No hardware validation.** The NVMe driver is QEMU-tested; the Yoga's SK
  hynix drive has not been run by this environment.
- **No network driver for the target laptop.** 0.8 has the emulated Intel
  e1000/e1000e wired driver only. The Yoga has AX211 Wi-Fi and no wired port,
  so netboot/self-update cannot run on that laptop until USB Ethernet or Wi-Fi
  support lands. See [DRIVERS.md](DRIVERS.md).
- **No USB xHCI, I²C-HID touchpad, ACPI battery/lid/backlight, camera, audio,
  Bluetooth, Thunderbolt or fingerprint support.** The internal Yoga keyboard
  is i8042/PS-2 and is expected to work.
- **Loader updates are manual.** A new `BOOTX64.EFI` requires the netboot
  channel; the A/B flow updates the system image only.
- **The ESP itself is not A/B.** If its FAT is damaged, recovery is the
  netboot USB stick.
- **Large images need contiguous physical pages.** `SYS_UPDATE_INSTALL`
  allocates the image contiguously; the current system image is small, but a
  much larger future image may need a streaming write syscall.
- **No GPT host/unit test.** The C parser/writer is exercised through QEMU
  boot and install, and `tools/mkdisk.py` is an independent implementation
  used to cross-check the layout.
- **NVMe error recovery is conservative.** A timeout disables the controller
  and fails later I/O rather than attempting a reset; recovery requires a
  reboot.

## Review follow-up (changes requested at 653f13d)

The blocking and should-fix items from the review were addressed:

1. **Install gate.** `SYS_SYSTEM_INSTALL` now requires `BOOT_FLAG_NETBOOT`;
   the installed data partition is never a valid target. Install/update both
   open a kernel-drawn confirmation dialog.
2. **Origin and steering.** Images are RSA-signed; the canonical header is
   hashed, the signature is verified in loader and kernel, the loader compares
   the digest with boot control, and boot control carries a monotonic version
   floor. The loader reads the image once and loads from that buffer (no
   hash-then-reread TOCTOU).
3. **CAP_UPDATE.** Removed from Terminal; App Store and the netboot installer
   retain it, and every destructive operation is confirmed by the kernel.
4. **Slot numbering.** Loader and kernel use fixed A=0/B=1 with presence flags
   and reject overlapping/out-of-range GPT entries. A SYSTEM_B-only test booted
   slot 1 and the kernel marked slot 1 good.
5. **Disk identity.** The loader passes the boot ESP's GPT partition GUID and
   the disk GPT GUID to the kernel; the kernel only uses the matching disk.
   A two-disk QEMU test booted from the second disk and opened `sata1p5`, not
   `sata0p5`.
6. **NVMe PRP2.** Two-page transfers use the second data page directly; larger
   transfers use the PRP list.
7. **NVMe completions.** The CID is checked; a timeout disables the controller
   and makes later I/O fail.
8. **NVMe metadata.** Namespaces with separate metadata or non-512-byte LBAs
   are rejected.

Also fixed: MQES overflow, Rust panic routing to the kernel panic screen,
boot-control write failure falls back to the good slot, boot-control partition
size check, GPT entry bounds/overlap checks, NUL-checked header strings, BLK_*
error codes, AFS install table cleanup, duplicate partition registration,
CAP.MPSMIN/CAP.CSS/CSTS.CFS and I/O-BAR checks, controller/bus-master disable
on init failure, and a pinned Rust toolchain. Release workflow actions are
pinned by SHA and the workflow uses a protected `release` environment.

Additional evidence after the fixes:

- signed system image booted on QEMU NVMe;
- a payload byte flip was rejected with `system image digest mismatch`;
- a self-update through the kernel confirmation dialog wrote slot B and the
  next boot marked slot 1 good;
- a SYSTEM_B-only disk booted as slot 1 and was marked good;
- two installed disks booted from disk B and used disk B's data partition;
- `make test`, `make unittest` (54/66), `make test-nvme` and 192 Rust tests
  pass with the signing key.

## Round 2 review follow-up

The second review's blockers and should-fix items were addressed:

1. **Rollback floor.** Staging an update no longer raises `min_version`.
   `update_boot_good` sets the floor to the version of the slot that actually
   reached the kernel/storage path. A failed new slot falls back to the old
   slot and the old slot boots. This was tested with a corrupted pending slot
   and the floor at the old version.
2. **Owner release key.** `common/release_key.h` is now the repository owner's
   public key. The private key is not in the branch; it belongs only in the
   protected `release` environment's `AETHER_SIGNING_KEY` secret. `make`
   (live ISO) still builds without it; `make images` fails clearly until a
   key matching the header is supplied.
3. **Loader re-read.** The second header read must match the first
   `image_size`, so the single-image buffer cannot be hashed with a different
   length than it was read with.
4. **RSA size guard.** The verifier rejects anything that is not a full
   2048-bit modulus with the top bit set, instead of silently mis-reducing.
5. **Trust-anchor dependency.** Boot objects now depend on
   `common/release_key.h`, so changing the key rebuilds the loader without a
   clean build.
6. **NVMe checks.** CSTS.CFS is checked at init and before each command; the
   I/O BAR is detected through `pci_bar_ex`; metadata/LBA logs are distinct.
7. **Confirmation text.** The kernel dialog now names the requesting app and,
   for install, the target disk name, model and size.
8. **CI.** The signing key is written only for the signing step and removed by
   a trap; `netboot.yml`'s manual dispatch is restricted to `main`.
9. **Documented limitation.** `docs/SECURITY.md` states that the CRC32-only
   boot-control sector cannot resist an attacker with raw disk write access;
   TPM/RPMB or signed boot state would be needed.

Evidence for the round-2 changes was run with a throwaway signing key that was
not committed, then the owner's header was restored:

```text
make test                                      PASS
make unittest                                 54 kernel / 66 terminal, 0 failed
make test-nvme                                 PASS
cargo test (all runner packages)               192 passed, 0 failed
```

Rollback test: a pending 0.8.1 slot with a corrupted header fell back to the
0.8.0 slot, which booted and marked itself good with `min_version` 0x800.

## Review conclusions

The 0.8 slice has a working install and update path with a clear trust
boundary and a real Rust driver. It does not yet meet the "physical Yoga
laptop installs and updates itself" goal because the network/touchpad driver
slice is still missing; the docs and roadmap say so explicitly rather than
calling QEMU evidence hardware support.

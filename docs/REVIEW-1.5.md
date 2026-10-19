# Review of 1.5: USB storage

Based on `v1.4`. The 1.5 plan lists Wi-Fi, audio, battery/lid, cameras,
Bluetooth, USB storage, suspend/resume and thermal policy. These need either
vendor firmware or an ACPI AML interpreter. This slice delivers the part that
fits the existing xHCI transport and can be tested in QEMU: USB mass storage.

## Changes

- `kernel/usb.c` now recognises mass-storage interfaces (class 8, subclass 6,
  protocol 0x50) and configures their bulk IN/OUT endpoints. The descriptor
  walker only selects a configuration it can drive; CBI/UAS devices are
  ignored rather than guessed at.
- `kernel/usb_storage.c` implements bulk-only transport: one CBW, an optional
  data stage split into 4 KiB transfers, and a CSW checked by signature and
  tag. SCSI commands cover readiness, capacity, read, write and cache flush.
  A failed transfer marks the device failed; no unbounded retry loop.
- The disk registers as `usb0` through `blk.h`, so `afs_probe`, the unlock
  screen, the installer and `SYS_DISK_LIST` see it without special cases.
- `make test-usb-storage` boots QEMU with a 16 MiB `usb-storage` disk and
  checks enumeration, capacity and the blank-disk setup path.

## Evidence

```text
make                 PASS
make unittest        58 kernel / 70 terminal, 0 failed
make test-usb-storage PASS
```

The test log shows `mass storage ready, 32768 sectors`, `[blk] usb0` and
`[storage] usb0: blank disk`.

## Not done

- A write/read-back test through the UI (the blank-disk path stops at the
  passphrase dialog; the unit suite does not touch QEMU hardware).
- Wi-Fi, audio, battery/lid, camera, Bluetooth, suspend/resume and thermal
  policy. The ACPI AML interpreter is the common blocker.
- USB hubs, UAS, multiple LUNs and non-512-byte sectors.

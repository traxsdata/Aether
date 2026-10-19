# Review of the 1.3 daily-driver slice

This is the first 1.3 workstream PR. It is based on 1.0 and does not include
the 1.1 bundle runtime or the 1.2 Claude Code work.

## Changes

- **Change passphrase.** `afs_change_passphrase()` checks the old passphrase
  with `afs_verify`, derives a new key-encryption key from the new passphrase
  and the existing salt/PBKDF2 parameters, re-wraps the random volume key with
  a fresh nonce, and writes the superblock only after the new wrap is built.
  `SYS_CHANGE_PASSPHRASE` is limited to `CAP_CONFIG` (Settings).
- **ACPI shutdown.** `kernel/power.c` walks the RSDP, XSDT/RSDT and FADT,
  reads `PM1a_CNT_BLK`/`PM1b_CNT_BLK` and writes the conventional S5 value.
  If the table is missing or the write does not power off, it tries QEMU's
  ACPI shutdown ports and halts. `SYS_SHUTDOWN` is limited to `CAP_CONFIG`.
- **Desktop and Settings.** The æ menu has Shut Down; Settings > Security has
  current/new/new-again passphrase fields and a Change button.
- **Tests.** `make test-power` drives the menu and checks the serial log for
  the power-off path. The kernel `files` suite now changes the passphrase on a
  scratch volume, verifies the old one is refused, and remounts with the new
  one.

## Evidence

```text
make test        PASS
make unittest    57 kernel / 69 terminal, 0 failed
make test-lock   PASS
make test-power  PASS
AETHER_DISK=none tools/qemu-drive.py tests/settings.txt   PASS
```

The passphrase UI was checked by screenshot; the encrypted-volume change is
covered by the kernel test. The ACPI path is exercised on QEMU; firmware that
uses a non-standard S5 value or hides the FADT will fall through to the
fallback ports and halt instead of powering off.

## Not done

- Suspend/resume and lid/backlight.
- Trash/undo, clipboard and notifications (still 1.3 items).
- A physical-machine shutdown test.
- App package signatures and Secure Boot (1.4).

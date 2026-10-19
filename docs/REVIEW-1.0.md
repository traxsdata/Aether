# Review of the 1.0 work

1.0 starts from the 0.9 tree and the window-manager work that followed 0.8.
It deliberately does not touch the xHCI/I²C driver slice (PR #19) or the
Terminal and command-line tools (PR #20).

The goal was a desktop a normal user can leave running every day: a session
lock, data-safe saves, fewer ways for a background app to get in the way,
and a security boundary that survives the failure modes found in this review.

## What changed

### Session lock and everyday desktop

- **Lock Screen** in the æ menu and on **Super+L**. The kernel draws an
  opaque lock screen, stops delivering key and mouse events, and checks the
  passphrase against the mounted volume's key-wrap record with `afs_verify`.
  Unlocking does not reinitialize or remount the file system.
- **Auto-lock** after idle. Settings > Security offers Never, 5, 10 (the
  default) and 30 minutes; the setting is `security.autolock`. Temporary
  in-memory storage is never locked, because it has no passphrase to check.
- **Settings > Security** also has a Lock Now button (`SYS_LOCK`, trusted
  apps only) and shows the storage and kernel protection status.
- **Ctrl+Alt+Esc** is the emergency close: it closes the app behind a modal
  system dialog, or the focused app. It is handled before the dialog so a
  picker loop cannot trap the desktop.
- The æ menu gained Lock Screen and Restart.

### System dialog ownership

System dialogs used to publish their answer in `struct proc`, shared by all
of the app's threads. A multithreaded app could let a second thread open
another dialog and consume the first thread's answer. That mattered:

- `Block` on a firewall question could arrive as `Allow once` if a sibling
  thread got a successful file picker;
- a label-lowering approval could authorize a different file;
- a destructive update confirmation was shared with other dialog types.

Dialogs now belong to the **thread** that opened them. `wm_dialog_*` records
that thread, `dlg_finish` wakes only it, and each dialog type has its own
refusal budget (`dialog_denials[4]`). The denial budget also covers the file
picker, which previously had none.

### Window focus and lifetime

- A new window is focused only when the user launched the app (its first
  window) or the app already owns the focused window. A background app's
  later window goes to the bottom and receives no keyboard input until the
  user clicks it. This closes a keystroke-stealing primitive available to
  every installed package.
- `SYS_WIN_CREATE` validates its output pointer before allocating. A failed
  `copy_out` used to leak the window, its pixel buffer and a slot.
- Window address-space slots are recycled. An app that opened and closed 64
  windows used to be refused forever; files and notes can now keep working.

### Data-loss prevention

The review found metadata paths that leaked high-label data without tainting
the reader:

- `SEEK_END`, and `SEEK_CUR` on an `O_APPEND` handle, revealed a file's size;
- `sys_write` published the append offset before writing anything;
- a partial read that hit a bad block returned the bytes already copied
  without tainting;
- opening an existing file, `set_label`, the picker result, remove and rename
  exposed labels/names without tainting.

All of those now go through `meta_read()`/`dlp_taint()`, and a failed save
no longer changes the handle offset. The save picker also no longer opens
the target with `O_TRUNC`; Notes writes the new text and then calls
`SYS_TRUNCATE`, so a failed write leaves the old file intact. The picker
checks the file system (not just the 96 visible entries) before warning
about replacement, and clears the warning when the folder changes.

### AFS and persistence

- Superblock structure is validated in 64-bit arithmetic: `inode_blocks`,
  `ninodes` and `data_start` can no longer wrap and make the unlock scan
  write past its inode bitmap. `ninodes` must be a multiple of 15 and the
  inode table must be exactly `ninodes / 15` blocks.
- `afs_iget` rejects invalid modes, labels and out-of-range block pointers.
- `afs_truncate` uses overflow-free rounding, checks every pointer-block
  read/write, and refuses to claim success when the inode could not be
  updated. Blocks can no longer be freed and then read back as old data.
- Failed unlocks free their tables; the block cache is wiped when a volume
  is initialized.
- `afs_probe` also checks the backup GPT at the end of the disk, so a disk
  with a damaged primary table is not offered for formatting.
- Settings writes check the truncate/write results, cap the total file size
  and roll back the in-memory value if persistence fails.
- `mkdisk.py` writes the backup GPT entry array immediately before the
  backup header (`sectors - 33`), where `common/gpt.c` reads it, and rejects
  disks too small for the layout. The parser refuses a usable range that
  overlaps the backup GPT.

### Network

- UDP replies with a checksum are verified (DHCP and DNS always set one).
- DNS accepts an A record only for the queried name or a name reached
  through the answer's CNAME chain.
- A LAN or special-use **IP literal** always opens the kernel consent dialog,
  even for `browser` on 80/443. The browser's built-in policy no longer lets
  page content reach the router, local services or metadata addresses.
  Loopback, link-local and multicast literals are still refused outright.
- Literal addresses are canonicalized before firewall rule matching, and
  firewall app names are matched case-insensitively.
- Connection handles carry a generation; a blocked send/receive cannot move
  to a new connection that reused the same handle.
- A closed half-open connection is freed at once instead of retransmitting
  an ownerless SYN for 20 seconds.
- `net_poll` rejects a short Ethernet frame before reading its header.
- Untrusted apps no longer see the machine's IP addresses, netmask, gateway,
  DNS server or MAC through `SYS_NET_INFO`.

### Install, updates and app permissions

- `struct sysimage_header` now carries `boot_sha256`. `mksysimage.py --boot`
  fills it before signing, and `SYS_SYSTEM_INSTALL` requires the supplied
  boot image to match it. A signed kernel can no longer be paired with an
  unrelated bootloader during a fresh install.
- App updates keep the old capability mask. A package cannot silently add
  `CAP_NET` or `CAP_PICK` by replacing an installed app.
- Installed packages are clamped to 512 MiB (down from 4 GiB), and the
  kernel rejects a header that asks for more.
- The installer asks for the passphrase twice and its OS tab states what is
  actually verified (signed system image, integrity-checked packages).

### Smaller fixes

- Calculator reads scientific notation (`1e13`) back correctly.
- Settings has a Security tab and a scrollable firewall list that names the
  built-in browser policy instead of claiming every connection asks.
- `SYS_WAIT` only reports a child's status and consumes it once.

## Evidence

Run on this tree (QEMU TCG, 1280x800, 1 GiB):

```text
make test        PASS
make unittest    57 kernel / 69 terminal, 0 failed
AETHER_DISK=none tools/qemu-drive.py tests/settings.txt   PASS
```

The kernel unit tests include the new hostile-superblock case in the
`files` suite. `tests/settings.txt` now visits the Security tab and takes a
screenshot of it.

## Known gaps (not addressed here)

These are deliberate follow-ups, not regressions:

- **App packages are integrity-checked, not signed.** App Store trusts
  GitHub's release digest for transport; a compromised release account or a
  local file can still ship a package. Signed packages and a publisher UI
  are a 1.x item.
- **No Secure Boot signature.** The loader is still unsigned.
- **No per-user accounts.** The disk passphrase is the only user boundary;
  every app runs under the same user's files.
- **The installer and App Store still draw fixed-size layouts.** They fit
  the tested 1280x800 screen, but a 1024x600 netbook would clip the footer.
- **No clipboard.** Copy/paste between Notes, Files and the browser is not
  implemented; adding it safely needs a label-carrying clipboard.
- **Files deletion is still immediate.** There is no trash/undo.
- **Dialogs are still global (one at a time).** The per-thread ownership fix
  makes answers safe; serializing two apps' requests is unchanged.
- Drivers, Wi-Fi, USB storage, audio and the Terminal are outside this slice.

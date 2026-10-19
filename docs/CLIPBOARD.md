# Clipboard

The clipboard is kernel memory with a DLP label, shared by applications that
hold `CAP_CLIPBOARD`. It is not a shared page: every read and write goes
through `SYS_CLIPBOARD` (`CLIP_SET`, `CLIP_GET`, `CLIP_INFO`) and is bounded
to 64 KiB.

## Rules

- **Writing never lowers the label.** `CLIP_SET(data, len, label)` stores
  `max(label, caller's taint)`. A Confidential process cannot label a copy
  Public.
- **Reading raises the reader.** `CLIP_GET` copies at most `max` bytes and
  reports the label in `struct clip_result`; the caller's taint rises to that
  label. Existing log and network egress checks then apply.
- **No ambient access.** Reading, writing and even asking for the length need
  `CAP_CLIPBOARD`. Terminal, Notes and Files have it; a background app that
  does not cannot poll the clipboard.
- **Cleared on lock.** `wm_lock_now()` wipes the buffer and label before the
  lock screen is drawn. Unlocking never restores old clipboard data.

## Apps

- **Notes:** Ctrl+C/X copy or cut the whole document; Ctrl+V inserts at the
  cursor. The file's label is passed as the requested label.
- **Files:** Ctrl+C/X copy a path to both the internal file clipboard and the
  system clipboard; Ctrl+V keeps moving/copying the path when one is held,
  otherwise it writes clipboard text to a new `Pasted.txt`.
- **Terminal:** no key binding yet; the syscall is available to it.

The kernel audit log records who copied and pasted, the byte count and the
label, but never the contents.

# Review of 1.7: label-aware clipboard

Based on `v1.6`. This finishes the clipboard item deferred from 1.3.

## Changes

- New `CAP_CLIPBOARD` and `SYS_CLIPBOARD` with `CLIP_SET`, `CLIP_GET` and
  `CLIP_INFO`. The buffer is kernel memory (64 KiB), not a shared page; every
  operation is bounded and copied through `copy_in`/`copy_out`.
- DLP integration: `CLIP_SET` stores `max(label, caller taint)`, and
  `CLIP_GET` raises the reader's taint to the stored label. The audit log
  records the actor, byte count and label, not the contents.
- `wm_lock_now()` clears the clipboard before drawing the lock screen.
- Notes: Ctrl+C/X copy or cut the whole document; Ctrl+V inserts at the
  cursor. The open file's label is the requested label.
- Files: Ctrl+C/X still use its path clipboard and now also write the path to
  the system clipboard with the entry's label; Ctrl+V falls back to writing
  clipboard text as a new `Pasted.txt`.
- Kernel test: writing taints the writer, reading raises taint, a Public
  relabel cannot lower Confidential, oversized data and missing capability
  are refused, and clear resets both length and label.

## Evidence

```text
make            PASS
make unittest   60 kernel / 72 terminal, 0 failed
```

The UI path was checked by copying in Notes and pasting in Files.

## Not done

- Rich text, images and file objects on the clipboard.
- A selection model in Notes (copy is currently the whole document).
- A paste key binding in Terminal, and a clipboard manager/history.

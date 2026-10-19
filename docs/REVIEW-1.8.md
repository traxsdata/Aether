# Review of 1.8: trash and undo

Based on `v1.7`. This finishes the trash/undo item deferred from 1.3.

## Changes

- Files' Delete button and Del key now rename the selected item to
  `/home/.trash`, using `unique_path` for collisions (reports, reports 2, ...).
  No copy or read is involved, so labels and data are untouched.
- **Ctrl+Z** restores the last move when the destination still exists and the
  old name is free. The undo state holds full paths, not user text.
- **Trash** is a fourth sidebar place. It is hidden from the ordinary Home
  listing and created on demand. Delete inside Trash shows the existing
  confirmation and then removes the tree; outside Trash there is no
  permanent-delete dialog because nothing is destroyed.
- The in-window delete copy now says "permanently deleted" only for the Trash
  path. The test script was updated for the move/undo flow.
- **DLP fix:** `vfs.c` now raises a trusted process's taint when it reads a
  file or folder entry. Files' own guide promised this, but `meta_read` had
  exempted trusted apps; previewing a Secret file left the badge Personal.
  The explorer test now observes Secret taint after listing/previewing.

## Evidence

```text
make            PASS
make unittest   60 kernel / 72 terminal, 0 failed
AETHER_DISK=<fresh> tools/qemu-drive.py tests/storage.txt   PASS
AETHER_DISK=<fresh> tools/qemu-drive.py tests/explorer.txt  PASS
```

## Not done

- A size/age policy or automatic trash cleanup.
- A bulk "Empty Trash" command (each item can be selected and deleted with a
  confirmation).
- Restoring into a renamed path or resolving name collisions automatically.

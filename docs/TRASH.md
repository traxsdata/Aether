# Trash and undo

Files never removes an item in place. Delete moves it to `/home/.trash` with a
unique name, preserving its DLP label; the move is a rename, so the data is
never copied through a tainted process.

- **Undo:** **Ctrl+Z** (or the status-line hint) renames the last trashed item
  back to where it came from if that name is still free.
- **Trash place:** the sidebar's **Trash** opens `/home/.trash`. Deleting
  there is permanent and always shows the kernel-independent confirmation
  dialog first; an empty Trash deletes the item immediately after the
  confirmation.
- **Hidden from Home:** `.trash` is not listed as an ordinary entry, so a
  normal copy/move cannot put the trash inside itself. The Trash place
  creates the folder on first use.
- **Labels:** a rename carries the file's label. Restoring keeps it. Emptying
  the trash is the only path that destroys data, and it keeps the same label
  checks as any other delete.

# Filesystem stress review

Scope: `aether-fs` (the bounded RAM filesystem), its host tests, and the
service paths that turn paths and names into filesystem operations.

This is a prototype format. It is not a production filesystem, does not survive
power loss, and has no encryption or global consistency checker. The review
below separates issues fixed during this pass from issues that remain design or
follow-up work.

## Stress methods

- 4,000 randomized create, write, copy, rename, remove, list, read, and
  open/close operations over a bounded set of directories and names.
- 160 corrupted images with one to eight bit flips across the superblock,
  inode table, free bitmap, and data area, followed by listing, reading,
  stat, create, replace, rename, remove, and open/read/close.
- Maximum-name and maximum-file-size cases.
- Malformed directory entries: control-bearing names, zero inodes, unknown
  entry kinds, and duplicate names.
- Directory pointers and file block pointers aimed at reserved metadata.
- Deliberate inode and block exhaustion, followed by deletion and reuse.

## Fixed in this pass

| Severity | Issue | Resolution |
|---|---|---|
| High | A filename containing a tab, newline, NUL, or other ASCII control byte could inject extra records into the `name<TAB>kind<TAB>size` listing protocol or truncate a name on disk. | Path components now reject empty names and all ASCII control bytes. Stored directory entries with empty or control-bearing names are rejected as `Corrupt`. |
| Medium | A corrupted file inode could point at the superblock, inode table, or another reserved metadata block, exposing metadata as file contents. | All file and directory data-block pointers must be in the data region (`block >= DATA_START_BLOCK`). |
| Medium | Corrupt directory entries with inode 0, an out-of-range inode, an unknown kind byte, or a duplicate name were accepted and could produce ambiguous lookups or misleading listings. | `dir_entries` validates the inode range, the entry kind, name bytes, and duplicate names. |
| Low | `rename(path, path)` returned success even when the source did not exist. | The source is now looked up before the self-rename no-op succeeds. |

## Remaining issues and recommendations

### 1. No global cross-link validation (Medium)

The inode validator checks each inode independently. A hostile image can still
point two inodes at the same data block. Reading remains safe, but writing or
deleting one inode can free a block still referenced by the other, corrupting
it. Recommendation: add a bounded mount-time or first-use consistency scan that
verifies:

- every referenced data block is marked used;
- no data block is referenced by two inodes;
- every used data bitmap bit is reachable from an inode or reserved region.

### 2. Open handles survive replacement (Medium, data integrity)

`unlink` refuses to delete a file while a handle is open, but `write_file`
(including portal commit and kernel console writes) can replace the same
inode's contents. A writable handle opened before the replacement can then
write at its old offset into the newer content. This is not a sandbox escape,
but it can lose data under concurrent editing. Recommendation: add an inode
version/generation and reject stale writes, or refuse replacement while a
writable handle is open. Portal save must still support the current
stage-then-commit flow.

### 3. Directory block leak on a failed rename rollback (Low)

`rename` adds the destination entry, then removes the source entry. If the
source removal fails after the destination parent allocated its first
directory block, the rollback removes the entry but leaves the empty block
allocated. This requires a device error or a corrupt directory during the
operation. Recommendation: track whether `dir_add` allocated the block and
release it on rollback.

### 4. Post-commit cleanup can report failure (Low)

After a successful inode commit, `write_inode_blocks` releases the old blocks.
If a block release fails, the operation returns an error even though the new
contents are already committed, and the remaining old blocks leak. The new
contents are safe, but the caller sees an ambiguous result. Recommendation:
separate "commit failed" from "committed with leaked space" and report the
latter as a diagnostic.

### 5. Resource bounds are intentionally small (Informational)

- 128 KiB device, 256 blocks.
- 64 inodes, 8 data blocks (4 KiB) per file.
- 25 entries per directory, 16-byte names, 128-byte paths.
- 8 open files globally.
- 2 concurrent portal staging files, 2 MiB each.

Operations fail closed at these bounds. The 4 KiB per-file limit is much
smaller than the 2 MiB portal staging limit, so a staged save larger than the
filesystem limit fails at commit rather than corrupting the target. This is
safe but should be aligned before larger files are supported.

### 6. Stack use in read/write (Informational)

`read` and `write` each place a 4 KiB file buffer on the stack. The current
kernel stack is sufficient in testing, but future nesting or larger file
limits must be checked against the actual kernel stack budget.

### 7. Durability and isolation are not present (Documented limitation)

The block device is a RAM disk. The Trash index, portal grants, and files do
not survive a power cycle. The filesystem service and desktop adapters are
still trusted ring-0 code. None of the stress results should be read as a
claim of production durability, crash consistency, encryption, or
compromise-contained application isolation.

## Regression tests added

- `control_characters_cannot_inject_listing_records`
- `corrupt_directory_entries_fail_closed`
- `rename_to_the_same_missing_path_reports_not_found`
- `randomized_file_operations_never_panic_and_remain_consistent`
- `corrupted_images_never_panic_metadata_paths`
- Extended `corrupt_inode_metadata_is_rejected_without_panicking` with
  reserved-metadata block pointers.
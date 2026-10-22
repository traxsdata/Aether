# Filesystem and file handles

Aether's first storage stack is deliberately small. It proves the trust
boundary between a process, the file service, and a block device; it is not a
production on-disk format.

## Components

| Layer | Location | Responsibility |
|---|---|---|
| Virtual block device | `kernel/fs/src/lib.rs` (`RamDisk`) | 512 × 512-byte blocks (256 KiB) in RAM |
| Filesystem | `aether-fs` | superblock, inode table, directories, contiguous file blocks |
| File service | `kernel/src/fs.rs` | per-process roots, open handles, path validation |
| Capability layer | `kernel/ipc` + kernel `CapTable` | generation-checked `File` capabilities |
| Portal | `kernel/src/portal.rs` | exact-path grants outside a process root |

The format is intentionally bounded: 512 blocks (256 KiB), 96 inodes, at most
8 blocks (4 KiB) per file, 25 entries per directory block, and a 16-slot open
table. The inode table is reserved before the first data block. Exhaustion is
reported as `ENOSPC`; no metadata allocation can grow without bound.

The current user rights are `READ`, `WRITE`, and `CREATE`. Separate delete,
rename, execute, and directory-listing rights from the specification are not
exposed yet; the kernel console performs deletion with its own root authority.
The host-tested `aether_services::ux` policy models the full right set and the
file/folder grant scopes that the eventual service ABI will expose.

Every metadata operation validates the superblock magic, version, inode count,
and root inode before trusting an inode. Unformatted media and unknown versions
fail with `NotFormatted` or `Corrupt` rather than being parsed as a valid
filesystem.

## Path scoping

A process never resolves a global path. The kernel owns a root prefix for each
task:

- the native application runs with `/apps/demo`;
- the console shell runs with `/`;
- the sibling application record uses `/apps/other`.

`resolve(scope, path)` joins the root and the untrusted path, rejects every
`..` component, rejects names longer than 16 bytes, and bounds the result to 128
bytes. A leading `/` is still interpreted relative to the caller's root. This
makes sibling access by traversal impossible by construction, even if a caller
sends a malformed path.

## File handles

`open` creates an open-file object in the filesystem and grants a capability in
the calling task's table. The userspace handle encodes only the capability slot
index and generation. Every `read`, `write`, and `close`:

1. validates the handle generation against the task's capability table;
2. requires the object type to be `File` and the requested right;
3. maps the object id back to the open-file slot and validates its generation;
4. copies user data only after validating the whole range.

Closing revokes the capability and advances the slot generation, so a stale
handle cannot name a later file. A handle from another task is meaningless
because capability tables are per task. Deletion is refused while any handle
for the inode is open; after the last close the name can be removed and the
blocks released.

## Mutations and scope

`rename`, `copy_file`, `remove`, and `rmdir` operate below a trusted service
scope, using the same `resolve` rules as reads. `rename` changes only directory
entries, refuses an existing destination, and refuses to move a directory
inside itself. `copy_file` is bounded by `MAX_FILE_BYTES` and refuses an
existing destination. `remove` deletes a file or empty directory, and deletion
of an open file is refused until the last handle closes. The Files application
exposes duplicate (`F3`), rename (`F2`, with a bounded prompt), and delete
(`Delete`) on the same service methods; the console exposes `cp`, `mv`, and
`rm`. `AETHER_FILES_MUTATE_OK` proves the copy/rename/delete path at boot.

## User workspace and private application storage

`/home` is the current user's logical workspace. The seeded folders are
`Recent`, `Favorites`, `Documents`, `Downloads`, `Pictures`, `Music`, `Videos`,
`Projects`, `Shared`, and `Trash`. These are resource-provider locations, not a
promise that every future backend is a conventional physical directory. The
same thirteen destinations, including service-backed `Applications` and
`Devices` under `/system`, appear in the Files sidebar and in the specification
order used by Open, Save, and file-selection portals.

`/system/applications` exposes package metadata and user-visible documents; it
does not expose `/apps`. Installed applications receive
`/apps/<package-id>/private` with `Data`, `Cache`, `Settings`, and `Temporary`
subdirectories. The package identity owns the storage, and the Files adapter
denies vault enumeration regardless of the path displayed by the UI.

User-facing delete moves a file or empty directory into `/home/Trash` through a
service rename. Before the move, the service records the Trash component and
the original absolute path in a bounded `/system/trash/index` store. Activating
an item in Trash restores it only to that recorded path and refuses to
overwrite an existing item. Deleting from Trash is the explicit permanent
action and removes the metadata. The index uses the same fail-closed parser as
the portal store: malformed, duplicated, traversal-bearing, or oversized
records are rejected instead of restoring an item to a guessed location.

The host-tested `aether_services::ux` module owns the policy that is not yet in
the syscall ABI: stable resource identities, ownership domains, the complete
right set, folder-scoped grants, one-time and persistent lifetimes, search
isolation, application storage accounting, and uninstall preservation. See
[the unified filesystem note](../specifications/ui-filesystem.md).

## Portal grants

An application cannot name a file outside its root. It may call `portal_open`,
which succeeds only when a grant matches the exact process root, exact path,
and requested right. Grants are single-file and revocable; a grant for a file
never implies access to its parent or siblings. A `portal_request` queues an
interactive consent request but grants nothing; the desktop Portal window (or
`portal approve N` / `portal deny N` on the console) decides it. Approval
installs an exact-file record and rewrites `/system/portal/grants` through the
host-tested `aether_services::portal_store` codec. Corrupt, duplicate, or
traversal-bearing records are rejected rather than partially loaded.

`portal` lists grants and pending requests; `portal revoke <index>` marks a
grant revoked. Revocation applies to future portal opens. A file capability
that was already issued remains open until the owner closes it, because
revoking a grant cannot undo bytes the application has already read. The
record store survives service restarts within the volume, but the only block
device is currently a RAM disk, so records do not yet survive a power cycle.

## Persistence and recovery

The RAM disk is recreated and seeded on every boot. The image markers
`/system/images/current` and `/system/images/previous` are metadata only; user
data lives in `/home`. Recovery selects the previous marker when the current
marker is missing and records the selection in `/system/images/selected`.
Persistent block storage and real update verification remain follow-up work.

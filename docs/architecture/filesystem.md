# Filesystem and file handles

Aether's first storage stack is deliberately small. It proves the trust
boundary between a process, the file service, and a block device; it is not a
production on-disk format.

## Components

| Layer | Location | Responsibility |
|---|---|---|
| Virtual block device | `kernel/fs/src/lib.rs` (`RamDisk`) | 256 × 512-byte blocks in RAM |
| Filesystem | `aether-fs` | superblock, inode table, directories, contiguous file blocks |
| File service | `kernel/src/fs.rs` | per-process roots, open handles, path validation |
| Capability layer | `kernel/ipc` + kernel `CapTable` | generation-checked `File` capabilities |
| Portal | `kernel/src/portal.rs` | exact-path grants outside a process root |

The format is intentionally bounded: 32 inodes, at most 8 blocks (4 KiB) per
file, 25 entries per directory block, and an 8-slot open table. Exhaustion is
reported as `ENOSPC`; no metadata allocation can grow without bound.

The current user rights are `READ`, `WRITE`, and `CREATE`. Separate delete,
rename, execute, and directory-listing rights from the specification are not
exposed yet; the kernel console performs deletion with its own root authority.

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

## Portal grants

An application cannot name a file outside its root. It may call `portal_open`,
which succeeds only when a grant matches the exact process root, exact path,
and requested right. Grants are single-file and revocable; a grant for a file
never implies access to its parent or siblings. The boot prototype seeds one
read grant for `demo` on `/home/welcome.txt`, displays it in the Security
Center, and checks a temporary grant/revocation pair during boot
(`AETHER_REVOCATION_OK`).

`portal` lists the grant table and `portal revoke <index>` marks a grant
revoked. Revocation applies to future portal opens. A file capability that was
already issued remains open until the owner closes it, because revoking a grant
cannot undo bytes the application has already read. There is no persistent
grant store or interactive consent UI yet.

## Persistence and recovery

The RAM disk is recreated and seeded on every boot. The image markers
`/system/images/current` and `/system/images/previous` are metadata only; user
data lives in `/home`. Recovery selects the previous marker when the current
marker is missing and records the selection in `/system/images/selected`.
Persistent block storage and real update verification remain follow-up work.

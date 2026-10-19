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
because capability tables are per task.

## Portal grants

An application cannot name a file outside its root. It may call `portal_open`,
which succeeds only when a grant matches the exact process root, exact path,
and requested right. Grants are single-file and revocable; a grant for a file
never implies access to its parent or siblings. The boot prototype seeds one
read grant for `demo` on `/home/welcome.txt`, and displays it in the Security
Center.

## Persistence and recovery

The RAM disk is recreated and seeded on every boot. The image markers
`/system/images/current` and `/system/images/previous` are metadata only; user
data lives in `/home`. Recovery selects the previous marker when the current
marker is missing and records the selection in `/system/images/selected`.
Persistent block storage and real update verification remain follow-up work.

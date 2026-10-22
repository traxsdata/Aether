# System ABI Specification (Draft)

Document ID: AETHER-ABI-001

This document defines the boot hand-off contract between the Aether UEFI
loader and the Aether kernel, and the first stable subset of the user-space
system call ABI.

## Principles

- The kernel ABI and application ABI are distinct.
- Structures are `repr(C)` with fixed-width fields and natural alignment.
- New fields are appended only with a version bump.
- The kernel validates all metadata before trusting it.
- Physical addresses are used during boot; the environment is identity-mapped.

## BootInfo

Defined in [`abi/src/lib.rs`](../../abi/src/lib.rs).

| Field | Type | Meaning |
|---|---|---|
| `magic` | `u64` | `0x4145_5448_4552_4f53` (`AETHEROS`) |
| `version` | `u32` | Current value: `4` |
| `struct_size` | `u32` | Size of this structure in bytes |
| `memory_regions` | `u64` | Physical address of the region array |
| `memory_region_count` | `u64` | Number of regions, at most 256 |
| `kernel_start` | `u64` | First physical byte of the kernel image |
| `kernel_end` | `u64` | Exclusive end of the kernel image |
| `entry_point` | `u64` | Kernel entry physical address |
| `system_table` | `u64` | UEFI system table for runtime services, or 0 |
| `framebuffer` | `FramebufferInfo` | Linear GOP framebuffer or all-zero |
| `linux_appimage` | `BootArtifact` | Loader-staged AppImage extent, or all-zero |
| `linux_appimage_record` | `BootArtifact` | Signed import-record extent, or all-zero |
| `reserved` | `[u64; 4]` | Must be zero |

The current structure size is 168 bytes and the version is 4. A kernel that
sees a smaller `struct_size`, an unknown version, or a bad magic refuses to
boot.

`BootArtifact` contains a physical `address` and `length`. An all-zero pair is
absent; a half-present, misaligned, overflowing, or oversized pair is rejected.
The AppImage is bounded to 32 MiB and its record to 64 KiB. The loader reads
`\linux\app.AppImage` and `\linux\app.import` from the boot volume, keeps the
pages allocated across `ExitBootServices`, and never executes the AppImage
runtime. The kernel verifies the record and digest before parsing.

## FramebufferInfo

| Field | Type | Meaning |
|---|---|---|
| `base` | `u64` | Four-byte-aligned physical framebuffer base, or zero |
| `size` | `u64` | Mapped byte length, at least `stride * height * 4` |
| `width` | `u32` | Visible horizontal pixels |
| `height` | `u32` | Visible vertical pixels |
| `stride` | `u32` | Pixels per scan line |
| `format` | `u32` | 0 = RGBX, 1 = BGRX, 2 = bitmask, 3 = blit-only (UEFI GOP values) |
| `reserved` | `u32` | Must be zero |

An absent framebuffer is all zero. A present descriptor must use a linear
32-bit format, have non-zero dimensions, a stride of at least `width`, and a
size covering the visible rows. The kernel maps the framebuffer identity,
write-combining (cache-disabled when the CPU has no PAT), and non-executable. Bitmask and blit-only modes are rejected
for the current software compositor.

Dimensions and stride are bounded to 8192 pixels, and the aperture to 512 MiB.
The loader checks the 36-byte GOP mode-information layout (one 16-byte pixel
mask, stride at offset 32) and never enlarges the reported aperture to make
validation pass. `PixelFormat::encode_rgb` / `decode_rgb` convert the renderer's
`0x00RRGGBB` colors to and from native little-endian words, clearing reserved bits.

**Migration from version 2:** version 2 reversed the RGB/BGR discriminant names;
some rendering code ignored the format entirely. Version 3 fixes those semantics
without changing the structure size. Rebuild loader and kernel together; mixed
versions are rejected. See [ADR-016](../decisions/ADR-016-desktop-review-and-uefi-correctness.md).

Version 4 appended the two `BootArtifact` descriptors and grew the structure to
168 bytes. Version 5 appends the loader-staged runtime-rootfs descriptors and
grows the structure to 200 bytes. Older loaders cannot boot a newer kernel.

## MemoryRegion

| Field | Type | Meaning |
|---|---|---|
| `start` | `u64` | Inclusive physical start |
| `len` | `u64` | Length in bytes, must be non-zero |
| `kind` | `u32` enum | Coarse classification |
| `reserved` | `u32` | Must be zero |

Kinds: `Usable`, `Reserved`, `AcpiReclaimable`, `AcpiNvs`, `Unusable`,
`BootServices`, `RuntimeServices`, `Loader`, `Mmio`, `Other`.

The loader merges adjacent regions of the same kind. Regions are sorted by
ascending `start`, non-overlapping, and `start + len` must not overflow. The
region array is 8-byte aligned. The loader and kernel both validate these
properties; the kernel rejects a map that is unsorted or overlapping.

## Validation contract

The kernel calls `BootInfo::validate()` and `validate_regions()` before using
the array. The loader guarantees that:

- `validate()` succeeds.
- Reserved header fields are zero and the map pointer is aligned.
- Every region is readable for the lifetime of the boot.
- The region array and `BootInfo` are marked loader-owned in the firmware map.
- The kernel image range is reserved.
- Regions are sorted and no region overlaps another.

The kernel must not dereference `memory_regions` before `validate()` succeeds.
The loader allocates `BootInfo`, the firmware-map buffer, and the region array
below 4 GiB so they remain inside the kernel's 16 GiB identity map. It applies
the same rule to the staged AppImage and record, which remain allocated as
loader-owned memory and are reserved again by the kernel frame allocator.

## User-space ABI (implemented subset)

The application ABI is separate from the boot ABI. User code enters through
interrupt vector `0x80` with the syscall number in `RAX`, arguments in `RDI`,
`RSI`, `RDX`, `R10`, `R8`, and `R9`, and the result in `RAX`. Negative results
are errno values encoded as two's-complement `i64`.

Implemented calls:

| Number | Call | Arguments | Result |
|---|---|---|---|
| 0 | `yield` | — | 0 after another task runs or the next tick |
| 1 | `exit` | status | does not return |
| 2 | `write` | fd, buffer, length | bytes written |
| 3 | `getpid` | — | task id |
| 4 | `open` | path, path length, rights | file handle |
| 5 | `read` | handle, buffer, length | bytes read |
| 6 | `write_file` | handle, buffer, length | bytes written |
| 7 | `close` | handle | 0 |
| 8 | `list` | path, path length, buffer, length | bytes written |
| 9 | `stat` | path, path length | file size |
| 10 | `sleep` | — | 0 after yielding to the scheduler (no sleep queue yet) |
| 11 | `uptime` | — | timer ticks |
| 12 | `portal_open` | path, path length, rights | file handle |
| 13 | `create_window` | reserved | reserved |
| 14 | `task_count` | — | debug value |
| 15 | `endpoint_create` | — | endpoint capability handle (or `ENOSPC` when the pool is exhausted) |
| 16 | `endpoint_send` | endpoint handle, payload, payload length, transfer handle (0 = none) | 0 (`EAGAIN` when the queue is full) |
| 17 | `endpoint_receive` | endpoint handle, buffer, buffer length (at least 64), required transferred-handle out-field | bytes received; the out-field receives the transferred handle, or 0 |
| 18 | `endpoint_close` | endpoint handle | 0; revokes every alias in the caller's table and destroys an owned endpoint |
| 19 | `endpoint_receive_timeout` | same as 17, plus timeout ticks in `R8` (0 = no deadline) | bytes received, or `ETIMEDOUT` |
| 20 | `portal_request` | path, path length, rights | pending consent-request index (grants nothing until approved) |
| 21 | `spawn` | argument block, block length, stdin (0 = terminal, else a pipe id), stdout (same) | child process id |
| 22 | `wait` | child process id | exit status (0–255, or 128 + n when killed or faulted); `EAGAIN` while it runs |
| 23 | `kill` | process id | 0; `EACCES` outside the caller's terminal session |
| 24 | `process_list` | buffer, length | number of 48-byte process records written |
| 25 | `system_info` | buffer, length (at least 96) | bytes written |
| 26 | `read_input` | buffer, length | bytes read from standard input; 0 at end of input; `EAGAIN` while empty |
| 27 | `terminal_size` | — | `(columns << 16) \| rows` |
| 28 | `pipe_create` | — | pipe id; the caller holds both ends |
| 29 | `pipe_read` | pipe id, buffer, length | bytes read; 0 once every writer is gone; `EAGAIN` while empty |
| 30 | `pipe_write` | pipe id, buffer, length | bytes accepted; `EAGAIN` when full; `EPIPE` without readers |
| 31 | `pipe_close` | pipe id, end (1 = read, 2 = write) | 0 |
| 32 | `remove` | path, path length | 0; `ENOTEMPTY` for a non-empty directory |
| 33 | `rename` | source path, length, destination path, length | 0 |
| 34 | `make_dir` | path, path length | 0; `EEXIST` when present |
| 35 | `arguments` | buffer, length | length of the caller's argument block |
| 36 | `terminal_mode` | set flag (1 = set), mode (`TERMINAL_RAW` or 0) | previous mode, plus stdin/stdout-is-terminal bits |

Rules for this implementation:

- Handles encode a capability slot index and generation as
  `(generation << 8) | index`. Handle `0` is "no handle"/"no transfer" and
  values above `0x00ff_ffff` are rejected before lookup. They are never raw
  pointers or kernel object addresses.
- Every syscall validates handle validity, generation, object type, and rights.
- User pointers must lie in the validated user window and every page must be
  present with the user bit (and writable for output) before a copy.
- File paths are resolved relative to the caller's kernel-owned root; `..` is
  rejected.
- Sender identity is derived from the current task, not from a message field.
- Portal access requires an exact process-root, path, and right grant.
- A ring-3 fault retires the task; the kernel remains alive.

Endpoint syscalls (15–17) add these rules:

- `endpoint_create` mints a capability for a new bounded endpoint object with
  `SEND`, `RECEIVE`, and `TRANSFER` rights, granted from the caller's own
  capability table. The endpoint pool is fixed (eight objects); further
  `endpoint_create` calls fail with `ENOSPC` instead of growing kernel memory.
- `endpoint_send` copies at most 64 payload bytes (`MAX_MESSAGE_BYTES` from
  the IPC crate) and refuses more before touching the queue. The optional
  transfer handle must be a handle the caller owns and must carry `TRANSFER`;
  the receiver's installed copy never carries `TRANSFER`, so transfer chains
  cannot be extended by receivers.
- `endpoint_receive` requires a buffer able to hold a maximum-size message
  (at least 64 bytes) so a receive is never partial, plus a required
  four-byte writable out-field for the transferred handle that does not
  overlap the payload buffer. Passing `0` for the out-field is an `EINVAL`
  usage error; it is not silently treated as "discard the transferred
  handle", because the kernel installs a transferred capability before it
  reports success. The transferred handle is stored byte-wise; the call
  returns the received byte count in `RAX`.

Error numbers (two's-complement `i64` in `RAX`):

| Value | Name | Meaning |
|---|---|---|
| -1 | `ENOSYS` | unknown syscall number |
| -2 | `EINVAL` | invalid argument, range, or usage |
| -3 | `EACCES` | capability rights, portal, or permission denial |
| -4 | `ENOENT` | path does not exist |
| -5 | `ENOSPC` | kernel resource limit (open-file pool, capability table, endpoint pool) |
| -6 | `EPERM` | sandbox escape attempt (`..` outside the task root) |
| -7 | `EBADF` | bad handle: forged, stale, or wrong object type |
| -8 | `ENOGRANT` | no portal grant for this path/right |
| -9 | `EIO` | device or filesystem corruption |
| -10 | `EAGAIN` | the endpoint queue is full (send) or empty (non-blocking receive); retry |
| -11 | `ETIMEDOUT` | a blocking receive reached its tick deadline |
| -12 | `EPIPE` | a pipe write with no reader left |
| -13 | `EEXIST` | the path already exists |
| -14 | `ENOTDIR` | a path component is not a directory |
| -15 | `EISDIR` | a file operation named a directory |
| -16 | `ENOTEMPTY` | `remove` of a directory that still has entries |

Terminal session calls (21–36) add these rules:

- Only tasks attached to the desktop Terminal's session may use them; every
  other task gets `EACCES` (or, for `arguments`, an empty block). The first
  session task is the shell the kernel starts when the window opens; every
  later one is a child that a session task spawned.
- `spawn` starts only the programs named in `TERMINAL_PROGRAMS`, from the
  kernel-embedded userland image; there is no path-based exec. The argument
  block is NUL-separated text (working directory, program name, arguments),
  at most 512 bytes and 32 entries. A child's stdin and stdout are the
  terminal or pipe ends the caller already holds, so a program can never
  attach to a pipe it was not given.
- Session programs run as the ordinary user: their filesystem root is
  `/home`, they hold no capabilities at start, and `portal_open` is denied.
  The kernel console's ring-0 shell is unchanged and separate.
- `kill` and `wait` reach only processes in the caller's session; kernel
  tasks, services, and applications are out of reach. Ctrl+C in cooked mode
  ends every session job except the shell; in raw mode (`terminal_mode`) the
  byte is delivered to the program instead.
- `process_list` reports every task (so `ps -e` and `htop` work), but only
  identity, state, CPU ticks, and resident memory; no addresses or handles.
- Pipes are eight fixed 1 KiB kernel buffers per session. A process holds a
  bit mask of the ends it owns; exit releases them, and a reader sees end of
  input once no writer remains.

Still planned: general process creation for applications (terminal programs
have `spawn`/`wait`), derivation-tree capability slots and
mapping removal, a per-endpoint wait list with cancellation, memory-object
mapping, window rendering syscalls, and a versioned SDK release process.

Endpoint close/destroy, blocking receives with timeouts, and exact-file portal
consent requests are implemented. A portal request is display state only: it
creates no capability, and the desktop approval installs a persisted exact-file
record that `portal_open` still checks.

## Linux display endpoint (Gate 4 prototype)

This is a separate Linux-ABI surface, not a native syscall. A Linux task opens
the exact path `/dev/aether-display`; a second open in the same task returns
`EBUSY`. A short or oversized write returns `EINVAL`. The endpoint is
default-deny and only the kernel boot display fixture and the signed
`ai.storyteller.gridcraft` AppImage identity may open it.

`write` carries one 48-byte little-endian request header followed by an optional
bounded title:

```text
offset size field
0      4    magic = 0x41445450 ("ADTP")
4      2    version = 1
6      2    kind: 1=create, 2=present, 3=destroy, 4=set_title
8      4    length: bytes after the header (title, <=64; otherwise 0)
12     4    width (1..=1920)
16     4    height (1..=1200)
20     4    stride (>= width*4, multiple of 4, stride*height <= 16 MiB)
24     4    format: 0=XRGB8888, 1=ARGB8888 (little-endian u32 pixels)
28     4    padding (must be zero; C alignment before buffer)
32     8    buffer: guest pointer, present only
40     4    flags (must be 0)
44     4    reserved (must be 0)
```

`present` validates the whole `buffer..buffer+stride*height` range as readable
user memory before copying it into a kernel-owned surface. A faulting range
returns `EFAULT`; a bad stride or geometry returns `EINVAL`. The kernel never
retains the guest pointer. Successful writes return 48.

`read` drains fixed 32-byte records (a short buffer returns `EINVAL`, an empty
nonblocking endpoint returns `EAGAIN`, and `poll` reports `POLLIN` when data is
queued). Overflow drops the oldest record:

```text
offset size field
0      4    kind: 1=configure, 2=key, 3=pointer_motion, 4=pointer_button,
            5=pointer_axis, 6=enter, 7=leave, 8=close, 9=frame
4      4    code (key/button/axis, or width for configure)
8      4    x (surface pixel; width for configure)
12     4    y (surface pixel; height for configure)
16     4    state (1=pressed, 0=released; height for configure)
20     4    serial (monotonic per endpoint)
24     4    modifiers (1=shift, 2=ctrl, 4=alt, 8=super; zero otherwise)
28     4    reserved (zero)
```

The specified record is 32 bytes, so the proposal's `reserved[3]` is replaced
by a 4-byte modifiers field plus one reserved word (the proposal's arithmetic
would make the record 36 bytes). Pointer `x`/`y` are already converted from the
compositor's scaled window content into the created surface's pixel space and
clamped to its bounds. Key events are queued as a press immediately followed by
a release because the PS/2 decoder reports make events only; the bridge maps
the pair to `wl_keyboard.key` transitions. Focus gain queues configure, enter,
and a pointer sample even when the pointer is stationary. `frame` is queued
after a presented buffer is accepted and copied.

Linux `memfd_create` (319), `ftruncate` (77), and `fstat` operate on bounded
anonymous shared objects: 16 MiB per object, four per task, and a 64 MiB global
page budget. `mmap(MAP_SHARED)` aliases the object's pages through the shared
frame registry; `mmap(MAP_PRIVATE)` copies. A shared mapping may not extend past
the object's current length. Descriptor references are retained by `dup` and
`fork`, and released by `close`, task exit, and fault. `read`/`write` on the
memfd are supported and bounded by its current length.

Kernel-owned markers are emitted only after validated operations:
`AETHER_GATE4_DISPLAY_CREATE_OK pid=<n> size=<w>x<h>`,
`AETHER_GATE4_PRESENT_OK frames=<n> checksum=<0x...> source=linux`, and
`AETHER_GATE4_INPUT_OK keys=<n> pointer=<n> focus=<n>`.

## Loader-to-kernel calling convention

- The system is in 64-bit long mode, ring 0, interrupts disabled.
- `RDI` contains the physical address of `BootInfo`.
- The kernel starts on firmware's identity map and replaces CR3 with its own
  identity-mapped page tables after validating the hand-off.
- The kernel must establish its own stack before calling Rust code.
- The loader's boot-services resources are no longer available.

## Versioning

`BOOT_INFO_VERSION` increments for any layout or semantic change. The kernel
accepts only versions it understands. Additive changes append fields and
increase `struct_size`; the loader must zero new fields.

## Known limitations

- No ACPI pointer or RSDP field yet; it will be appended in a future version.
- No kernel command line or entropy seed.
- User handles and the syscall table are still a prototype ABI; they are not
  frozen for third-party binaries.
- No relocation support; the kernel must load at its link address.
- Boot-services memory is freed only after the CR3 switch; runtime-services
  memory is identity-mapped when present.
- The identity map and frame bitmap cover at most 16 GiB. Regions above that
  limit are truncated for mapping and allocation; boot metadata is kept below
  4 GiB.
- The user-space ABI has an implemented subset, but the boot ABI remains the
  only contract with stable layout guarantees.

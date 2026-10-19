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
| `version` | `u32` | Current value: `1` |
| `struct_size` | `u32` | Size of this structure in bytes |
| `memory_regions` | `u64` | Physical address of the region array |
| `memory_region_count` | `u64` | Number of regions, at most 256 |
| `kernel_start` | `u64` | First physical byte of the kernel image |
| `kernel_end` | `u64` | Exclusive end of the kernel image |
| `entry_point` | `u64` | Kernel entry physical address |
| `system_table` | `u64` | UEFI system table for runtime services, or 0 |
| `framebuffer` | `FramebufferInfo` | Linear GOP framebuffer or all-zero |
| `reserved` | `[u64; 4]` | Must be zero |

The current structure size is 136 bytes and the version is 2. A kernel that
sees a smaller `struct_size`, an unknown version, or a bad magic refuses to
boot.

## FramebufferInfo

| Field | Type | Meaning |
|---|---|---|
| `base` | `u64` | Physical framebuffer base, or zero |
| `size` | `u64` | Mapped byte length, at least `stride * height * 4` |
| `width` | `u32` | Visible horizontal pixels |
| `height` | `u32` | Visible vertical pixels |
| `stride` | `u32` | Pixels per scan line |
| `format` | `u32` | 0 = BGRX, 1 = RGBX, 2 = bitmask, 3 = blit-only |
| `reserved` | `u32` | Must be zero |

An absent framebuffer is all zero. A present descriptor must use a linear
32-bit format, have non-zero dimensions, a stride of at least `width`, and a
size covering the visible rows. The kernel maps the framebuffer identity,
cache-disabled, and non-executable. Bitmask and blit-only modes are rejected
for the current software compositor.

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
below 4 GiB so they remain inside the kernel's 16 GiB identity map.

## User-space ABI (implemented subset)

The application ABI is separate from the boot ABI. User code enters through
interrupt vector `0x80` with the syscall number in `RAX`, arguments in `RDI`,
`RSI`, `RDX`, `R10`, `R8`, and `R9`, and the result in `RAX`. Negative results
are errno values encoded as two's-complement `i64`.

Implemented calls:

| Number | Call | Arguments | Result |
|---|---|---|---|
| 0 | `yield` | — | 0 |
| 1 | `exit` | status | does not return |
| 2 | `write` | fd, buffer, length | bytes written |
| 3 | `getpid` | — | task id |
| 4 | `open` | path, path length, rights | file handle |
| 5 | `read` | handle, buffer, length | bytes read |
| 6 | `write_file` | handle, buffer, length | bytes written |
| 7 | `close` | handle | 0 |
| 8 | `list` | path, path length, buffer, length | bytes written |
| 9 | `stat` | path, path length | file size |
| 10 | `sleep` | — | 0 |
| 11 | `uptime` | — | timer ticks |
| 12 | `portal_open` | path, path length, rights | file handle |
| 13 | `create_window` | reserved | reserved |
| 14 | `task_count` | — | debug value |

Rules for this implementation:

- Handles encode a capability slot index and generation. They are never raw
  pointers or kernel object addresses.
- Every syscall validates handle validity, generation, object type, and rights.
- User pointers must lie in the validated user window and every page must be
  present with the user bit (and writable for output) before a copy.
- File paths are resolved relative to the caller's kernel-owned root; `..` is
  rejected.
- Sender identity is derived from the current task, not from a message field.
- Portal access requires an exact process-root, path, and right grant.
- A ring-3 fault retires the task; the kernel remains alive.

Still planned: process creation/wait, capability derivation and revocation,
blocking IPC with timeouts, memory-object mapping, window rendering syscalls,
and a versioned SDK release process.

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

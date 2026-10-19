# System ABI Specification (Draft)

Document ID: AETHER-ABI-001

This document defines the boot hand-off contract between the Aether UEFI
loader and the Aether kernel. The user-space application ABI is separate and
is not defined yet.

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
| `reserved` | `[u64; 4]` | Must be zero |

The current structure size is 96 bytes. A kernel that sees a smaller
`struct_size`, an unknown version, or a bad magic refuses to boot.

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

## User-space ABI (planned, not implemented)

The application ABI is separate from this boot ABI. No user mode or syscall
gate exists yet, so the following is an initial surface specification, not a
stable ABI:

- Process, task, and thread creation, termination, wait, and fault delivery.
- Capability derivation, transfer, revocation, and handle-table operations.
- IPC endpoint send/receive with bounded payloads, capability transfer, and
  timeouts.
- Memory-object creation, mapping, unmapping, and protection changes.
- Service requests for the file portal, networking, audio, and input as those
  services are implemented.

Rules for the first implementation:

- Handles are opaque values interpreted only by the kernel or the resource-owning
  service; they are never raw pointers or kernel object addresses.
- Every syscall validates handle validity, generation, object type, and rights.
- User pointers, lengths, and arrays are validated and copied explicitly; the
  kernel must not dereference an untrusted user pointer directly.
- Sender identity is derived from the current task, not from a message field.
- Capability transfer occurs only through an authorized IPC operation.
- The ABI is versioned; incompatible changes require a new version and a
  migration note.

Concrete syscall numbers, handle encoding, and structure layouts will be fixed
by an ADR before the user-mode loader is merged.

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
- No framebuffer descriptor yet.
- No kernel command line or entropy seed.
- No relocation support; the kernel must load at its link address.
- Boot-services memory is freed only after the CR3 switch; runtime-services
  memory is identity-mapped when present.
- The identity map and frame bitmap cover at most 16 GiB. Regions above that
  limit are truncated for mapping and allocation; boot metadata is kept below
  4 GiB.
- The user-space ABI is specified only as a planned surface; it is not
  implemented. The boot ABI is the only stable contract in this document.

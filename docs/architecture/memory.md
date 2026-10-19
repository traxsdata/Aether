# Physical Memory and Paging

Phase 1 gives the kernel ownership of physical memory and of the page tables
that map it. The implementation lives in:

- `kernel/memory` (`aether-memory`): host-testable bitmap allocator and
  page-table encoding.
- `kernel/src/memory`: kernel policy, identity-map construction, and CR3
  activation.

## Frame allocator

Memory is represented as 4 KiB frames, one bit per frame. The bitmap is a
512 KiB static array in the kernel image, covering 16 GiB. Memory above that
limit is a documented Phase 1 limitation; a dynamically sized allocator will
replace it when larger systems are supported. The identity map is capped at the
same 16 GiB, so a machine with more RAM still boots using the low 16 GiB; the
loader keeps all boot metadata below 4 GiB so it always remains mapped.

Initialization starts with every frame marked in use, then releases
`MemoryKind::Usable` regions. The first 1 MiB and the kernel image range are
always reserved, even if the boot memory map claims otherwise. The hand-off
block and region array are reserved explicitly as a second line of defense.

**Boot-services memory is freed only after the CR3 switch.** Firmware may keep
its active page tables in boot-services data; reusing those frames before the
kernel installs its own tables could corrupt the running address space.

ACPI reclaimable memory stays reserved until an ACPI parser explicitly
releases it. MMIO, unusable, reserved firmware holes, and loader memory are
never handed out.

Operations are bounded and non-blocking:

- `allocate` scans the bitmap from a hint and wraps once.
- `deallocate` rejects double frees and out-of-range frames.
- `reserve_range` rounds outward to whole frames.
- `free_range` trims partial pages and ignores unmappable addresses.

The allocator is not yet synchronized. Phase 1 is single-core with interrupts
disabled; the scheduler must add locking or per-CPU instances before
concurrency.

## Page tables

Before loading those tables, the kernel enables `IA32_EFER.NXE` and `CR0.WP`.
NX makes the `NO_EXECUTE` bits effective; WP makes supervisor writes to
read-only kernel text fault. The kernel refuses to boot if the CPU does not
report NX support instead of silently weakening the mapping.

The kernel builds a new four-level identity map and installs it in CR3:

- `0..2 MiB`: 4 KiB pages, writable and non-executable, covering firmware
  remnants and page-table frames.
- The 2 MiB windows containing the kernel image: 4 KiB pages split by section
  permissions.
- `2 MiB..end`: 2 MiB pages where the window is fully covered by mappable RAM,
  loader, ACPI, or runtime-service regions; 4 KiB pages at region boundaries so
  reserved holes and device MMIO stay unmapped. All mappings are writable and
  non-executable except the kernel text/rodata rules above.

Kernel section boundaries come from linker symbols:

| Range | Permissions |
|---|---|
| `.text` | present, read + execute, no write |
| `.rodata` | present, read-only, NX |
| `.data` / `.bss` | present, writable, NX |
| other low/kernel-window memory | present, writable, NX |

The identity map covers only RAM, loader, ACPI, and runtime-service regions.
The PCI address hole, reserved firmware holes, MMIO, and unusable memory are
not mapped. The map ends at the highest needed region, rounded up to 2 MiB, and
is capped at 16 GiB. The hand-off block and region array are reserved and
validated for ordering and overlap before use.

Page-table frames are allocated from the frame allocator and zeroed before
use. They are reachable both before and after the switch because the new map
is identity-based.

## Per-task address spaces

Phase 2 adds `create_address_space`, which allocates a new PML4, copies the
kernel identity-map entry, and maps a private physical frame at
`PRIVATE_BASE = 0x0000_0080_0000_0000` (PML4 slot 1). Every task sees the same
virtual address backed by a different frame. The scheduler switches CR3 on
context switches, so a write by one task is invisible to another.

The kernel remains identity-mapped in every address space. This is enough for
kernel-task isolation but is not yet a user/kernel split; see
[scheduler.md](scheduler.md).

## Failure handling

If a page-table frame cannot be allocated, or an address cannot be encoded,
boot stops with `AETHER_BOOT_FAILED reason=paging: ...`. The kernel never
activates a partial map.

The IDT is installed before paging so a fault during memory initialization
prints a vector, error code, instruction pointer, RFLAGS, and CR2 diagnostic
rather than triple-faulting.

## Verified behavior

`make test` requires `AETHER_MM_OK` and `AETHER_BOOT_OK`. The boot log reports
`boot: cpu: NX and write-protect enabled`, the identity-map size, page-table
frame count, CR3, and free frames.

`make fault-test` builds a kernel that executes `int3` after enabling paging;
the test requires the IDT handler to report the breakpoint and emit
`AETHER_BOOT_FAILED`.

## Next steps

1. Higher-half kernel mapping and a per-address-space user region.
2. User/supervisor page permissions and user-mode entry.
3. Guard pages and stack overflow detection with an IST.
4. Dynamic bitmap sizing and identity mapping for memory above 16 GiB.
5. Locking/per-CPU allocator state.
6. Per-task kernel stacks allocated from frames.
7. A fault-injection test proving writes to kernel text and instruction fetches
   from data are rejected with NX and WP enabled.

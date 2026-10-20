# Boot architecture

## Flow

1. **Firmware.** OVMF scans the FAT image and loads
   `/EFI/BOOT/BOOTX64.EFI`, a PE32+ UEFI application.
2. **Loader entry.** `efi_main` initializes COM1 and prints
   `AETHER_LOADER_OK stage=entry`.
3. **Image validation.** The loader parses the embedded kernel ELF:
   - ELF64, little-endian, `ET_EXEC`, `EM_X86_64`;
   - fixed 64-byte header and 56-byte program headers;
   - at most 16 `PT_LOAD` segments;
   - every file range inside the embedded image;
   - `p_filesz <= p_memsz`, non-zero memory size, and no overlaps;
   - physical addresses above the first page and page-aligned consistently
     with file offsets;
   - an entry point inside a loadable, executable segment.
4. **Loading.** The loader allocates the image's aligned physical range with
   `AllocateAddress`, copies file bytes, and zeroes the remainder of each
   segment.
5. **Hand-off construction.** It allocates pages below 4 GiB for `BootInfo`, a
   64 KiB firmware-map buffer, and a fixed region array
   (`MAX_MEMORY_REGIONS = 256`). UEFI descriptors are converted to
   `MemoryRegion` entries, adjacent regions of the same kind are merged, and
   the result is checked for ordering and overlap.
6. **Boot-services exit.** `ExitBootServices` is retried a bounded number of
   times if firmware reports a stale map key. The final successful call is
   followed immediately by the kernel jump.
7. **Kernel entry.** `_start` disables interrupts, installs a 64 KiB stack, and
   calls `kernel_main` with the `BootInfo` pointer in `RDI`.
8. **Validation.** The kernel checks magic, version, structure size, region
   count, kernel range, entry point, and every region.
9. **Kernel ownership.** It installs a flat GDT and an exception IDT, enables
   NX and supervisor write protection, builds a bitmap frame allocator, creates
   kernel page tables, switches CR3, and only then releases boot-services
   memory.
10. **Execution.** It starts the timer and scheduler, runs the isolation,
    capability/IPC, sandbox, package, update, and portal-revocation checks, then
    loads the ring-3 application. After the boot markers it keeps the console
    shell alive for inspection.

See [memory.md](memory.md) for allocator and paging details.

## ABI

`aether-abi` defines `BootInfo` and `MemoryRegion`. The layout is `repr(C)`
with fixed-width fields and a size assertion in host tests. Any change to field
order, size, or semantics requires a `BOOT_INFO_VERSION` bump.

Rules:

- Pointers are **physical addresses**; the kernel starts on the identity map
  left by firmware.
- The kernel does not call UEFI boot services.
- Unused and reserved fields are zero; the kernel rejects non-zero reserved
  header fields.
- The region array is bounded and aligned; the loader fails closed rather than
  truncating.
- Regions are sorted by ascending physical start and must not overlap. Both the
  loader and kernel validate this.
- Memory kinds are stored as raw `u32` values and decoded with
  `MemoryKind::from_raw`, so an invalid discriminant becomes an error instead
  of undefined behaviour.

## Kernel image constraints

The kernel links at `0x04000000` and is built as a static `ET_EXEC` image with
a 64 KiB BSS stack. The loader does not process relocations. The kernel image
is mapped with 4 KiB pages so text is read-execute, rodata is read-only NX, and
data/BSS are writable NX. NX and `CR0.WP` are enabled explicitly before the new
tables are loaded. This keeps the first boot path small, but it also means:

- the load address is fixed and must remain free in firmware's map;
- the image must not require dynamic linking or position-independent
  relocations;
- the image is embedded in the loader, so the loader and kernel are rebuilt
  together.

Removing these constraints is future work (relocatable kernel, separate image
on the ESP, and a verified image format).

## QEMU target

`make test` runs:

```text
qemu-system-x86_64 -machine q35,accel=tcg -cpu max -m 1024M \
  -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=OVMF_VARS.fd \
  -drive file=build/aether.img,format=raw,if=virtio \
  -display vnc=127.0.0.1:9 -device VGA \
  -serial file:build/serial.log -no-reboot
```

The harness uses a headless VNC display and an emulated VGA adapter so OVMF
exposes a linear GOP framebuffer.

The harness waits for `AETHER_BOOT_OK`, fails on loader/kernel failure markers,
and prints the serial transcript. It uses TCG so it works without KVM or root.

## Failure behaviour

- Malformed or oversized images are rejected before any kernel bytes are
  written.
- Allocation failure, address mismatch, or a memory map that exceeds the
  bounded array stops in the loader with an `AETHER_LOADER_FAILED` line.
- Kernel validation failure prints `AETHER_BOOT_FAILED reason=...` and halts.
- A CPU without NX support fails with `AETHER_BOOT_FAILED reason=cpu: ...`
  rather than booting with executable data.
- A page fault, general-protection fault, or other CPU exception from ring 0
  prints `KERNEL EXCEPTION` with vector, error code, RIP, RFLAGS, and CR2, then
  emits `AETHER_BOOT_FAILED` and halts.
- The same diagnostic from ring 3 retires only the faulting user task; the
  kernel continues with the next runnable task.

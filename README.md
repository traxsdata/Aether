# Aether

Aether is an independent, capability-based desktop operating system for
x86-64 PCs. It is not a Linux distribution: the kernel, system interfaces, and
desktop are developed in this repository.

The project's security goal is simple to state and hard to build: applications
receive only the capabilities they need, user files are opened through explicit
grants, and the desktop is not a security authority.

> **Status: kernel prototype, Phase 2 in progress.** The tree now has a
> reproducible UEFI boot path, a validated physical memory map, a bitmap frame
> allocator, kernel-owned page tables with NX and write-protect enforcement,
> structured exception reporting, a 100 Hz preemptive scheduler,
> hardware-enforced address-space isolation between kernel tasks, per-task
> capability tables, and bounded IPC with capability transfer. There is no user
> mode, filesystem, graphics stack, or Windows compatibility yet. Do not use
> this tree for real data.

## What works today

- A `no_std` Rust UEFI loader that validates an embedded ELF64 kernel before
  copying it.
- A `no_std` x86-64 kernel linked at a fixed physical address with its own
  stack, GDT, IDT, serial console, and panic reporting.
- A bounded loader-to-kernel `BootInfo` ABI with magic, version, size, kernel
  range, and a merged physical memory map.
- A 16 GiB bitmap frame allocator that reserves the kernel and hand-off and
  releases firmware boot-services memory only after the CR3 switch.
- Kernel-owned four-level page tables: 4 KiB W^X mappings for the kernel image
  and 2 MiB or 4 KiB identity mappings for kernel-accessible RAM. NX and
  write-protect are enabled explicitly rather than inherited from firmware.
- Handlers for all 32 architectural exception vectors with vector, error code,
  RIP, RFLAGS, and CR2 diagnostics.
- A 100 Hz PIT/PIC timer interrupt, preemptive round-robin scheduler, and static
  task/thread control blocks.
- Per-task PML4 address spaces with a private physical frame mapped at the same
  virtual address, switched in CR3 by the scheduler.
- A boot-time isolation test that verifies each task's private mapping survives
  preemption by another task.
- Per-task capability tables with generation-checked handles, endpoint identity
  checks, endpoint rights, bounded IPC queues, and kernel-mediated capability
  transfer.
- A boot-time capability/IPC test covering successful transfer, insufficient
  rights, forged handles, and endpoint confusion.
- Host unit tests for ABI layout and region validation, memory arithmetic, the
  frame allocator, page-table encoding, capability tables, IPC endpoints, and a
  defensive ELF parser.
- `make test`, which boots the FAT image under QEMU with OVMF and requires
  `AETHER_MM_OK`, `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`, `AETHER_CAP_OK`,
  `AETHER_IPC_OK`, and `AETHER_BOOT_OK`.
- `make fault-test`, which executes `int3` after paging and requires the IDT
  handler to report the breakpoint.
- `make negative-test`, which proves the boot harness rejects a deliberately
  failing kernel.
- A local CI gate (`make ci`) that runs formatting, clippy, documentation and
  licence checks, unit tests, the boot test, the fault test, and the negative
  boot test. The same gate runs on every pull request in
  [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

A representative serial transcript from a successful boot (region counts and
frame addresses vary with firmware, build, and feature set):

```text
AETHER_LOADER_OK stage=entry version=0.1.0
loader: kernel elf start=0x4000000 end=0x40a9000 entry=0x4000000 segments=4
loader: memory regions=39 usable=1016 MiB
loader: boot services exited after 1 attempt(s)
AETHER_LOADER_HANDOFF entry=0x4000000
AETHER_KERNEL_OK stage=entry version=0.1.0 commit=...
boot: hand-off validated (version 1, 96 bytes)
boot: memory map: 39 regions, 1016 MiB allocatable
boot: kernel image [0x0000000004000000, 0x00000000040a9000), entry 0x0000000004000000
boot: cpu: NX and write-protect enabled
boot: paging: identity map to 1024 MiB, 6 page-table frames, CR3=0x0000000000100000
boot: frame allocator: 1015 MiB free (259944 frames), bitmap capacity 16384 MiB
AETHER_MM_OK
boot: x86-64 execution environment ready
sched: task 1 ready pml4=0x0000000000107000 private=0x0000000000106000
sched: task 2 ready pml4=0x000000000010c000 private=0x000000000010b000
sched: task 3 ready pml4=0x0000000000111000 private=0x0000000000110000
sched: timer started at 100 Hz
AETHER_SCHED_OK workers=3 ticks=23
AETHER_ISOLATION_OK tasks=3
AETHER_CAP_OK denials=2
AETHER_IPC_OK messages=3
AETHER_BOOT_OK stage=kernel version=0.1.0
```

## Build

Prerequisites on a Linux host:

- Rust 1.99.1 with the `x86_64-unknown-none` and `x86_64-unknown-uefi` targets
  (pinned by `rust-toolchain.toml`).
- `mtools` (`mformat`, `mmd`, `mcopy`).
- QEMU (`qemu-system-x86_64`) and OVMF firmware for `make test`.
- GNU Make and Python 3.

```sh
git clone https://github.com/traxsdata/Aether.git
cd Aether
. "$HOME/.cargo/env"        # if Rust is installed with rustup
make                        # build build/aether.img
```

`make` produces a 64 MiB FAT image containing
`/EFI/BOOT/BOOTX64.EFI`. The kernel ELF is embedded in the loader, so the two
artifacts cannot drift apart.

### Tests

```sh
make unittest      # host unit tests (ABI, ELF parser, memory arithmetic, allocator)
make test          # QEMU/OVMF boot test; prints the serial transcript and PASS
make fault-test    # exception-path test: int3 after paging must reach the IDT
make negative-test # boot harness must detect a deliberately failing kernel
make docs-check    # relative Markdown links
make license-check # workspace and dependency licence metadata
make lint          # rustfmt, host clippy, kernel clippy, loader clippy
make ci            # full pull-request gate
make clean
```

`make test` uses QEMU TCG and needs no KVM or root access. Override the
firmware paths with `OVMF_CODE` and `OVMF_VARS` if they are not in a standard
location.

## Repository layout

```text
abi/                 loader/kernel hand-off ABI (no_std, host-testable)
boot/common/         architecture-independent boot logic and ELF validation
boot/loader/         UEFI application (no_std, target x86_64-unknown-uefi)
kernel/memory/       host-testable frame allocator and page-table encoding
kernel/ipc/          host-testable capability tables and IPC endpoints
kernel/              Aether kernel (no_std, target x86_64-unknown-none)
  src/scheduler.rs   preemptive kernel scheduler, isolation and IPC tests
tools/               boot test harness
docs/                architecture, security, and decision records
```

The repository intentionally does not contain placeholder directories for
future services. Components are added when implementation starts, as required
by the specification.

## Boot overview

1. OVMF loads `/EFI/BOOT/BOOTX64.EFI` from the FAT image.
2. The loader parses the embedded kernel ELF and rejects malformed images
   before writing to memory.
3. It allocates the kernel at the image's linked physical address (`0x4000000`
   today), copies `PT_LOAD` segments, and zeroes BSS.
4. It allocates the hand-off block, map buffer, and region array below 4 GiB,
   converts the UEFI memory map into a fixed, bounded array, validates it, and
   writes a `BootInfo` block.
5. It exits boot services and jumps to the kernel with the `BootInfo` pointer.
6. The kernel installs its own GDT and IDT, validates the hand-off and region
   ordering, enables NX and write-protect, initializes the frame allocator,
   builds page tables, switches CR3, and reports memory and image state.
7. It starts a 100 Hz timer, preemptively runs three tasks in separate address
   spaces, verifies that private mappings survive preemption, exercises
   capability rights and bounded IPC including a transfer, prints
   `AETHER_MM_OK`, `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`, `AETHER_CAP_OK`,
   `AETHER_IPC_OK`, and `AETHER_BOOT_OK`, then halts.

The loader and kernel are separate programs with a versioned ABI. Internal
kernel structures are not exposed to the loader.

## Security posture

This prototype is **not** a sandbox yet. It validates and bounds the boot
hand-off and the kernel image parser, owns its page tables, explicitly enables
NX and write-protect so kernel text is read-execute-only and data is
non-executable, reports exceptions instead of resetting silently, isolates
kernel tasks in separate address spaces, and enforces generation-checked
capability rights and endpoint identity on bounded IPC. It still has no user
mode or user/kernel memory split, so kernel tasks share the kernel mapping and
all run at CPL0.

See [`docs/security/threat-model.md`](docs/security/threat-model.md) for the
current threat model, including what is deliberately out of scope for the
prototype.

## Contributing

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request. In
short: branch from `main`, keep commits focused, run `make ci`, and include the
exact commands and results in the PR description. Security-relevant changes
require explicit review.

Architecture decisions are recorded in [`docs/decisions/`](docs/decisions/).
The backlog and acceptance tests are in the
[technical specification](Technical%20Specification%20and%20Initial%20Development%20Backlog.txt).
The Windows compatibility feasibility plan is in
[`docs/specifications/windows-compatibility.md`](docs/specifications/windows-compatibility.md).

Further reading: [memory and paging](docs/architecture/memory.md),
[scheduler and address spaces](docs/architecture/scheduler.md),
[capabilities](docs/architecture/capabilities.md), [IPC](docs/architecture/ipc.md),
[security boundaries](docs/security/security-boundaries.md), and the
[development environment](docs/development.md).

## License

Apache License 2.0. See [`LICENSE`](LICENSE). The specification proposes a
final license review before a public release; this file records the current
project default, not a legal conclusion.

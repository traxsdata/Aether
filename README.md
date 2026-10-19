# Aether

Aether is an independent, capability-based desktop operating system for
x86-64 PCs. It is not a Linux distribution: the kernel, system interfaces, and
desktop are developed in this repository.

The project's security goal is simple to state and hard to build: applications
receive only the capabilities they need, user files are opened through explicit
grants, and the desktop is not a security authority.

> **Status: early kernel prototype.** The current tree implements the first
> sprint from the
> [technical specification](Technical%20Specification%20and%20Initial%20Development%20Backlog.txt):
> a reproducible UEFI boot path, a kernel hand-off with a validated physical
> memory map, and serial diagnostics. There is no scheduler, user space,
> filesystem, graphics stack, or Windows compatibility yet. Do not use this
> tree for real data.

## What works today

- A `no_std` Rust UEFI loader that validates an embedded ELF64 kernel before
  copying it.
- A `no_std` x86-64 kernel linked at a fixed physical address with its own
  stack, serial console, and panic reporting.
- A bounded loader-to-kernel `BootInfo` ABI with magic, version, size, kernel
  range, and a merged physical memory map.
- Host unit tests for ABI layout, memory arithmetic, and a defensive ELF parser.
- `make test`, which boots the FAT image under QEMU with OVMF and fails unless
  the kernel prints `AETHER_BOOT_OK`.
- A CI workflow that builds the kernel and loader and runs the same boot test.

The exact serial transcript from a successful boot is:

```text
AETHER_LOADER_OK stage=entry version=0.1.0
loader: kernel elf start=0x4000000 end=0x4013000 entry=0x4000000 segments=4
loader: memory regions=39 usable=1016 MiB
loader: boot services exited after 1 attempt(s)
AETHER_LOADER_HANDOFF entry=0x4000000
AETHER_KERNEL_OK stage=entry version=0.1.0 commit=...
kernel: handoff magic=0x534f524548544541 version=1 regions=39 usable=1016 MiB
kernel: image=0x4000000..0x4013000 entry=0x4000000
kernel: long mode, ring 0, interrupts disabled
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
make unittest   # host unit tests (ABI, ELF parser, memory arithmetic)
make test       # QEMU/OVMF boot test; prints the serial transcript and PASS
make lint       # rustfmt, host clippy, kernel clippy, loader clippy
make ci         # formatting + unit tests + boot test (the PR gate)
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
kernel/              Aether kernel skeleton (no_std, target x86_64-unknown-none)
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
4. It converts the UEFI memory map into a fixed, bounded array and writes a
   `BootInfo` block.
5. It exits boot services and jumps to the kernel with the `BootInfo` pointer.
6. The kernel validates the hand-off, reports memory and image state, prints
   `AETHER_BOOT_OK`, and halts.

The loader and kernel are separate programs with a versioned ABI. Internal
kernel structures are not exposed to the loader.

## Security posture

This prototype is **not** a sandbox yet. It validates and bounds the boot
hand-off and the kernel image parser, but it has no address-space isolation,
capability system, or user space to isolate. Serial output is diagnostic and
must not contain secrets.

See [`docs/security/threat-model.md`](docs/security/threat-model.md) for the
current threat model, including what is deliberately out of scope for the
prototype.

## Contributing

Read [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request. In
short: branch from `main`, keep commits focused, run `make unittest`,
`make test`, and `make lint`, and include the exact commands and results in the
PR description. Security-relevant changes require explicit review.

Architecture decisions are recorded in [`docs/decisions/`](docs/decisions/).
The backlog and acceptance tests are in the
[technical specification](Technical%20Specification%20and%20Initial%20Development%20Backlog.txt).

## License

Apache License 2.0. See [`LICENSE`](LICENSE). The specification proposes a
final license review before a public release; this file records the current
project default, not a legal conclusion.

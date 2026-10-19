# Aether

Aether is an independent, capability-based desktop operating system for
x86-64 PCs. It is not a Linux distribution: the kernel, system interfaces, and
desktop are developed in this repository.

The project's security goal is simple to state and hard to build: applications
receive only the capabilities they need, user files are opened through explicit
grants, and the desktop is not a security authority.

> **Status: native desktop prototype, Phase 3 complete and the Phase 4
> prototype in place.** The tree has a reproducible UEFI boot path and ISO, a validated
> physical memory map, kernel-owned page tables with NX and write-protect
> enforcement, a 100 Hz preemptive scheduler, a ring-3 user/kernel boundary,
> validated syscalls, a bounded filesystem with per-process roots and
> generation-checked file capabilities, supervised services, a console shell, a
> linear framebuffer compositor, keyboard input, a file portal, and a recovery
> prototype. Windows compatibility, networking, persistent storage, audio, and
> GPU acceleration are not implemented. Do not use this tree for real data.

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
- A ring-3 native application loaded from an embedded ELF image into a private
  PML4, with TSS/RSP0 kernel stacks, an `int 0x80` syscall gate, user-pointer
  validation, and fault containment.
- A bounded virtual block device and filesystem with directories, files, open
  handles, path traversal rejection, quotas, and per-process roots.
- Generation-checked file capabilities; `open`, `read`, `write`, `close`,
  `list`, and `stat` syscalls; and a kernel-mediated file portal grant.
- A host-tested service supervisor with dependency order, restart budgets, and
  quarantine, plus an application lifecycle table.
- A serial console shell with filesystem, service, application, recovery, and
  desktop commands.
- A linear GOP framebuffer service, software compositor, six-window desktop,
  PS/2 keyboard input, Security Center, file manager, settings, portal view,
  and current/previous image recovery view.
- A native application SDK (`sdk/`) for the initial syscall surface.
- Host unit tests for ABI layout and region validation, memory arithmetic, the
  frame allocator, page-table encoding, capability tables, IPC endpoints, the
  filesystem, service supervision, application lifecycle, and the defensive
  ELF parser.
- `make test`, which boots the FAT image under QEMU with OVMF and requires
  `AETHER_MM_OK`, `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`, `AETHER_CAP_OK`,
  `AETHER_IPC_OK`, `AETHER_APP_OK`, `AETHER_FS_OK`, `AETHER_SANDBOX_OK`,
  `AETHER_SUPERVISOR_OK`, `AETHER_RECOVERY_OK`, `AETHER_GRAPHICS_OK`,
  `AETHER_DESKTOP_OK`, `AETHER_INPUT_OK`, `AETHER_PORTAL_OK`,
  `AETHER_SHELL_OK`, and `AETHER_BOOT_OK`.
- `make shell-test`, which drives the console over a QEMU serial socket and
  checks filesystem, service, recovery, desktop, and input commands.
- `make fault-test`, which executes `int3` after paging and requires the IDT
  handler to report the breakpoint.
- `make negative-test`, which proves the boot harness rejects a deliberately
  failing kernel.
- `make iso`, which builds a timestamped UEFI El Torito ISO under
  `build/ISOs/`.
- A local CI gate (`make ci`) that runs formatting, clippy, documentation and
  licence checks, unit tests, the boot test, the shell test, the fault test, and
  the negative boot test. A GitHub Actions workflow will publish the same gate
  in a follow-up; until then, run the gate locally before opening a pull
  request.

A representative serial transcript from a successful boot (region counts and
frame addresses vary with firmware, build, and feature set):

```text
AETHER_LOADER_OK stage=entry version=0.1.0
loader: GOP 1280x800 stride=1280 format=1
loader: framebuffer base=0x80000000 size=4096000 mode=1280x800 stride=1280 format=1
loader: kernel elf start=0x4000000 end=0x41c4000 entry=0x4000000 segments=4
loader: memory regions=39 usable=1013 MiB
loader: boot services exited after 1 attempt(s)
AETHER_LOADER_HANDOFF entry=0x4000000
AETHER_KERNEL_OK stage=entry version=0.1.0 commit=...
boot: hand-off validated (version 2, 136 bytes)
boot: cpu: NX and write-protect enabled
boot: paging: identity map to 1024 MiB, CR3=0x0000000000100000
boot: framebuffer mapped 1280x800 stride=1280 format=1
AETHER_MM_OK
sched: user task ready pid=4 entry=0x0000020000000230 stack=0x0000020004000000
AETHER_SUPERVISOR_OK starts=11 running=9 quarantined=1
AETHER_RECOVERY_OK current=aether-0.1.0 previous=aether-0.0.9 selected=aether-0.1.0 rollback=false user_data=true
AETHER_GRAPHICS_OK mode=1280x800 format=linear
AETHER_DESKTOP_OK windows=6 checksum=0xc3c51a73
AETHER_INPUT_OK keys=6 dropped=0 queue=32
AETHER_SHELL_OK
AETHER_APP_ENTRY cpl=3
init: private write ok
init: sibling-private: denied EPERM
init: root-escape: denied EPERM
init: portal=welcome to aether
AETHER_PORTAL_OK
AETHER_APP_OK
AETHER_SCHED_OK workers=3 ticks=23
AETHER_ISOLATION_OK tasks=3
AETHER_CAP_OK denials=2
AETHER_IPC_OK messages=3
AETHER_USER_OK tasks=4 exits=1 faults=0
AETHER_FS_OK opens=4 denials=0
AETHER_SANDBOX_OK escapes=2 access_denials=0
AETHER_BOOT_OK stage=kernel version=0.1.0
```

## Build

Prerequisites on a Linux host:

- Rust 1.99.1 with the `x86_64-unknown-none` and `x86_64-unknown-uefi` targets
  (pinned by `rust-toolchain.toml`).
- `mtools` (`mformat`, `mmd`, `mcopy`).
- QEMU (`qemu-system-x86_64`) and OVMF firmware for `make test`.
- GNU Make and Python 3.
- `xorriso` for `make iso`.

```sh
git clone https://github.com/traxsdata/Aether.git
cd Aether
. "$HOME/.cargo/env"        # if Rust is installed with rustup
make                        # build build/aether.img
```

`make` produces a 64 MiB FAT image containing
`/EFI/BOOT/BOOTX64.EFI`. The kernel ELF is embedded in the loader, so the two
artifacts cannot drift apart. `make iso` builds a UEFI El Torito ISO containing
a small EFI system image and writes it to `build/ISOs/aether-<UTC>.iso`.

### Tests

```sh
make unittest      # host unit tests (ABI, ELF, filesystem, services, IPC, memory)
make test          # QEMU/OVMF boot test; prints the serial transcript and PASS
make shell-test    # drive the console shell over a QEMU serial socket
make fault-test    # exception-path test: int3 after paging must reach the IDT
make negative-test # boot harness must detect a deliberately failing kernel
make docs-check    # relative Markdown links
make license-check # workspace and dependency licence metadata
make lint          # rustfmt, host/kernel/user/loader clippy
make iso           # timestamped UEFI El Torito ISO under build/ISOs
make ci            # full pull-request gate
make clean
```

`make test` uses QEMU TCG and needs no KVM or root access. The graphics test
needs a display device for OVMF to expose GOP; the harness starts a headless
VNC server and an emulated VGA adapter. Override the firmware paths with
`OVMF_CODE` and `OVMF_VARS` if they are not in a standard location.

## Repository layout

```text
abi/                 boot ABI and user syscall ABI (no_std, host-testable)
boot/common/         architecture-independent boot logic and ELF validation
boot/loader/         UEFI application (no_std, target x86_64-unknown-uefi)
kernel/fs/           host-testable virtual block device and filesystem
kernel/memory/       host-testable frame allocator and page-table encoding
kernel/ipc/          host-testable capability tables and IPC endpoints
kernel/services/     host-testable supervisor and application lifecycle rules
kernel/              Aether kernel (no_std, target x86_64-unknown-none)
  src/scheduler.rs   scheduler, ring-3 process, isolation and IPC tests
  src/syscall.rs     validated user syscall gate
  src/desktop.rs     software compositor and desktop shell
  src/input.rs       PS/2 keyboard service
  src/portal.rs      scoped file portal grants
  src/recovery.rs    current/previous image selection
sdk/                 native application SDK
user/                ring-3 native init and sandbox test
tools/               boot, shell, ISO, and repository test harnesses
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
   converts the UEFI memory map into a fixed, bounded array, queries GOP, and
   writes a versioned `BootInfo` block.
5. It exits boot services and jumps to the kernel with the `BootInfo` pointer.
6. The kernel installs its own GDT, TSS, and IDT, validates the hand-off and
   region ordering, enables NX and write-protect, initializes the frame
   allocator, maps RAM and the framebuffer, switches CR3, and reports memory and
   image state.
7. It starts a 100 Hz timer, preemptively runs three ring-0 workers in separate
   address spaces, verifies that private mappings survive preemption, exercises
   capability rights and bounded IPC including a transfer, and loads a ring-3
   native application into its own PML4.
8. The application exercises its private root, denied sibling access, denied
   root escape, kernel-pointer rejection, and a portal-granted read.
9. The supervisor starts the filesystem, application, input, display, desktop,
   portal, and security services; the compositor paints the desktop; the input
   self-test drives focus; and recovery verifies the current/previous image
   records without touching user data. The console shell stays available after
   the boot markers.

The loader and kernel are separate programs with a versioned ABI. Internal
kernel structures are not exposed to the loader.

## Security posture

The prototype now has a real CPU privilege boundary: user code runs at CPL3 in
a private PML4 whose only user mappings are in the user window; kernel text,
stacks, page tables, and the identity map remain supervisor-only. System calls
validate every user range, length, handle, generation, object type, and right
before copying. A ring-3 fault retires only the faulting task. Files are opened
through generation-checked capabilities; a process root rejects `..`, and the
portal grants one exact outside file for one process and one right set.

The desktop is not a policy authority: it can display grants and request
capabilities, but the filesystem and portal services make the decision. Service
profiles are declared and the supervisor quarantines a service that exhausts
its restart budget.

This is still a prototype. Storage is a volatile RAM disk, there is one native
application, portal grants are seeded rather than interactively consented, and
there is no capability revocation UI, blocking IPC, SMP, networking, audio,
persistent storage, or GPU acceleration. Do not store real data on it.

See [`docs/security/threat-model.md`](docs/security/threat-model.md) and
[`docs/security/security-boundaries.md`](docs/security/security-boundaries.md)
for the current boundaries and deliberate exclusions.

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
[filesystem and file handles](docs/architecture/filesystem.md),
[compositor and desktop](docs/architecture/desktop.md),
[security boundaries](docs/security/security-boundaries.md), and the
[development environment](docs/development.md).

## License

Apache License 2.0. See [`LICENSE`](LICENSE). The specification proposes a
final license review before a public release; this file records the current
project default, not a legal conclusion.

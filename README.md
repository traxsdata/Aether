# Aether

Aether is an independent, capability-based desktop operating system for
x86-64 PCs. It is not a Linux distribution: the kernel, system interfaces, and
desktop are developed in this repository.

The project's security goal is simple to state and hard to build: applications
receive only the capabilities they need, user files are opened through explicit
grants, and the desktop is not a security authority.

> **Status: native desktop prototype. Phase 4's core deliverables are in
> place; interactive portal consent and its exit criterion (launching a
> packaged application from the desktop with an explicit consent grant) are
> still pending. The Phase 5/6 packaging, compatibility, update, and
> release-engineering slices are in place.**
> The tree has a reproducible UEFI boot path and ISO, a validated physical
> memory map, kernel-owned page tables with NX and write-protect enforcement, a
> 100 Hz preemptive scheduler, a ring-3 user/kernel boundary, validated
> syscalls including user-mode IPC endpoints, a bounded filesystem with
> per-process roots and generation-checked file capabilities, supervised
> services, a console shell, a linear framebuffer compositor, keyboard input, a
> file portal, a recovery prototype, signed package installation,
> per-application runtime profiles, a compatibility database, Ed25519 update
> verification with rollback protection, and a signed release bundle plus
> qualification report.
>
> Windows compatibility, networking, persistent storage, audio, GPU
> acceleration, a general installer, Secure Boot, and an external security
> review are not implemented. Do not use this tree for real data.

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
  rights, forged handles, and endpoint confusion, followed by a ring-3
  demonstration of the `endpoint_create`/`endpoint_send`/`endpoint_receive`
  syscalls: a round trip, queue-capacity `EAGAIN`, a forged handle, a file
  handle used as an endpoint, a `TRANSFER`-stripped receiver copy, and pool
  exhaustion.
- A ring-3 native application loaded from an embedded ELF image into a private
  PML4, with TSS/RSP0 kernel stacks, an `int 0x80` syscall gate, user-pointer
  validation, and fault containment.
- A bounded virtual block device and filesystem with directories, files, open
  handles, path traversal rejection, quotas, and per-process roots.
- Generation-checked file capabilities; `open`, `read`, `write`, `close`,
  `list`, and `stat` syscalls; a kernel-mediated file portal grant with
  exact-path revocation (`AETHER_REVOCATION_OK`); and enforcement of the
  verified package's declared open-file limit (`ENOSPC` on excess opens).
- A host-tested service supervisor with dependency order, restart budgets, and
  quarantine, plus an application lifecycle table.
- A serial console shell with filesystem, service, application, recovery, and
  desktop commands.
- A linear GOP framebuffer service, software compositor, seven-window desktop,
  PS/2 keyboard input, Security Center, file manager, settings, portal view,
  current/previous image recovery view, and a native GridCraft spreadsheet
  with a formula bar, cell editing, arithmetic, ranges, and five functions.
- A native application SDK (`sdk/`) for the initial syscall surface,
  including typed wrappers for the endpoint syscalls and error names.
- A dependency-free `aether-crypto` crate with SHA-256, SHA-512, and Ed25519
  verification and signing, tested against RFC 8032 and an independent
  implementation.
- A signed `.aether` package format (`aether-packaging`): canonical manifest,
  bounded payload archive, permission and limit allowlists, publisher trust
  store, and one verifier shared by the kernel and release tooling.
- Kernel installation of a verified package into a root derived from its
  identity, with boot-time rejection of an unknown publisher, a modified
  payload, and a zeroed signature (`AETHER_PACKAGE_OK`).
- Per-application runtime profiles (`aether-services::runtime`) with derived
  prefixes, declared permissions and limits, path containment, and explicit
  consent for prefix sharing (`AETHER_RUNTIME_OK`).
- A compatibility database and classification harness (`aether-compat`) with
  verified/compatible/limited/unsupported records and named-test evidence.
- A signed update manifest format (`aether-update`) with image digests,
  publisher pins, a monotonic rollback counter, channel policy, and deliberate
  recovery overrides; the boot test accepts one update and rejects replay,
  downgrade, and an untrusted key (`AETHER_UPDATE_OK`).
- `aether-admin`, a host tool that builds and signs packages and updates and
  assembles a release bundle. Signing keys are read from a file outside the
  repository; unsigned releases are refused.
- `make release` / `make release-check` for a signed ISO bundle with checksums
  and a signed manifest, `make release-demo` for the same flow with the
  checked-in test key plus a tamper acceptance test (a poisoned checksum entry
  and a flipped image byte must each be refused by `release verify`), and
  `make qualify` for a release report with commit, toolchain, emulator, step
  results, boot markers, and artifact digests.
- Host unit tests for ABI layout and region validation, memory arithmetic, the
  frame allocator, page-table encoding, capability tables, IPC endpoints, the
  filesystem, service supervision, application lifecycle, hashes, signatures,
  package policy, update policy, runtime policy, and the defensive ELF parser.
- `make test`, which boots the FAT image under QEMU with OVMF and requires
  `AETHER_MM_OK`, `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`, `AETHER_CAP_OK`,
  `AETHER_IPC_OK`, `AETHER_IPC_USER_OK`, `AETHER_APP_OK`, `AETHER_FS_OK`,
  `AETHER_SANDBOX_OK`, `AETHER_SUPERVISOR_OK`, `AETHER_RECOVERY_OK`,
  `AETHER_REVOCATION_OK`, `AETHER_PACKAGE_OK`, `AETHER_RUNTIME_OK`,
  `AETHER_UPDATE_OK`, `AETHER_GRAPHICS_OK`, `AETHER_DESKTOP_OK`,
  `AETHER_GRIDCRAFT_OK`, `AETHER_INPUT_OK`, `AETHER_PORTAL_OK`,
  `AETHER_SHELL_OK`, and `AETHER_BOOT_OK`.
- `make shell-test`, which drives the console over a QEMU serial socket and
  checks filesystem, service, memory, portal grant/revocation, package,
  runtime, update, recovery, desktop, spreadsheet export, and input commands.
- `make fault-test`, which executes `int3` after paging and requires the IDT
  handler to report the breakpoint.
- `make rollback-test`, which boots a kernel with the
  `recovery-rollback-selftest` feature: the current image marker is removed and
  the previous image must be selected with the user data still readable
  (`AETHER_RECOVERY_ROLLBACK_OK`).
- `make negative-test`, which proves the boot harness rejects a deliberately
  failing kernel.
- `make iso`, which builds a timestamped UEFI El Torito ISO under
  `build/ISOs/`.
- A CI gate (`make ci`) that runs formatting, clippy, documentation and
  licence checks, unit tests, the boot test, the shell test, the fault test,
  the rollback test, the negative boot test, and the release/tamper
  demonstration. A hosted GitHub Actions workflow is a separate follow-up.

A representative serial transcript from a successful `make test` boot (region
counts, frame addresses, and ELF extents vary with firmware, build, and feature
set; `commit=` is bracketed so this file does not have to be rebuilt):

```text
AETHER_LOADER_OK stage=entry version=0.1.0
loader: kernel elf start=0x4000000 end=0x41e6000 entry=0x4000000 segments=4
loader: GOP 1280x800 stride=1280 format=1
loader: framebuffer base=0x80000000 size=4096000 mode=1280x800 stride=1280 format=1
loader: memory regions=39 usable=1012 MiB
loader: boot services exited after 1 attempt(s)
AETHER_LOADER_HANDOFF entry=0x4000000
AETHER_KERNEL_OK stage=entry version=0.1.0 commit=...
boot: hand-off validated (version 2, 136 bytes)
boot: memory map: 39 regions, 1012 MiB allocatable
boot: cpu: NX and write-protect enabled
boot: paging: identity map to 1024 MiB, 9 page-table frames, CR3=0x0000000000100000
boot: framebuffer mapped 1280x800 stride=1280 format=1
graphics: framebuffer 1280x800 format=RedGreenBlueReserved
boot: graphics service ready
boot: frame allocator: 1011 MiB free (258919 frames), bitmap capacity 16384 MiB
AETHER_MM_OK
boot: x86-64 execution environment ready
sched: task 1 ready pml4=0x000000000010a000 private=0x0000000000109000
sched: task 2 ready pml4=0x000000000010f000 private=0x000000000010e000
sched: task 3 ready pml4=0x0000000000114000 private=0x0000000000113000
package: installed org.aether.demo 0.2.0 at /apps/org.aether.demo
AETHER_PACKAGE_OK installed=1 rejected=3 files=2
AETHER_RUNTIME_OK apps=1 isolated=1 prefix_checks=4 shared=0
AETHER_UPDATE_OK accepted=1 rejected=4 replay_protected=true counter=2 selected=0.2.0
AETHER_SUPERVISOR_OK starts=11 running=9 quarantined=1
AETHER_RECOVERY_OK current=aether-0.1.0 previous=aether-0.0.9 selected=aether-0.1.0 rollback=false user_data=true
portal: audit revoked root=/apps/org.aether.demo path=/portal/README.txt
AETHER_REVOCATION_OK checks=4
AETHER_GRIDCRAFT_OK checks=15 failures=0 sum_milli=10000
AETHER_GRAPHICS_OK mode=1280x800 format=linear
AETHER_DESKTOP_OK windows=7 checksum=0xd3be5264
desktop: activated window=SECURITY CENTER
AETHER_INPUT_OK keys=6 dropped=0 queue=32
Aether console shell. Type 'help' for commands.
AETHER_SHELL_OK
aether>
sched: user task ready pid=4 pml4=0x0000000000118000 entry=0x0000020000000510 stack=0x0000020004000000
sched: timer started at 100 Hz
AETHER_APP_ENTRY cpl=3
init: pid=4
init: yield/sleep ok
init: ipc round trip ok
init: ipc-too-long EINVAL
init: ipc-forged EBADF
init: ipc-queue-full EAGAIN
init: ipc-drain-empty EAGAIN
init: ipc-retransfer EACCES
init: ipc transfer chain ok
init: ipc-create-limit ENOSPC
init: private write ok
init: note=hello from ring 3
init: own-private: opened
init: sibling-private: denied EPERM
init: root-escape: denied EPERM
init: ipc-wrong-type EBADF
init: sibling-list EPERM
init: sibling-stat EPERM
init: open-file-limit ENOSPC
init: open-files ok
init: kernel-pointer-syscall=-2
init: portal=welcome to aether
AETHER_PORTAL_OK
init: portal-parent: denied ENOGRANT
init: portal-sibling: denied ENOGRANT
init: portal-write: denied ENOGRANT
AETHER_APP_OK
user: exit pid=4 status=0 closed=0
AETHER_SCHED_OK workers=3 ticks=23
AETHER_ISOLATION_OK tasks=3
AETHER_CAP_OK denials=2
AETHER_IPC_OK messages=3
AETHER_USER_OK tasks=4 exits=1 faults=0
AETHER_FS_OK opens=9 denials=0
AETHER_SANDBOX_OK escapes=4 access_denials=0 quota_denials=1
AETHER_IPC_USER_OK sends=7 receives=7 denials=7
AETHER_BOOT_OK stage=kernel version=0.1.0
```

The `AETHER_IPC_USER_OK` line is printed by the scheduler's boot gate after
counting the user task's IPC sends, receives, and denials; the user program
itself prints no success marker for IPC, so a broken kernel cannot fake it from
inside the sandbox.

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
make unittest      # host unit tests (ABI, ELF, filesystem, services, IPC,
                   # memory, crypto, packages, updates, compatibility)
make compat-test   # compatibility database and classification rules
make update-test   # package and update policy crates
make test          # QEMU/OVMF boot test; prints the serial transcript and PASS
make shell-test    # drive the console shell over a QEMU serial socket
make fault-test    # exception-path test: int3 after paging must reach the IDT
make rollback-test # recovery rollback branch test with its own marker
make negative-test # boot harness must detect a deliberately failing kernel
make docs-check    # relative Markdown links
make license-check # workspace and dependency licence metadata
make lint          # rustfmt, host/kernel/user/loader clippy
make iso           # timestamped UEFI El Torito ISO under build/ISOs
make release       # signed release bundle (requires a signing key)
make release-check # verify a release bundle against a trusted publisher key
make release-demo  # same flow with the checked-in test key, verify, and tamper test
make qualify       # full release gate and build/qualification/report.md
make ci            # pull-request gate
make clean
```

`make release` requires `AETHER_SIGNING_KEY` (a 32-byte hex seed file outside the
repository) and `AETHER_PUBLISHER`; it refuses to produce an unsigned bundle.
`make release-demo` uses the checked-in test key and proves the mechanism, not a
release trust root. `make qualify` runs every gate and records the commit,
toolchain, emulator, step results, boot markers, and artifact digests.

`make test` uses QEMU TCG and needs no KVM or root access. The graphics test
needs a display device for OVMF to expose GOP; the harness starts a headless
VNC server and an emulated VGA adapter. Override the firmware paths with
`OVMF_CODE` and `OVMF_VARS` if they are not in a standard location.

## Repository layout

```text
abi/                 boot ABI and user syscall ABI (no_std, host-testable)
boot/common/         architecture-independent boot logic and ELF validation
boot/loader/         UEFI application (no_std, target x86_64-unknown-uefi)
crypto/              SHA-256, SHA-512, and Ed25519 (no_std, host-tested)
packaging/           .aether container, manifest, and verifier (no_std)
updates/             signed update manifests and rollback policy (no_std)
kernel/fs/           host-testable virtual block device and filesystem
kernel/memory/       host-testable frame allocator and page-table encoding
kernel/ipc/          host-testable capability tables and IPC endpoints
kernel/services/     host-testable supervisor, lifecycle, and runtime profiles
kernel/              Aether kernel (no_std, target x86_64-unknown-none)
  src/scheduler.rs   scheduler, ring-3 process, isolation and IPC tests
  src/syscall.rs     validated user syscall gate
  src/packaging.rs   package verification, installation, and self-test
  src/runtime.rs     runtime profile registration and isolation self-test
  src/updates.rs     signed update verification and policy self-test
  src/desktop.rs     software compositor and desktop shell, GridCraft view
  src/input.rs       PS/2 keyboard service
  src/portal.rs      scoped file portal grants
  src/recovery.rs    current/previous image selection
sdk/                 native application SDK
tests/fixtures/      test keys and signed package/update fixtures
tests/compatibility/ compatibility database and classification harness
tools/               boot, shell, ISO, release, and qualification harnesses
tools/admin/         package/update/release signing tool (host only)
apps/gridcraft/      native spreadsheet application model (no_std, host-tested)
user/                ring-3 native init and sandbox test
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
   root escape, kernel-pointer rejection, a portal-granted read, the IPC
   endpoint syscalls (queue capacity, forged and wrong-type handles, transfer
   stripping, pool exhaustion), sibling listing and stat denials, and the
   open-file limit from its verified package.
9. The kernel verifies and installs an embedded signed package, checks that an
   unknown publisher, a modified payload, and a zeroed signature are rejected,
   derives the application's runtime profile, and verifies a signed update
   while rejecting replay, downgrade, and an untrusted publisher key.
10. The supervisor starts the filesystem, application, input, display, desktop,
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
Revoking a grant blocks future portal opens for that root/path (the console
command is `portal revoke <index>`); an already-open capability stays valid
until it is closed because revocation cannot undo bytes already read.

The desktop is not a policy authority: it can display grants and request
capabilities, but the filesystem and portal services make the decision. Desktop
and service code still runs in the kernel in this prototype, so that separation
is a code-structure rule, not an enforced user-space boundary. Service profiles
are declared and the supervisor quarantines a service that exhausts its restart
budget.

Packages and updates are authenticated before use: the kernel verifies an
Ed25519 signature over the manifest, the publisher is pinned in a trust store,
and the payload or image digest must match. A package cannot request network,
device, or system-service authority, and its storage root is derived from its
identity rather than declared by the manifest. Updates require a newer rollback
counter and version; a replay or downgrade is rejected unless a recovery action
explicitly allows an older version, and re-presenting the just-activated
manifest after activation must fail as a rollback. The user task's identity in
this prototype is the installed demo package: its verified manifest provides
the task's file scope, portal permissions, and open-file limit. That is a
verified-manifest profile, not the package's entry point running — packaged
entry points are still not executed as independent processes. A valid
signature says who produced an artifact, not that it is safe.

This is still a prototype. Storage is a volatile RAM disk, packages are not
persisted or executed from their entry points, update activation and the UEFI
installer are not implemented, the signing keys in `tests/fixtures` are test
keys, and there is no Secure Boot, measured boot, TPM counter, capability
revocation UI, blocking IPC, SMP, networking, audio, or GPU acceleration. The
serial console shell is a ring-0 diagnostic surface with full filesystem
authority, not the ordinary-user terminal required by the specification; it
does not provide an administrative elevation boundary. Do not store real data
on it.

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
[`docs/specifications/windows-compatibility.md`](docs/specifications/windows-compatibility.md)
and the host-interface review in
[`docs/specifications/windows-host-interface-report.md`](docs/specifications/windows-host-interface-report.md).

Further reading: [memory and paging](docs/architecture/memory.md),
[scheduler and address spaces](docs/architecture/scheduler.md),
[capabilities](docs/architecture/capabilities.md), [IPC](docs/architecture/ipc.md),
[filesystem and file handles](docs/architecture/filesystem.md),
[compositor and desktop](docs/architecture/desktop.md),
[GridCraft application](docs/architecture/gridcraft.md),
[package format](docs/specifications/package-format.md),
[signed updates](docs/architecture/updates.md),
[compatibility database](docs/specifications/compatibility.md),
[supported hardware](docs/reference/supported-hardware.md),
[installer and recovery](docs/reference/installer-and-recovery.md),
[security boundaries](docs/security/security-boundaries.md),
[security response](docs/security/security-response.md),
[community](docs/community.md), and the
[development environment](docs/development.md).

## License

Apache License 2.0. See [`LICENSE`](LICENSE). The specification proposes a
final license review before a public release; this file records the current
project default, not a legal conclusion.

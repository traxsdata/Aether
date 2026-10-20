# Linux compatibility layer

Aether's Linux layer runs unmodified Linux x86-64 applications, including
AppImages, without a virtual machine. It is a user-space compatibility service,
not a Linux kernel and not a second operating system. The goal is to provide
Linux semantics to the application while Aether keeps its own capability,
filesystem, lifecycle, and compositor boundaries.

The architectural decision is recorded in
[ADR-017](../decisions/ADR-017-linux-app-runtime.md); the requirement inventory
and gates are in
[Linux application compatibility](../specifications/linux-compatibility.md),
with per-facility owners and test cases in
[the host-interface inventory](../specifications/linux-host-interface-inventory.md).

## Layers

```text
 application (unmodified Linux ELF / AppImage payload)
        |
        v
 linuxd  ── ELF loader, libc ABI, /proc, signals, threads, Wayland client side
        |
        +── IPC ──> fsd / portal / waylandd / input / software renderer
        |
        v
 narrow Aether kernel primitives
 address space · thread create · futex wait/wake · fault delivery · syscall trap
```

### Application domain

Each application gets its own address space, private application root, writable
overlay, runtime profile, and capability table. The layer cannot see another
application's root or capabilities. A crash retires the domain, not the desktop.

### `linuxd`

`linuxd` is the Linux personality. It owns:

- AppImage import and the read-only application root;
- ELF and shared-library loading, `ld.so` interaction, TLS, and environment;
- Linux syscalls that can be emulated in user space;
- a bounded synthetic `/proc`, `/dev/null`, `/dev/urandom`, and similar nodes;
- thread creation requests, `futex` policy, and signal delivery;
- the client side of the Wayland protocol used by `winit`.

It never receives ambient filesystem, network, or device authority. It asks
`fsd` and the portal for handles, exactly like a native application.

### Kernel primitives

The kernel must expose a small set of operations that cannot be implemented
safely in user space: map/unmap/protect user pages, create and schedule
threads in an address space, wait and wake on a futex word with a timeout,
deliver faults as signals, and enter the Linux syscall path. These are
validated primitives, not Linux semantics. They are security-critical and need
their own threat-model review before they land.

### Wayland bridge

`waylandd` is an Aether service that speaks the subset of Wayland used by
`winit`/`egui`: registry, compositor, shared memory, xdg-shell, seat, output,
and clipboard. Buffers are validated for size, stride, format, and lifetime
before the Aether compositor presents them. The application does not receive a
framebuffer, a DRM device, or a GPU command stream.

### Graphics

The first GUI target uses a bundled software Vulkan (lavapipe) or software GL
path selected explicitly by the runtime profile. It is slow but reviewable and
does not require a GPU service. Accelerated graphics is a later service with
its own capability and device model. A missing renderer is a clean failure, not
an attempt to open a host device.

## Security controls

| Control | How it applies to the Linux layer |
|---|---|
| Capability model | The domain receives only the endpoints and handles named in its runtime profile. The layer cannot mint capabilities. |
| Application identity | The domain identity is derived from the verified package/AppImage, never from an application-provided string. |
| Private storage | The imported root is read-only; writes go to a per-application overlay. Other app roots are denied by `fsd`. |
| File portal | User files are opened through explicit grants. A Linux path such as `/home/user/file.xlsx` is a runtime mapping, not host authority. |
| Network | Denied by default. Sockets fail with a clear error unless a network capability is granted. |
| Devices | No `/dev/dri`, `/dev/input`, block devices, or raw PCI access. Input and display are service protocols. |
| Resource limits | Address-space, thread, file, and CPU limits are enforced per domain; exhaustion retires the domain. |
| Update integrity | AppImages and runtime packages are digest- and publisher-verified before import. |
| Crash containment | A fault in the layer or application terminates only that domain; unrelated apps and the desktop continue. |

## Compatibility boundary

The layer is deliberately incomplete. Unsupported Linux behaviour must fail
closed with `ENOSYS` or the nearest Linux error, be recorded in the
compatibility database, and never be silently approximated in a way that grants
authority. A compatibility claim needs a completed run, a named test suite, the
exact AppImage digest, and the runtime version.

## Current state

- AppImage container inspection: implemented in `apps/linux/appimage`, with
  host tests, pinned digest verification, signed import records with Ed25519
  publisher/trust-store checks, and the `appimage-inspect` /
  `appimage-manifest` diagnostics.
- SquashFS/zstd import: implemented in `apps/linux/squashfs`, with a synthetic
  fixture, a real-AppImage host extraction, and `squashfs-extract`.
- Linux ELF loading into a private PML4: implemented for the boot test fixture.
- Dynamic image loading: `aether-linux-elf` parses `ET_DYN`/`ET_EXEC` and
  `PT_INTERP`; `kernel/src/linux_loader.rs` maps a main image and interpreter,
  builds the initial stack with auxv, and enters the interpreter, which hands
  off to the main entry. Relocations and shared-library loading are not part of
  this slice; the test interpreter only validates auxv and transfers control.
- Linux `syscall` entry: implemented, with `write`, `exit`/`exit_group`,
  `mmap`, `munmap`, `mprotect`, `brk`, `arch_prctl` FS base, single-task futex,
  `clock_gettime`, `getrandom`, `uname`, `getpid`/`gettid`, and
  `set_tid_address`, and a read-only root with `openat`, `read`, `pread64`,
  `close`, `lseek`, `access`, `stat`/`fstat`/`newfstatat`/`statx`,
  `readlink`, `fcntl`, `dup`, and `dup2`. The static fixture
  `tests/fixtures/linux/hello.S` runs at CPL3, allocates and writes anonymous
  memory, maps `/etc/motd` through file-backed `mmap`, releases it with
  `munmap`, reuses the address with `MAP_FIXED`, grows the break, sets FS-based
  TLS, opens and reads `/etc/motd`, writes through a validated user pointer,
  exits cleanly, and gates the boot with `AETHER_LINUX_ROOT_OK`,
  `AETHER_LINUX_OVERLAY_OK`, `AETHER_LINUX_VMA_OK`, and `AETHER_LINUX_OK`.
  A fixed per-task VMA table tracks ranges and rejects writable+executable
  mappings. A runtime frame allocator backs `mmap` and `brk`; `munmap`, task
  exit, and faults unmap the pages and return their frames. `mremap`, page-fault
  handling, and shared mappings remain missing. The root is embedded and
  read-only; `/tmp` writes go to a bounded per-task overlay that is cleared on
  exit/fault. This is not yet an AppImage import or a persistent
  per-application store.
- Runtime rootfs packaging and signing: `tools/build-runtime-rootfs.sh` builds
  a zstd SquashFS image and signs it as a `linux-rootfs` record. The populated
  glibc/Mesa/Wayland rootfs remains missing.
- In-OS import: the kernel embeds a small signed `linux-rootfs` SquashFS image,
  verifies it against the test trust store, imports it with a 4 MiB bootstrap
  bump heap, and the Linux file syscalls read from the imported table
  (`AETHER_LINUX_IMPORT_OK`). This is the embedded boot fixture, not yet a
  user-supplied AppImage.
- SquashFS reader and extraction: implemented in `apps/linux/squashfs` for
  zstd, gzip, lz4, lzo, xz, and lzma in `no_std` (metadata, file data,
  fragments, directories, symlinks, hardlink marking, xattr dropping,
  extraction limits, host extractor). In-OS integration and persistent overlay
  storage remain missing.
- Dynamic glibc libraries, threads, real futex waiting, signals, file-backed
  mappings, and memory reclaim: not implemented.
- Wayland bridge and software renderer: not implemented.

The compatibility database records GridCraft 0.8.0 as `unsupported` until these
gates pass. A static test program is not an ArtCraft application.

# Development environment

## Host requirements

- Linux x86-64 (primary and CI platform)
- `git`, GNU Make, Python 3
- Rust via rustup (pinned by `rust-toolchain.toml`)
- `mtools` (`mformat`, `mmd`, `mcopy`)
- `qemu-system-x86_64` and OVMF firmware for the boot tests
- `xorriso` for `make iso`

Debian/Ubuntu:

```sh
sudo apt update
sudo apt install build-essential curl git make python3 qemu-system-x86 \
    ovmf mtools dosfstools xorriso
```

The first Cargo invocation downloads Rust 1.99.1 and the
`x86_64-unknown-none` / `x86_64-unknown-uefi` targets.

## Common commands

```sh
make unittest      # host tests for abi/, fs, services, memory, IPC, boot common,
                   # crypto, packaging, updates, compatibility
make test          # build the image and boot it under QEMU/OVMF
make shell-test    # drive the console shell over a QEMU serial socket
make fault-test    # boot a kernel that executes int3 after paging is enabled
make negative-test # prove the boot harness rejects a deliberately failing kernel
make compat-test   # compatibility database and classification rules
make update-test   # package and update policy crates
make docs-check    # check relative links in Markdown files
make license-check # check workspace and dependency licence metadata
make lint          # rustfmt + clippy (host, kernel, user, loader)
make iso           # timestamped UEFI El Torito ISO under build/ISOs
make release       # signed release bundle (needs AETHER_SIGNING_KEY)
make release-check # verify a release bundle (needs AETHER_TRUST)
make release-demo  # signed bundle with the test key, then verify
make qualify       # full release gate and build/qualification/report.md
make ci            # full pull-request gate
make               # build build/aether.img
make clean
```

`make test` requires the markers from `tools/test-boot.py`, including
`AETHER_USER_OK`, `AETHER_FS_OK`, `AETHER_SANDBOX_OK`, `AETHER_SUPERVISOR_OK`,
`AETHER_RECOVERY_OK`, `AETHER_REVOCATION_OK`, `AETHER_PACKAGE_OK`,
`AETHER_RUNTIME_OK`, `AETHER_UPDATE_OK`, `AETHER_GRAPHICS_OK`,
`AETHER_DESKTOP_OK`,
`AETHER_INPUT_OK`, `AETHER_PORTAL_OK`, and `AETHER_BOOT_OK`, and fails on any
failure marker. `make fault-test` requires the exception handler to report
`breakpoint (3)` and `AETHER_BOOT_FAILED`. All use TCG and need no KVM or root
access.

The graphics marker requires a display device for OVMF to expose GOP. The test
harnesses start a headless VNC server and an emulated VGA adapter. A completely
headless run prints `AETHER_GRAPHICS_SKIP` and does not satisfy the desktop
markers.

## Toolchain pinning

`rust-toolchain.toml` is the source of truth:

```toml
[toolchain]
channel = "1.99.1"
profile = "minimal"
components = ["rustfmt", "clippy"]
targets = ["x86_64-unknown-none", "x86_64-unknown-uefi"]
```

The kernel is linked by `kernel/kernel.ld` through `kernel/build.rs` at
`0x04000000` with identity-mapped segments. The loader embeds the kernel ELF at
build time (`boot/loader/build.rs`), so the image and the loader cannot drift
apart.

Check the resulting layout with:

```sh
readelf -h target/x86_64-unknown-none/release/aether-kernel
readelf -l target/x86_64-unknown-none/release/aether-kernel
```

## Build layout

```text
build/aether-kernel.elf   kernel copied from the Cargo target directory
build/aether-init.elf     ring-3 native program embedded in the kernel
build/BOOTX64.EFI         UEFI loader with the kernel embedded
build/aether.img          64 MiB FAT image with /EFI/BOOT/BOOTX64.EFI
build/efiboot.img         8 MiB FAT image used as the ISO's El Torito entry
build/ISOs/*.iso          timestamped UEFI ISO images
build/serial.log          last QEMU serial capture
```

## QEMU

`tools/test-boot.py` selects OVMF code/vars pairs from standard locations and
starts:

```text
qemu-system-x86_64 -machine q35,accel=tcg -cpu max -m 1024M
  -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE_4M.fd
  -drive if=pflash,format=raw,file=build/OVMF_VARS.fd
  -drive file=build/aether.img,format=raw,if=virtio
  -display vnc=127.0.0.1:9 -device VGA
  -serial file:build/serial.log -no-reboot
```

Set `OVMF_CODE` / `OVMF_VARS` to override firmware paths and
`AETHER_BOOT_TIMEOUT` to change the timeout.

## Debugging

- Serial output is the primary channel before and after `ExitBootServices`.
- `build/serial.log` contains the last automated boot output.
- The kernel prints a structured exception report with vector, error code,
  RIP, CS, RFLAGS, and CR2, then emits `AETHER_BOOT_FAILED`.
- Use QEMU `-d int,cpu_reset` or `-s -S` with GDB for instruction-level
  debugging. The boot harness can be adapted for an interactive run.
- If the loader fails, it prints `AETHER_LOADER_FAILED` with a reason. If the
  kernel rejects the hand-off, it prints `AETHER_BOOT_FAILED reason=...`.

## Package and update fixtures

The kernel embeds signed fixtures from `tests/fixtures/`:

```sh
sh tests/fixtures/build.sh   # rebuild packages, manifests, and manifests' digests
```

`keys/test.seed` and `keys/untrusted.seed` are test keys. The kernel trust store
embeds `keys/test.pub`; regenerating the seeds regenerates the public key, so the
two stay consistent. Never sign a release with a fixture key. See
[`tests/fixtures/README.md`](../tests/fixtures/README.md).

Signing and verification use the host tool:

```sh
cargo run -q -p aether-admin -- --help   # prints usage
cargo run -q -p aether-admin -- key public --seed /secure/key.seed
```

## Release bundles

```sh
make release AETHER_SIGNING_KEY=/secure/key.seed AETHER_PUBLISHER="Aether Contributors"
make release-check AETHER_TRUST="Aether Contributors:<public key hex>"
```

`make release` refuses to run without a key. `make release-demo` uses the test
key to exercise the same path in CI. `make qualify` runs every gate and writes
`build/qualification/report.md`; it is the release-manager input.

## Reproducibility

`make ci` from a clean checkout is the gate. The kernel and loader use pinned
dependencies (`--locked`) and a pinned toolchain. The build identifier embedded
in the kernel banner is the short Git commit, so images differ across commits
by design. The ISO embeds build timestamps, so a release records the signed
artifact digest (with `SHA256SUMS` and `release-info.txt`) instead of claiming
bit-for-bit image reproduction.

## CI

`tools/ci.sh` is the pull-request gate. The same script is intended to run in
GitHub Actions; until the workflow is published, run it locally before opening
a pull request:

1. `make lint` — formatting and clippy for host, kernel, loader, and the fault
   self-test configuration.
2. `make docs-check` — relative Markdown links.
3. `make license-check` — workspace and dependency licence metadata.
4. `make unittest` — host unit tests.
5. `make compat-test` and `make update-test` — compatibility classification and
   package/update policy.
6. `make test` — real QEMU boot with the full marker set, including the ring-3,
   filesystem, sandbox, supervisor, recovery, package, runtime, update,
   graphics, desktop, input, portal, and shell markers.
7. `make shell-test` — drive `help`, `ls`, `cat`, `apps`, `security`,
   `services`, `portal`/`portal revoke`, `recovery`, `packages`, `runtime`,
   `updates`, `desktop`, `input`, and `uptime` over a serial socket.
8. `make fault-test` — deliberate `int3` after paging, requiring the IDT report
   and failure marker.
9. `make negative-test` — a deliberately failing kernel must be rejected by the
   same harness used for the normal boot test.

`make qualify` adds `make release-demo` and writes a report; run it before a
release, not on every pull request.

The gate does not require KVM, root access, or secrets. A GitHub Actions
workflow will install `qemu-system-x86`, `ovmf`, and `mtools` and run the same
script once it is published.

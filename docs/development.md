# Development environment

## Host requirements

- Linux x86-64 (primary and CI platform)
- `git`, GNU Make, Python 3.11 or later
- Rust via rustup (pinned by `rust-toolchain.toml`)
- `mtools` (`mformat`, `mmd`, `mcopy`)
- `qemu-system-x86_64` and OVMF firmware for the boot tests
- `xorriso` for `make iso`

Debian/Ubuntu:

```sh
sudo apt update
sudo apt install build-essential curl git make python3 qemu-system-x86 \
    ovmf mtools dosfstools xorriso squashfs-tools
```

The first Cargo invocation downloads Rust 1.99.1 and the
`x86_64-unknown-none` / `x86_64-unknown-uefi` targets.

## Common commands

```sh
make unittest      # Rust host tests (including input/browser/actual renderer)
                   # and Python build-tooling tests
make test          # build the image and boot it under QEMU/OVMF
make large-apps-test # the same boot with Linux-sized task/descriptor/VMA limits
make glibc-test    # opt-in: package host glibc + hello and boot through ld.so
make gate3-test    # stage the pinned GridCraft AppImage and run its headless CLI
make photocraft-test # stage the pinned PhotoCraft AppImage and run its headless CLI
python3 tools/test-craft-apps.py            # download, verify, and smoke-test every Craft app
python3 tools/test-craft-apps.py --no-gui pdfcraft  # one app, headless only
make craft-smoke-test CRAFT_ID=... CRAFT_VERSION=... CRAFT_APPIMAGE=... \
    CRAFT_PIN=... CRAFT_PROGRAM=/usr/bin/app CRAFT_ARGS=--version
make craft-gui-test CRAFT_ID=... CRAFT_VERSION=... CRAFT_APPIMAGE=... \
    CRAFT_PIN=... CRAFT_PROGRAM=/usr/bin/app
make iso-test      # UEFI ISO, VGA/SVGA, actual PS/2 windows and Files workflows
make shell-test    # drive the console shell over a QEMU serial socket
make fault-test    # boot a kernel that executes int3 after paging is enabled
make crash-screen-test # a kernel page fault and a panic must paint the crash report
make negative-test # prove the boot harness rejects a deliberately failing kernel
make compat-test   # compatibility database and classification rules
make update-test   # package and update policy crates
make docs-check    # check relative links in Markdown files
make license-check # check workspace and dependency licence metadata
make lint          # rustfmt + clippy (host, kernel, user, loader)
make iso           # timestamped UEFI El Torito ISO under build/ISOs
make vmware-vmx    # write build/aether-vmware.vmx for the newest ISO
make release       # signed release bundle (needs AETHER_SIGNING_KEY)
make release-check # verify a release bundle (needs AETHER_TRUST)
make release-demo  # signed bundle with the test key, then verify
make qualify       # full release gate and build/qualification/report.md
make ci            # full pull-request gate
make               # build build/aether.img
make clean
```

`make test` requires the markers from `tools/boot_support.py`, including
`AETHER_USER_OK`, `AETHER_FS_OK`, `AETHER_SANDBOX_OK`, `AETHER_SUPERVISOR_OK`,
`AETHER_RECOVERY_OK`, `AETHER_REVOCATION_OK`, `AETHER_PACKAGE_OK`,
`AETHER_RUNTIME_OK`, `AETHER_UPDATE_OK`, `AETHER_GRAPHICS_OK`,
`AETHER_DESKTOP_OK`, `AETHER_FILES_OK`, `AETHER_WM_OK`, `AETHER_POINTER_OK`, `AETHER_INPUT_OK`,
`AETHER_FILES_MUTATE_OK`, `AETHER_PORTAL_OK`, `AETHER_PORTAL_CONSENT_OK`, and `AETHER_BOOT_OK`, and fails on any failure marker.
`make iso-test` boots the same distribution ISO through OVMF on VGA and QEMU's
VMware SVGA adapter and requires the full boot/security marker set. It then
drives real PS/2 arrows, dragging/resizing, Files folders/previews, denied vault
access, menus/search, cursor-only painting, RGB colors, and partial/full
screenshot equality. `make fault-test` requires the exception handler to report
`breakpoint (3)` and `AETHER_BOOT_FAILED`. All use TCG and need no KVM or root
access.

The graphics marker requires a display device for OVMF to expose GOP. The test
FAT-image harness starts a headless VNC server and a VGA adapter; the ISO harness
uses `-display none` with an explicit VGA/SVGA device and QMP screenshots. No
graphical host session is needed. A run with no graphics **device** cannot
satisfy the desktop markers.

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
build/aether-vmware.vmx   VMware Workstation definition for the newest ISO
build/aether-vmware-serial.log  Workstation COM1 capture after a manual boot
build/iso-test/<adapter>/ serial/QMP interaction logs, PPM screenshots, timings
build/iso-test/artifact.json  tested ISO path, SHA-256 and result
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

`make iso-test` uses `AETHER_ISO_TIMEOUT` for boot and accepts `AETHER_ISO` for
an explicit artifact. To re-test an existing ISO without rebuilding, run
`AETHER_ISO=/absolute/path/aether.iso python3 tools/test-iso.py`. The VMX generator
also accepts `AETHER_ISO`; `make vmware-vmx` does not rebuild a previously tested
ISO. The kernel banner marks modified tracked sources with `-dirty`; a PR still
needs a committed, exact tested SHA before publishing its evidence.

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
embeds `keys/test.pub` unless `AETHER_TRUST_PUBLIC_KEY_FILE` names another key
at build time, and boots print `AETHER_TRUST_WARNING` while it is the test key;
regenerating the seeds regenerates the public key, so the
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
4. `make unittest` — Rust host tests, actual renderer surface checks, and Python tooling tests.
5. `make compat-test` and `make update-test` — compatibility classification and
   package/update policy.
6. `make test` — real QEMU boot with the full marker set, including the ring-3,
   filesystem, sandbox, supervisor, recovery, package, runtime, update,
   graphics, desktop, window manager, pointer, input, portal, and shell markers.
7. `make iso-test` — boot the same ISO through UEFI El Torito on VGA and VMware
   SVGA emulation; exercise real window/Files input and compare screenshots.
   `make vmware-vmx` then generates the Workstation definition for that ISO.
8. `make shell-test` — drive `help`, `ls`, `cat`, `apps`, `security`, `mem`,
   `services`, `portal`/`portal approve`/`portal deny`/`portal revoke`, `cp`, `mv`, `recovery`, `packages`, `runtime`,
   `updates`, `desktop`, `input`, and `uptime` over a serial socket.
9. `make fault-test` — deliberate `int3` after paging, requiring the IDT report
   and failure marker. `make crash-screen-test` then page-faults in the kernel
   after the desktop is up, and panics right after graphics init; each boot
   must print `AETHER_CRASH_SCREEN shown=1` and a QEMU screendump must show
   the blue crash report with text on it.
10. `make rollback-test` — discard the current image marker and require the
   previous image with its user data, printing
   `AETHER_RECOVERY_ROLLBACK_OK`.
11. `make negative-test` — a deliberately failing kernel must be rejected by
    the same harness used for the normal boot test.

12. `make release-demo` — signed bundle verification and both tamper rejections.

`make qualify` runs the same gates, including rollback and the signed-release
demo, and writes a report; run it before a release. Neither gate qualifies
VMware Workstation or replaces an external security/accessibility review.

The gate does not require KVM, root access, or secrets. A GitHub Actions
workflow will install `qemu-system-x86`, `ovmf`, and `mtools` and run the same
script once it is published.

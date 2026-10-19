# Development environment

## Host requirements

- Linux x86-64 (primary and CI platform)
- `git`, GNU Make, Python 3
- Rust via rustup (pinned by `rust-toolchain.toml`)
- `mtools` (`mformat`, `mmd`, `mcopy`)
- `qemu-system-x86_64` and OVMF firmware for the boot tests

Debian/Ubuntu:

```sh
sudo apt update
sudo apt install build-essential curl git make python3 qemu-system-x86 \
    ovmf mtools dosfstools
```

The first Cargo invocation downloads Rust 1.99.1 and the
`x86_64-unknown-none` / `x86_64-unknown-uefi` targets.

## Common commands

```sh
make unittest      # host tests for abi/, boot/common/, kernel/memory/, kernel/ipc/
make test          # build the image and boot it under QEMU/OVMF
make fault-test    # boot a kernel that executes int3 after paging is enabled
make negative-test # prove the boot harness rejects a deliberately failing kernel
make docs-check    # check relative links in Markdown files
make license-check # check workspace and dependency licence metadata
make lint          # rustfmt + clippy (host, kernel, loader)
make ci            # full pull-request gate
make               # build build/aether.img
make clean
```

`make test` requires `AETHER_MM_OK`, `AETHER_SCHED_OK`,
`AETHER_ISOLATION_OK`, `AETHER_CAP_OK`, `AETHER_IPC_OK`, and `AETHER_BOOT_OK`
and fails on any failure marker. `make fault-test` requires the exception
handler to report `breakpoint (3)` and `AETHER_BOOT_FAILED`. Both use TCG and
need no KVM or root access.

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
build/BOOTX64.EFI         UEFI loader with the kernel embedded
build/aether.img          64 MiB FAT image with /EFI/BOOT/BOOTX64.EFI
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
  -display none -serial file:build/serial.log -no-reboot
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

## Reproducibility

`make ci` from a clean checkout is the gate. The kernel and loader use pinned
dependencies (`--locked`) and a pinned toolchain. The build identifier embedded
in the kernel banner is the short Git commit, so images differ across commits
by design. Fully reproducible images (fixed timestamps, signing, checksums)
remain release-engineering work.

## CI

`tools/ci.sh` is the pull-request gate. The same script is intended to run in
GitHub Actions; until the workflow is published, run it locally before opening
a pull request:

1. `make lint` — formatting and clippy for host, kernel, loader, and the fault
   self-test configuration.
2. `make docs-check` — relative Markdown links.
3. `make license-check` — workspace and dependency licence metadata.
4. `make unittest` — host unit tests.
5. `make test` — real QEMU boot with required `AETHER_MM_OK`,
   `AETHER_SCHED_OK`, `AETHER_ISOLATION_OK`, `AETHER_CAP_OK`, `AETHER_IPC_OK`,
   and `AETHER_BOOT_OK` markers.
6. `make fault-test` — deliberate `int3` after paging, requiring the IDT report
   and failure marker.
7. `make negative-test` — a deliberately failing kernel must be rejected by the
   same harness used for the normal boot test.

The gate does not require KVM, root access, or secrets. A GitHub Actions
workflow will install `qemu-system-x86`, `ovmf`, and `mtools` and run the same
script once it is published.

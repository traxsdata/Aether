# Migrating Aether onto the rlinux kernel

## Decision

Aether will stop using its own from-scratch microkernel (`kernel/`) and boot
on [rlinux](https://github.com/traxsdata/rlinux) instead — mainline Linux,
with drivers/subsystems incrementally converted to Rust. Aether's desktop,
window manager, compositor, filesystem layer, and apps become userspace
processes on top of that kernel instead of running under Aether's own
syscall/IPC ABI.

This supersedes the "not a Linux distribution" framing in `README.md` and
the phased plan in `docs/roadmap.md`, which describe the custom microkernel.
Those documents are rewritten in Phase 4 below, once the new boot path is
proven, not before — the old kernel stays the buildable, working thing until
there is a real replacement for every piece of it.

## Phase 0: building rlinux

The first Phase 0 step is a reproducible build of the pinned rlinux kernel
that Aether can later boot (#185; booting itself is #186). It adds build
tooling only — no change to `kernel/`, `boot/`, or the existing image:

- `tools/pins/rlinux.pin` — the pinned rlinux repo URL and full commit SHA
  (`main` = kernel 7.3.0 at the time of pinning).
- `tools/rlinux/aether.config` — Aether's QEMU/VM device profile as a config
  fragment (EFI boot, console, PS/2 input, stdvga + EFI simple framebuffer,
  virtio-blk, e1000/e1000e), every symbol `=y`, on top of
  `x86_64_defconfig`.
- `tools/build-rlinux-kernel.sh` — fetches the source for the pinned SHA
  (reused per-SHA from `build/rlinux/src`, from the shared git mirror at
  `/opt/aether-shared/rlinux-cache/rlinux.git` when available, otherwise
  from the GitHub archive), configures, checks that **every** fragment
  symbol is `=y` in the final `.config`, and builds `bzImage`.

Exact commands:

```sh
# the build (works from a clean tree; reuses build/rlinux/src when the
# recorded SHA matches)
make rlinux-kernel

# equivalent, without make:
sh tools/build-rlinux-kernel.sh

# without the shared mirror (e.g. CI): falls back to the archive download
RLINUX_MIRROR=/nonexistent sh tools/build-rlinux-kernel.sh
```

Inside the source tree the kernel is configured and built as:

```sh
make LLVM=-21 x86_64_defconfig
sh scripts/kconfig/merge_config.sh -m .config tools/rlinux/aether.config
make LLVM=-21 olddefconfig
make LLVM=-21 rustavailable          # must print "Rust is available!"
make -j"$(nproc)" LLVM=-21 bzImage
```

`LLVM=-21` is this machine's spelling: it ships clang-21/lld-21 without a
plain `clang`, and the kernel maps `LLVM=-21` to `clang-21` (its
`Documentation/kbuild/llvm.rst`); `LLVM=1` would look for plain `clang`.
The product is `build/rlinux/bzImage` (`CONFIG_RUST=y` verified). The target
is deliberately **not** part of `make ci` / `tools/ci.sh` while it is Phase 0
build-time work.

## Why this is a large migration, not a kernel swap

Aether's `kernel/` is ~28k lines and is not just a kernel in the Linux sense:
it also owns the syscall ABI, capability/IPC model, scheduler, a bounded
filesystem with per-process roots, and the services supervisor that the
desktop, window manager (`kernel/wm/`), and compositor are built directly
against. None of that exists in the same shape under Linux:

- **Syscalls & capabilities** → Aether's custom syscalls (`kernel/src/syscall.rs`,
  3,678 lines) and capability-based file grants have no Linux equivalent;
  the userspace code calling them has to be rewritten against POSIX/Linux
  syscalls, or against a compatibility shim that emulates the capability
  model in userspace (e.g. a privileged helper daemon + seccomp, or a FUSE
  filesystem for the per-process roots).
- **IPC** → `kernel/ipc/` and the services supervisor (`kernel/services/`,
  `kernel/src/supervisor.rs`) are replaced by whatever IPC Aether's daemons
  use on Linux (D-Bus, Unix sockets, or a purpose-built protocol).
- **Window manager & compositor** → `kernel/wm/` currently talks to the
  kernel's own display/input primitives. On rlinux it becomes a userspace
  compositor (e.g. over DRM/KMS + libinput, the way Wayland compositors
  work), which is a different codebase shape even if some logic (Spaces,
  Spotlight-style search) carries over conceptually.
- **Filesystem** → the bounded, per-process-root, generation-checked
  filesystem (`kernel/fs/`) either becomes a userspace layer on top of a
  real Linux filesystem, or is dropped in favor of standard Linux
  permissions/namespaces, which changes the security model Aether's specs
  currently promise.
- **Boot** → rlinux's own UEFI boot path (mainline Linux + its existing
  bootloader story) replaces Aether's `boot/loader`.
- **Packaging/updates** (`packaging/`, `updates/`) assume Aether's own
  signed-bundle format; whether that's kept as a layer on top of a Linux
  rootfs or replaced is a Phase 3 decision below, not a Phase 1 one.

## Phased plan

Each phase is its own set of tracked issues; a phase does not start until
the previous one's exit criterion is met and reviewed. Nothing in `kernel/`
is deleted before Phase 4, so the existing build/boot/CI keeps working
throughout — if the migration stalls, Aether is not left unbootable.

### Phase 0 — Feasibility spike (current)
- Boot rlinux to a shell on the target hardware/QEMU profile Aether already
  tests against. Reuse rlinux's own boot path, not Aether's `boot/loader`.
- Confirm `CONFIG_RUST=y` builds and the minimal driver set Aether's current
  hardware support list needs (PS/2 keyboard/mouse, the display path, disk)
  is present or portable from rlinux's candidate queue
  ([ROADMAP.md](https://github.com/traxsdata/rlinux/blob/main/ROADMAP.md)).
- Exit criterion: a documented, reproducible QEMU boot of rlinux reaching a
  shell, with the hardware Aether needs enumerated and present.

### Phase 1 — Userspace proof of concept
- Pick ONE existing Aether userspace piece with the smallest Linux-porting
  surface (candidate: the console shell, `kernel/term/` — no compositor, no
  WM) and get an equivalent running as an ordinary process on rlinux.
- Decide the syscall/capability compatibility story here, concretely, for
  just that one piece — don't design the general answer in the abstract.
- Exit criterion: that one component runs on rlinux, with a written record
  of exactly what Linux syscalls/libraries it needed and what Aether
  capability-model guarantees were kept, weakened, or dropped to get there.

### Phase 2 — Compositor & window manager
- Port `kernel/wm/` and the compositor to run over DRM/KMS + libinput (or
  an existing Wayland compositor library) instead of Aether's kernel
  display/input primitives.
- Exit criterion: the existing WM feature set (Spaces, Spotlight-style
  search) demonstrably working in a window on rlinux, QEMU-tested the same
  way `make iso-test`/`make gate4-test` test today's build.

### Phase 3 — Filesystem, packaging, services
- Decide and implement the filesystem/capability compatibility layer for
  real (informed by Phase 1's findings), port `packaging/` and `updates/`
  to a Linux rootfs, port the services supervisor to a Linux-native
  equivalent (systemd user services, or a small custom supervisor daemon).
- Exit criterion: a signed, installable Aether image that boots rlinux and
  reaches the full desktop with packaging/update flows working, parallel to
  (not yet replacing) the existing custom-kernel image.

### Phase 4 — Cutover
- Only once Phase 3's image has been dogfooded and passes the same gates
  (`make ci`, `make gate4-test`, etc.) as the current build: delete
  `kernel/`, `boot/loader`, and the now-dead custom-kernel build targets;
  rewrite `README.md` and `docs/roadmap.md` to describe Aether as built on
  rlinux; update `CONTRIBUTING.md`'s contributor workflow accordingly.
- This is the only phase that is actually destructive to the current tree,
  and it happens last, deliberately.

## Non-goals right now

- Do not attempt Phase 2+ work before Phase 0/1 have real, tested results.
- Do not delete or stop maintaining `kernel/` until Phase 4.
- Do not weaken the capability-model security promises in the specs without
  writing down, in the relevant phase's issue, exactly what was traded away
  and why — that is a decision for the project owner, not something a PR
  should do quietly.

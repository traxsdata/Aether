# PR #20 Gate 4 review — windowed GridCraft

**Status:** Gate 4 met at QEMU prototype scope on 2026-10-21 at tested code
commit `716dcb4`, revalidated at `ba6544c` (a docs-only descendant). This
review records only evidence that has actually run. It is not a compatibility
claim beyond the named tests.

Related: [Linux compatibility](../specifications/linux-compatibility.md),
[host-interface inventory](../specifications/linux-host-interface-inventory.md),
[ADR-017](../decisions/ADR-017-linux-app-runtime.md),
[Linux layer](../architecture/linux-layer.md).

## Scope

Gate 4 is met at QEMU prototype scope only when all of the following run in the
same boot, against the pinned GridCraft 0.8.0 artifact
(`942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`,
19,024,376 bytes):

1. `usr/bin/gridcraft` starts unmodified with a Wayland client connection and a
   software renderer (lavapipe/LLVMpipe), with no `/dev/dri`, host GPU, or X11.
2. Its window is presented in the Aether compositor with a non-blank frame
   checksum (and a QEMU screenshot when the harness runs interactively).
3. Real Aether keyboard and pointer input reaches the focused Linux window.
4. A workbook is opened and saved through an exact-file portal grant, and the
   grant is revoked afterwards; a fresh open fails closed.
5. The kernel emits the Gate 4 markers from validated state, not guest output.

## Prototype architecture

- The runtime rootfs carries libwayland-client ABI compatibility (the bridge),
  libxkbcommon, the Vulkan loader and lavapipe ICD, and the LLVM/driver
  closure. The rootfs is signed and digest-pinned; the pin is host-derived.
- The bridge is a Wayland-compatible client implementation, not a Wayland wire
  server. It maps surfaces and buffers to a narrow Aether display endpoint
  (`/dev/aether-display`) and receives bounded input events from the kernel.
- The kernel validates every present range, copies pixels into a compositor
  surface, and routes input only while the Linux window is focused.
- The application root is read-only; writable files live in the private
  overlay. User files are reachable only through the portal namespace
  `/run/aether/portal/<grant-id>/<file>`.
- The compositor adapter is still ring 0. This is a prototype containment
  boundary, not the final ring-3 compositor.

## Host feasibility (not Aether evidence)

A host-only spike ran the real GUI under a minimal libwayland-server compositor
with five globals (compositor, shm, seat, output, xdg_wm_base) and lavapipe.
The full spreadsheet UI rendered at 1440×900 from `wl_shm` ARGB8888 buffers.
This proves the client/protocol/renderer subset is viable; it does not prove
Aether.

## Evidence

- [x] Exact commits tested: `716dcb4` (`test(linux): synchronize the wait4
      restart self-test`) and its docs-only descendant `ba6544c`. The
      revalidation at `ba6544c` reproduced the marker set and screenshot
      hashes below.
- [x] `make gate4-test` passed on `qemu-q35-ovmf` (software VGA, no GPU, no
      X11/D-Bus), 2026-10-21, against the pinned GridCraft 0.8.0 AppImage
      digest
      `942096a29a534aa2503e9e7aa1816e952f7250e73d55f8f32afd077547617f77`
      (19,024,376 bytes) and runtime profile `gate4-host-lavapipe` (Mesa
      `lavapipe-26.0.8-1ubuntu0.3`, Wayland `1.24.0-2`).
- [x] Marker transcript, captured from the run serial log:

```text
AETHER_GATE4_DISPLAY_CREATE_OK pid=7 size=1440x900
AETHER_GATE4_PRESENT_OK frames=1 checksum=0x7b2cafc5 source=linux
AETHER_GATE4_INPUT_OK keys=1 pointer=24 focus=3
AETHER_GATE4_SAVE_OK file=/home/gate4-gui.xlsx bytes=4093 staging=sctmp+rename source=kernel
AETHER_GATE4_PORTAL_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_bytes=4089 source=kernel
AETHER_GATE4_REVOKE_OK path=/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx read_denials=1 write_denials=1 source=kernel
AETHER_BOOT_OK stage=kernel version=0.1.0
```

- [x] Present marker: `AETHER_GATE4_PRESENT_OK frames=1
      checksum=0x7b2cafc5 source=linux`.
- [x] This transcript is from the `ba6544c` revalidation. The original run at
      `716dcb4` recorded identical presentation, save, portal, and revoke
      values, with `AETHER_GATE4_INPUT_OK ... pointer=23`; the pointer count
      depends on the click sequence and is not part of the gate.
- [x] QEMU screenshots:
      `build/gate4/gridcraft.ppm` — 1280x800, 1,024,000 non-black pixels,
      sha256
      `492127ee13607b711d248c583d3760753d1eb897293953119b2a9bfc35ebbebb`;
      `build/gate4/gridcraft-after-input.ppm` — 1280x800, 1,023,989 non-black
      pixels, sha256
      `6d877f91ebe36e30b6bdf7ec4e00215fcce002fbe9530ec44223ffd9e162cba8`.
      The post-input screenshot shows the real workbook with `GATE4-GUI.XLSX`
      in the title.
- [x] `AETHER_GATE4_INPUT_OK keys=1 pointer=24 focus=3`. The harness waited
      for document-title readiness, clicked the window, and sent `4`, `2`,
      `ctrl-s` through the emulated PS/2 path.
- [x] Portal, save, and revoke markers recorded above. The kernel read 4,089
      bytes through
      `/run/aether/portal/5b428ae53860eb72/gate4-gui.xlsx`, committed a
      4,093-byte workbook to `/home/gate4-gui.xlsx` with `.sctmp` staging plus
      rename, and recorded one read denial and one write denial after
      revocation. The harness closed the window and verified the post-exit
      revocation.
- [x] Boot also passes `AETHER_LINUX_RESTART_OK` after the fixture
      synchronization fix at `716dcb4`.
- [x] Compatibility database record after the run: GridCraft remains
      `limited`, now naming `linux.gridcraft.gui.present`,
      `linux.gridcraft.gui.input`, `linux.gridcraft.portal.save`, and
      `linux.gridcraft.portal.revoke` with platform `qemu-q35-ovmf` and date
      `2026-10-21`. The classification stays `limited` because the Gate 2
      production follow-up is open.
- [x] `make ci` passed on the tested code at `716dcb4` from a docs-only
      descendant worktree when run with the host's VNC `:9` collision isolated
      (`bwrap --unshare-net env AETHER_ISO_TIMEOUT=300 make ci`). The plain
      command in the shared host environment aborted at the boot step because a
      pre-existing QEMU process owned `127.0.0.1:5909` (VNC display `:9`); that
      is an environment conflict, not a code failure. In the isolated run,
      lint, docs-check, license-check, unittest (326 host tests plus 8 Python
      tests), compat, AppImage, SquashFS, update, boot, ISO/desktop on VGA and
      VMware SVGA, VMware VMX generation, shell, fault, rollback, negative, and
      release-tamper steps all passed. `make gate4-test` was run separately and
      passed with the marker transcript above; the `ba6544c` revalidation also
      passed. `make docs-check` and `make license-check` also pass directly.

## Known limitations

- Host-derived runtime pin; not reproducible across hosts.
- Mesa/lavapipe and the Wayland bridge are bundled binary/runtime components;
  their licences and provenance need a release review.
- Ctrl+S currently produces two staging/rename cycles because press and
  release both translate; the first commit emits the SAVE marker. This is
  accepted prototype behavior, not a claim of single-commit semantics.
- Input delivery is broadcast to matching proxies/wrappers; it is not yet
  per-surface delivery.
- No D-Bus/XDG desktop portal chooser: the gate uses an exact-file grant path
  and kernel-side revocation. An interactive chooser is follow-up.
- No persistent block device: the overlay and grants are RAM-backed across the
  boot.
- No GPU acceleration; lavapipe is slow.
- Linux syscall dispatch, ELF/library mapping, and VMA management remain in the
  kernel (Gate 2 production follow-up).
- The compositor adapter remains ring 0.
- No general Linux application compatibility is claimed. The GridCraft record
  must not be raised above what the named tests support, and it remains
  `limited` until the Gate 2 production follow-up closes.

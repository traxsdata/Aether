# PR #20 Gate 4 review — windowed GridCraft

**Status:** in progress. This review records only evidence that has actually
run. It is not a compatibility claim beyond the named tests.

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

## Current guest blocker (at the integration head)

The guest initializes the signed runtime rootfs, imports the pinned AppImage,
opens `/dev/aether-display`, reaches `AETHER_GATE4_DISPLAY_CREATE_OK
pid=7 size=1440x900`, and receives and acknowledges the xdg-shell configure.
It then parks in the event loop without a `wl_surface.attach`/`commit` of a
wgpu swapchain buffer, so no `AETHER_GATE4_PRESENT_OK` is emitted and the
input/save/revoke markers never run. The host-only bridge, using the same
client ABI, does present real 1440x900 ARGB frames and produces a real GUI
screenshot. The remaining work is the guest event/presentation handoff:
first-frame scheduling and Mesa WSI buffer-release/queue delivery over the
kernel display endpoint. This is a presentation blocker, not a container,
loader, portal, or rootfs failure.

## Evidence to fill in

- [ ] Exact commit tested. (EVIDENCE-PENDING: tested commit hash)
- [ ] `make gate4-test` pass/fail and marker transcript.
      (EVIDENCE-PENDING: marker transcript)
- [ ] `AETHER_GATE4_PRESENT_OK frames=... checksum=...`
      (EVIDENCE-PENDING: present marker values)
- [ ] QEMU screenshot path/dimensions/sha256 and non-black pixel count at
      `build/gate4/gridcraft.ppm`
      (EVIDENCE-PENDING: screenshot path, dimensions, and non-black pixel count)
- [ ] `AETHER_GATE4_INPUT_OK keys=... pointer=... focus=...`
      (EVIDENCE-PENDING: input marker values)
- [ ] `AETHER_GATE4_PORTAL_OK`, `AETHER_GATE4_SAVE_OK`,
      `AETHER_GATE4_REVOKE_OK`. (EVIDENCE-PENDING: portal/save/revoke marker values)
- [ ] Compatibility database record after the run. (EVIDENCE-PENDING: windowed
      record digest, runtime profile, named tests, platform, and test date)
- [ ] `make ci` result and anything not run. (EVIDENCE-PENDING: ci result)

## Known limitations

- Host-derived runtime pin; not reproducible across hosts.
- Mesa/lavapipe and the Wayland bridge are bundled binary/runtime components;
  their licences and provenance need a release review.
- No D-Bus/XDG desktop portal chooser: the gate uses an exact-file grant path
  and kernel-side revocation. An interactive chooser is follow-up.
- No persistent block device: the overlay and grants are RAM-backed across the
  boot.
- No GPU acceleration; lavapipe is slow.
- Linux syscall dispatch, ELF/library mapping, and VMA management remain in the
  kernel (Gate 2 production follow-up).
- The compositor adapter remains ring 0.
- No general Linux application compatibility is claimed. The GridCraft record
  must not be raised above what the named tests support.

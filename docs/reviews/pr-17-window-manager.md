# PR #17: window-manager follow-up review

**Date:** 2026-10-20

**Reviewed PR head:** `414b4516b402dd590aff91ed00080b5bec160e13`

**Result:** Functional/rendering fixes implemented and initially validated locally. This is
still a research desktop prototype, not a completed specification milestone or
a VMware Workstation qualification.

## Findings and changes

| Priority | Finding | Resolution |
|---|---|---|
| P1 | Every input event could repaint the entire desktop, recompute the wallpaper, and copy the entire uncached display. Slow rendering also stretched animation time. | Cache wallpaper, batch events, clip fills/blends before pixel loops, track old/new damaged bounds including shadows, update only cursor backing for ordinary motion, and use actual elapsed ticks. Expose rendering counters/timings. |
| P1 | Files was only a painted root listing; folders and files had no interaction handlers. | Add a bounded read-only browser model with sidebar destinations, selection, double-click/Enter, parent navigation, scrolling, refresh, and bounded text previews through an explicit filesystem-service adapter. |
| P1 | Dedicated E0 arrows were discarded. Modifier sides were conflated, and shared i8042 output was read without checking its source. | Add host-tested input decoders/FIFOs; decode E0 navigation and both modifier sides; route bytes by status/source and handle parity/timeout errors. |
| P1 | Latest-button snapshots could lose an entire click during a slow paint. A failed mouse probe could leave the keyboard disabled. | Preserve ordered pointer packets with a bounded FIFO and atomic final-state reconciliation. Attempt keyboard re-enable on every probe exit, and bound ACK/resend handling. |
| P1 | GOP mode information contained four pixel-mask structures instead of one, placing stride at the wrong offset. Fallback code guessed stride and enlarged short apertures. Pixel-format names were reversed. | Host-test the standard 36-byte layout/offset 32, validate descriptor length/alignment and the reported aperture, honor padded stride and both RGB/BGR byte orders, and bump the boot ABI to 3. Bound firmware-driven dimensions/apertures. |
| P2 | Minimizing could retain focus until animation completion; repeated reverse cycling could bounce between two windows; restore discarded zoom state; dock restore could focus another Space invisibly. | Correct eligibility/focus order, preserve pre-minimize state, and route restore through workspace-aware application activation. |
| P2 | Closing windows disappeared immediately rather than showing their transition. Wrapped IDs could alias live windows. | Paint closing windows until completion while denying them input; skip live IDs when allocating after wraparound. |
| P2 | Search-field/menu-padding clicks could execute the first action. Tinted menu alpha overflowed. Hit tests ignored animation geometry. | Hit only the shared painted control rectangles, use saturating alpha arithmetic, and share animated window frames between painting and hit testing. |
| P2 | Keyboard-only operation could not open all menus or resize a window. Lower content squared off rounded corners. | Add F10 and Left/Right menu access, Shift+arrow resize, and independent rounded-silhouette content clipping. |
| P2 | Boot markers did not exercise actual user interactions, and ISO checks used a reduced marker/failure set. | Require all boot/security markers and any subsystem failure; inject emulated PS/2 input through QMP, compare damaged/full screenshots, check colors and cursor-only rendering, and exercise Files against the real service. |
| P2 | VMX generation used fragile `ls` parsing, lacked serial diagnostics, and rebuilt a previously tested ISO. | Validate/escape paths, support an explicit ISO, preserve the tested artifact, disable unsupported input/devices/3D, bound display size, and capture COM1. Add CLI generation to CI/qualification and include the previously missing qualification rollback step. |

The runtime remains original Rust with minimal existing hardware assembly. No
Apple source, font, bitmap, icon, wallpaper, or exact UI implementation was
introduced. General design references and the stack decision are recorded in
[ADR-016](../decisions/ADR-016-desktop-review-and-uefi-correctness.md).

## Initial local validation

The results below record the initial **uncommitted working tree**, not a
post-commit attestation. Its guest banner identifies it as `414b4516b402-dirty`.
Subsequent committed-SHA validation and publication evidence are recorded in
PR #17; do not interpret the historical identifiers below as the published head.

| Command / scenario | Result |
|---|---|
| `PATH="$HOME/.cargo/bin:$PATH" TMPDIR=/tmp/opencode make ci` | PASS, exit 0; lint, links, licences, host tests, boot, ISO interactions, VMX generation, shell, fault, rollback, negative boot, signed-release demo and both tamper rejections |
| `make unittest` (also run inside CI/qualification) | 219 Rust tests and 6 Python tests pass; includes 56 WM/browser tests and a host harness of the actual graphics module |
| `make qualify` with the same Rust environment | PASS, 14/14 steps; report in `build/qualification/report.md` |
| ISO: QEMU VGA, 1280×800 | PASS: dedicated arrows, title drag, keyboard resize, Files sidebar/folders/previews, parent navigation, denied vault access, menus/search, cursor-only update, RGB colors, and partial/full screenshot equality |
| Same ISO: QEMU VMware SVGA, 1280×800 | PASS, same workflow; **not** VMware Workstation |
| Same ISO: manually invoked scripted VGA workflows at 800×600, 1024×768 and 1920×1200 | PASS, same interaction and screenshot checks |
| `git diff --check` | PASS |

The final qualification run checked 98 relative documentation links and 18
workspace packages under Apache-2.0, with no external Rust dependencies. This
review document is not part of the guest image.

Normal-boot evidence from this tree includes:

```text
AETHER_KERNEL_OK stage=entry version=0.1.0 commit=414b4516b402-dirty
boot: hand-off validated (version 3, 136 bytes)
graphics: framebuffer 1280x800 format=BlueGreenRedReserved backbuffer=on
AETHER_INPUT_OK keys=8 dropped=0 queue=32 mouse=ps2
AETHER_FILES_OK folders=4 navigation=1 preview=1 private_denied=1
AETHER_WM_OK windows=3 spaces=4 dock=8 menus=6 search=1 glass=Tinted
AETHER_BOOT_OK stage=kernel version=0.1.0
```

### Initial local-review artifact (historical, dirty-tree build)

- ISO: `build/ISOs/aether-20261020T045420Z.iso`
- SHA-256: `a53e0dcc647f14f75a8c0caed96a36183026d10bcec9fd8989ad3f8393303cfd`
- VM definition: `build/aether-vmware.vmx`; regenerated by later validation runs
- Current machine-readable evidence: `build/iso-test/artifact.json`
- Per-adapter serial/interaction logs, screenshots and timing samples:
  `build/iso-test/`
- Full CI log: `build/pr17-ci.log`
- Qualification log/report: `build/pr17-qualification.log` and
  `build/qualification/report.md`

Generated artifacts stay ignored, not committed. Follow the
[Workstation settings and smoke checklist](../reference/vmware-workstation.md),
including UEFI, Secure Boot off, PS/2 input, and no installed disk. Adjust the
absolute ISO/serial paths if moving the VMX to another host.
For a current build, use the ISO path, digest and commit in `artifact.json` and
the VMX generated by that run, rather than assuming the historical artifact above
matches the current branch. The PR description records the tested published SHA.

### Responsiveness measurement

The original PR's published/tested `abb5cf7982e2` image took approximately
189–326 ms per forced full-redraw/diagnostic round trip at 1280×800 in the
development QEMU environment (five samples; median 318.9 ms). Revised runs are
roughly 40–50 ms median at 1280×800, with ordinary pointer motion taking the
small cursor-only path. The final 1920×1200 software-renderer run was about
85 ms for a full redraw, so large modes still have a clear performance cost.

These are directional development measurements, **not** a controlled benchmark,
frame-rate promise, or Workstation result. The baseline used 1 GiB/one vCPU;
ISO interaction tests use 2 GiB/two vCPUs, and Aether itself remains single-core.
Serial diagnostics, emulation pacing and host load are included.

## Remaining limitations / review required

1. **Actual VMware Workstation is not installed here.** SVGA/OVMF emulation and
   valid VMX generation are useful evidence, not platform qualification. Record
   a Workstation version/host/configuration, ISO digest, COM1 log, screenshot,
   input behavior and rendering timings before claiming support.
2. **Desktop and service adapters still run at ring 0.** They are trusted kernel
   code. Moving them into capability-scoped ring-3 services with a versioned
   surface/input IPC protocol remains required by the technical specification.
   No claim of compositor-compromise containment is justified by this change.
3. **Files remains read-only and volatile.** Copy/move/rename/delete, tabs/split
   panes, persistent storage, import/export consent and packaged application
   execution remain unimplemented. Private app-vault denial is intentional.
4. **GOP has no atomic page flip/vblank guarantee.** Buffering reduces partial
   drawing exposure; it cannot guarantee tear-free scanout, GPU blur, or 60 fps.
   Larger-than-buffer modes use the slower direct path.
5. **Accessibility is a prototype scope.** Keyboard parity, reduced motion and
   high contrast are implemented; a screen reader, scalable text, IME and an
   external accessibility review are not.
6. **Kernel/security maintainer review remains mandatory**, particularly for the
   boot ABI, firmware validation and privileged input/rendering changes. No
   external security review or non-test-key release was performed.

## Commit and PR handling

The initial local review did not commit, push, rebase, merge or publish a GitHub
review. Follow-up publication appends focused commits to the existing PR branch;
generated images/logs, local configuration and keys remain excluded. Follow
[CONTRIBUTING.md](../../CONTRIBUTING.md) and the
[PR template](../../.github/pull_request_template.md): keep commits focused with
conventional imperative subjects, then rerun `make ci` **after committing** and
record that exact new SHA, commands/results, normal-boot markers, untested
platforms and limitations before updating PR #17. Do not force-push reviewed
history or describe this dirty-tree run as a new exact-commit attestation.

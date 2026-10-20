# ADR-016: Responsive desktop, working Files, and correct GOP hand-off

- **Status:** Proposed for the PR #17 follow-up; requires kernel/security review
- **Date:** 2026-10-20
- **Supersedes in part:** [ADR-015](ADR-015-window-manager-and-compositor.md)
  (full-screen-only presentation, pointer snapshots, and tear-free claims)

## Context

Review of PR #17 and interactive testing found two user-visible problems: drawing
windows was sluggish, and Files painted a directory list without handling folder
navigation. Boot markers and model-only tests had not exercised these workflows.

There were also correctness issues: discarded E0 arrow scancodes, lost mouse
button edges between frames, focus remaining on minimizing windows, incorrect
zoom restoration, search-field clicks activating results, and 8-bit menu-alpha
overflow at the Tinted setting.

The firmware binding declared four `EFI_PIXEL_BITMASK` structures instead of one.
Consequently it read the stride at the wrong offset and fell back to width. The
boot ABI also reversed GOP's RGB/BGR names; ignoring pixel formats masked that
mistake on one adapter rather than fixing it.

## Decision

1. **Keep the Rust/framebuffer-first MVP stack.** Do not import a Linux/Wayland
   event loop or a large GUI toolkit into an independent `no_std` OS. Keep the
   WM, browser presentation state, and input decoding allocation-free and
   host-testable. `aether-input` has no hardware I/O or service authority.
   This is a prototype stack choice, not a claim that Rust `no_std` GUI toolkits
   do not exist; evaluating one later requires a surface/input ABI and a separate
   dependency, licensing, performance, and privilege review.
2. **Bound rendering work.** Cache the procedural wallpaper in a second
   1920×1200 RAM buffer; repaint damaged regions back-to-front, including old
   window positions and shadows, and present only those pixels. A separate
   15×22 cursor backing store avoids scene repaint for ordinary pointer motion.
   Clip rows before filling/blending, retain an independent damage clip through
   nested window clips, and clip content to the rounded window silhouette.
   Batch input before painting and advance animations by actual elapsed ticks.
3. **Preserve input transitions.** Decode set-1 E0 navigation and both sides of
   modifiers. Queue pointer packets, then reconcile overflow with an atomic
   final snapshot; do not infer clicks solely from the latest button state.
   Route shared i8042 output by its source/status bits. Every mouse-probe exit
   attempts to re-enable the keyboard; a missing mouse must not disable it.
4. **Make Files functional but read-only.** Use a bounded browser model and an
   explicit filesystem-service adapter. Support selection, double-click/Enter,
   parent navigation, sidebar locations, keyboard scrolling, refresh, and
   bounded text previews. `/apps` vault contents are denied by the service
   adapter; path validation in the UI is not authorization. Previewing does not
   execute a program, grant a capability, or act as portal consent.
5. **Correct the firmware contract.** Host-test the 36-byte GOP structure and
   its stride offset of 32. Validate `SizeOfInfo` before dereferencing it, retain
   the reported stride, and reject undersized apertures instead of inventing a
   larger one. Respect both byte orders during drawing and blending. Bump the
   boot ABI to **3**, with `0 = RGBX`, `1 = BGRX`, matching UEFI. Structure size
   remains 136 bytes; version-2 loaders/kernels are deliberately incompatible.
   Bound dimensions/stride to 8192 pixels and mapped apertures to 512 MiB.
6. **Test the actual user workflow.** `make iso-test` boots the same El Torito
   ISO under OVMF with VGA and QEMU's VMware SVGA adapter. QMP injects real
   emulated PS/2 input. Check window movement/resizing, Files navigation and
   previews, private-data denial, menus/search, color order, cursor-only
   updates, and partial/full screenshot equality. Require every security/boot
   marker and reject any subsystem failure marker. Host tests also compile the
   actual graphics module against guarded/padded RAM surfaces.
7. **Keep claims narrow.** GOP presentation is a volatile memory copy, not an
   atomic page flip or a vblank guarantee. QEMU SVGA is not VMware Workstation.
   Generate a Workstation 17+ VMX with UEFI, Secure Boot off, PS/2 input, bounded
   display size, no disk/network/USB, and a serial capture file. Actual
   Workstation qualification remains a separate mandatory platform test.

## Consequences and specification alignment

- The two large buffers cost 18,432,000 bytes total (about 17.6 MiB), plus the
  small cursor backing. This is a bounded prototype tradeoff, not a general
  high-DPI compositor. Larger modes retain the slower direct-paint fallback.
- Files still lacks mutations, tabs, split panes, persistent storage, and
  import/export consent. These are incremental Epic G follow-ups, not completed
  Explorer requirements.
- **The compositor and service adapters still execute at ring 0.** They are in
  the trusted computing base. Service-only calls are a coding convention, not
  containment of compromise. This prototype does not fulfill the technical
  specification's user-space desktop/service boundary or Phase 4 exit criterion.
- The next architectural steps are a versioned surface/input IPC protocol,
  capability-scoped display/input resources, then a ring-3 compositor and
  application views. Do not add implicit elevation, app-vault access, packaged
  application execution, or portal grants as shortcuts around those boundaries.
- Boot ABI and firmware validation changes require explicit maintainer security
  review before merge. Passing CI is evidence, not external assurance.

## Public references and clean-room provenance

- [Apple's public macOS design overview](https://www.apple.com/os/macos/), used
  only for general principles: readable layered surfaces, consistent toolbars,
  sidebars, user-controlled transparency, and search. No Apple code, fonts,
  icons, wallpaper, or exact metrics are used.
- [UEFI GOP specification](https://uefi.org/specs/UEFI/2.10/12_Protocols_Console_Support.html#graphics-output-protocol),
  for the public firmware ABI, not OS UI implementation.

## Alternatives

- **Add more effects before measuring:** rejected; expensive full-screen work
  was already delaying input, and extra polish must not hide broken workflows.
- **Fix only the synthetic boot tests:** rejected; those passed while real
  arrows and folder navigation did not work.
- **Claim a complete microkernel desktop or VMware qualification:** rejected;
  neither the existing architecture nor the available test platform supports it.

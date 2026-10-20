# ADR-015: Clean-room Golden Gate-inspired window manager

- **Status:** Accepted for the prototype
- **Date:** 2026-10-20
- **Supersedes in part:** ADR-012 (fixed six-window, no-animation desktop)

## Context

Phase 4 delivered a keyboard-only software compositor with six fixed windows.
The next desktop milestone needs a real window-management model: focus and
z-order, move/resize/minimize/zoom, a menu bar, a dock, workspaces, a workspace
overview, search, animation, and readable layered surfaces.

The public description of macOS 27 Golden Gate emphasises a refined Liquid Glass
language: a global transparency slider from clear to tinted, more uniform
toolbars, tighter and consistent window corners, edge-to-edge sidebars with
colourful icons, stronger shadows and separation, and improved system search.
Those are useful design principles, but the implementation is closed and its
artwork is proprietary.

The project also needs a distribution artifact that boots on VMware Workstation
as well as the existing QEMU/OVMF harness.

## Decision

1. **Clean-room design only.** Recreate general principles — top menu bar,
   floating dock, rounded windows, traffic-light controls, workspaces, overview,
   search, and layered translucency — using original code, palettes, icons,
   glyphs, and a procedural wallpaper. Do not embed or trace Apple code, fonts,
   images, or icons.
2. **A host-testable window-manager crate.** Add `aether-wm` under `kernel/wm`.
   It is `no_std`, forbids `unsafe`, allocates nothing, and owns only pure state:
   geometry, layout, focus, z-order, workspaces, menus, dock targeting,
   Spotlight filtering, hit testing, settings, and integer animation.
3. **Keep the renderer in the kernel for this milestone.** `kernel/src/desktop.rs`
   paints through the existing graphics service. The window manager remains a
   code-structure boundary, not a security boundary, until the compositor moves
   to ring 3.
4. **Layer, do not blur.** Implement Clear/Balanced/Tinted glass levels with
   alpha compositing, specular highlights, darkened edges, and stronger
   shadows. Do not claim background blur, HDR, or GPU composition.
5. **Double-buffer.** Compose into a bounded static 1920×1200 back buffer and
   present with one linear copy. Avoid tearing and make animation pacing
   independent of framebuffer write latency. Fall back to direct painting above
   that size.
6. **Add a bounded PS/2 mouse path.** Initialise the auxiliary port, unmask
   IRQ12, decode three-byte packets, and keep a pointer snapshot for the desktop.
   Keep full keyboard parity for every window action.
7. **Expose accessibility choices as live settings.** Reduced motion, high
   contrast, glass level, and dock magnification are real state transitions,
   not mock controls. The settings are in-memory until persistent preferences
   exist.
8. **Test the distribution path, not just the image.** Add an ISO boot test that
   uses the same El Torito/UEFI path as VMware and generate a VMware Workstation
   `.vmx` for the newest ISO.

## Consequences

- The window manager is substantially easier to test and review than a
  monolithic renderer. Geometry regressions and focus bugs are host-test failures.
- The 9.2 MiB back buffer increases kernel BSS but removes a user-visible
  correctness problem. Larger resolutions fall back to direct painting.
- The UI is still a software renderer: full-screen repaints are acceptable in a
  VM but not a path to high-DPI or HDR. A future compositor protocol should own
  per-application surfaces and damage tracking.
- The mouse gives the desktop the expected interaction model; tablet, absolute
  pointer, USB HID, and multi-touch remain unsupported.
- Glass levels simulate diffusion rather than true blur. The implementation must
  keep describing them as tint/opacity levels.
- The desktop remains a trusted kernel component in this milestone. The
  authority rules from ADR-012 still apply: it can display policy but cannot
  grant capabilities.

## Alternatives considered

- **Adopt an existing Rust GUI toolkit:** rejected. There is no `no_std`,
  framebuffer-first toolkit that fits the kernel's constraints and trust model;
  adding one would import a large dependency and an inappropriate event loop.
- **Direct framebuffer painting with damage rectangles:** deferred. Damage
  tracking is the right long-term optimization, but double buffering gives the
  larger correctness win for the same bounded complexity.
- **Copy Apple's Golden Gate assets or metrics exactly:** rejected as both a
  licensing problem and a violation of the clean-room requirement.
- **Ship only a QEMU image:** rejected. The deliverable is an ISO that a VMware
  Workstation user can attach to a UEFI VM.

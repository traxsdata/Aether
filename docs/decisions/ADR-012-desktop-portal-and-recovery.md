# ADR-012: Software desktop, file portal, and recovery prototype

- Status: accepted
- Date: 2026-10-19

## Context

Phase 4 needs a graphical desktop, a file manager, a Security Center, a file
portal, and a recovery path. The project has no GPU driver, window toolkit, or
persistent storage, and the specification warns that graphics work can consume
the project.

## Decision

1. Start with the firmware's linear GOP framebuffer. The loader validates the
   mode and passes it in BootInfo v2; the kernel maps it cache-disabled and
   non-executable.
2. Paint a bounded software compositor directly into the framebuffer with a
   small bitmap font. Do not add a GPU command stream or a toolkit dependency.
3. Keep six fixed windows: Files, Security Center, File Portal, Settings,
   Recovery, and System Log. Focus and movement are keyboard-driven.
4. Implement the portal as exact `(process root, path, rights)` grants. The
   desktop can display grants and request a portal open, but the portal service
   decides. Grants are single-file and revocable in the data model.
5. Keep the recovery prototype to metadata selection: current and previous
   image markers, a selected marker, and an independent user-data sentinel.
   Do not claim signature verification or crash-consistent updates.
6. Provide a timestamped UEFI El Torito ISO target for distribution. The ISO
   carries a small FAT EFI system image; the kernel remains embedded in the
   loader.

## Consequences

- The desktop is testable on any OVMF configuration with a display and adds no
  external graphics dependency.
- The compositor is immediate-mode and single-threaded; animation and
  multi-window application rendering are deferred.
- The portal demonstrates least authority but still needs interactive consent
  and persistent grant storage.
- The recovery prototype demonstrates selection and data preservation, not
  authenticated updates or rollback-attack resistance.

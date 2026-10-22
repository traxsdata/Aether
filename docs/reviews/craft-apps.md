# Craft app suite review — headless and windowed

**Date:** 2026-10-22  
**Scope:** every published Linux x86-64 Craft app with an upstream AppImage  
**Platform:** `qemu-q35-ovmf`, TCG, software VGA, no host GPU or X11

This review records only what was actually run. A headless pass means the
AppImage was signature- and digest-checked, imported, launched through its
`PT_INTERP`, and exited zero under the Gate 3 host-glibc runtime. A GUI pass
means the windowed binary reached the kernel-owned
`AETHER_GATE4_PRESENT_OK` marker and the QEMU screenshot contained a non-blank
frame. It does not mean the application's full feature set works.

The driver is [`tools/test-craft-apps.py`](../../tools/test-craft-apps.py);
each app is pinned to the release named below and its upstream `SHA256SUMS.txt`
is verified before use. Raw logs, screenshots, and digests are written to
`build/craft-apps/<app>/` and `build/craft-apps/results.json`.

## Results

| App | Version | Headless | GUI | Notes |
|---|---:|---|---|---|
| GridCraft | 0.10.0 | pass | pass | `gridcraft-cli`; windowed run presented |
| PhotoCraft | 0.12.0 | pass | pass | `photocraft-cli`; windowed run presented |
| PdfCraft | 0.11.0 | pass | fail | windowed run creates a display, then winit reports `NoCompositor` |
| FilmCraft | 0.10.0 | pass | pass | headless uses `/usr/bin/filmcraft --version`; needs `libasound.so.2`; ALSA config/devices are still absent and logged |
| VectorCraft | 0.14.0 | pass | fail | multi-header SquashFS root now imports; windowed run reports `NoCompositor` |
| LightCraft | 0.11.0 | pass | pass | `lightcraft-cli`; windowed run presented |
| DesignCraft | 0.11.0 | pass | pass | `designcraft-cli`; windowed run presented |
| EffectCraft | 0.14.0 | pass | fail | windowed run creates a display, then winit reports `NoCompositor` |
| WordCraft | 0.9.0 | pass | pass | `wordcraft-cli`; windowed run presented |
| DeckCraft | 0.9.0 | pass | pass | `deckcraft-cli`; windowed run presented |
| SoundCraft | 0.11.0 | pass | pass | `soundcraft-cli`; windowed run presented |
| CadCraft | 0.10.0 | pass | pass | `cadcraft-cli`; windowed run presented |
| FigCraft | 0.9.0 | pass | fail | windowed run creates a display, then winit reports `NoCompositor` |

**Summary:** 13/13 headless passes, 9/13 windowed passes.

## Defects found and fixed

- **Interpreter/main-image overlap.** PhotoCraft's main image ends near
  148 MiB while the interpreter was based at 120 MiB. Moved the interpreter
  base to 512 MiB and added an explicit overlap check that fails closed.
- **Staged-AppImage cap.** The 32 MiB loader bound excluded most Craft apps.
  Raised it to 128 MiB; all tested AppImages fit.
- **Missing overlay directory and flush syscalls.** PhotoCraft called
  `mkdir`/`mkdirat` and `fsync`/`fdatasync`, which returned `ENOSYS`.
  Implemented overlay-backed directory creation and RAM-backed flush.
- **Task-slot exhaustion.** Large apps spawn enough threads that later boot
  fixtures could no longer `clone`. Widened the non-GUI task table; exited
  slots are still not reclaimed, which remains a follow-up.
- **Multi-header SquashFS directories.** VectorCraft's AppImage uses more than
  one directory header, and its 142 MiB executable's block-size list crosses
  a metadata-block boundary. The importer now walks all headers and reads
  inode data as a metadata stream.
- **Gate 4 display grant.** The endpoint was hard-coded to GridCraft. The Gate
  4 test build now grants it to any signed AppImage; normal builds keep the
  GridCraft-only policy. Threads and forks inherit the signed application's
  grant.
- **Missing audio library.** FilmCraft's GUI needs `libasound.so.2`; it is now
  part of the Gate 4 runtime closure.

## Windowed limitation

Four apps create the display endpoint and then fail inside winit with
`WaylandError(Connection(NoCompositor))`: PdfCraft, VectorCraft, EffectCraft,
and FigCraft. The shim delivers the expected registry globals, but these
applications do not bind them and re-enter the event loop. The other nine
complete the same handshake. Treat the four failures as a Wayland-client
compatibility gap, not as evidence that those applications cannot run.

## Not claimed

- Full application functionality, file workflows, dialogs, audio, GPU
  acceleration, or persistence.
- Any classification above `limited`; the Gate 2 production follow-up is still
  open.
- The `runner/eframe` and `.app` App Store path described by
  `Aether-CraftApps`; that build tooling is not in this repository.

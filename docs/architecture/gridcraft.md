# GridCraft native application

Aether ships a native spreadsheet application in the `GRIDCRAFT` desktop window.
It is the first application in the tree that is more than a diagnostic view:
it keeps user-editable state, evaluates formulas, exposes a formula bar, and is
reachable from the compositor and the console shell.

## What runs

`apps/gridcraft` is a `no_std`, allocation-free application model:

- a bounded 10 x 10 sheet with fixed-size cell and edit buffers;
- cell selection, edit, commit, cancel, clear, and bounded navigation;
- an incremental recursive-descent formula engine with cell references,
  ranges, parentheses, unary signs, arithmetic, and `SUM`, `AVERAGE`, `MIN`,
  `MAX`, and `COUNT`;
- stable error values (`ERR:DIV/0`, `ERR:NAME`, `ERR:VALUE`, `ERR:CIRC`,
  `ERR:NUM`, `ERR:PARSE`) and cycle detection;
- a compact number formatter and a CSV export used by the `gridcraft` shell
  command.

The kernel's `kernel/src/desktop.rs` paints the window from the public model
through the existing framebuffer service. It does not evaluate formulas,
access files, or hold capabilities. The compositor routes decoded keys to the
application when the window is focused:

| Key | Selection mode | Edit mode |
|---|---|---|
| printable | start editing and insert | insert |
| Enter | start editing | commit |
| Backspace | clear the cell | delete the last character |
| Escape | leave the application | cancel the edit |
| arrows | move the selection | commit, then move |
| Tab | next window | commit, then next window |

`make test` requires the `AETHER_GRIDCRAFT_OK` marker. The marker is printed
only when the application's boot self-test passes formula precedence, ranges,
cycle detection, editing, navigation, sample recalculation, and display
checks. A regression therefore fails the boot gate rather than shipping a
silently broken app. `make shell-test` checks that the `gridcraft` command
exports the live sheet.

## Scope and honest limits

This is an **Aether-native compatibility surface for GridCraft's core
workflow**, not the upstream egui/wgpu build from
[`storytold/gridcraft`](https://github.com/storytold/gridcraft). It shares no
code or assets with upstream; the window name describes the interaction being
prototyped.

Running the unmodified upstream application needs infrastructure this tree
does not have yet:

1. **Allocation.** Upstream depends on `std` collections and `serde`. Aether's
   kernel and user program have no allocator. A native port needs a reviewed
   heap, an allocation-failure policy, and a `no_std` engine layer.
2. **A GUI runtime.** GridCraft's frontend is `egui`/`eframe`/`wgpu`. Aether has
   a software compositor and no window protocol, GPU service, clipboard, or
   font stack. The UI would need an `egui` backend over the Aether graphics
   service, or a separate frontend over the same engine.
3. **Process launch and files.** Aether has no user-space process creation from
   the desktop, no package entry-point execution, and no interactive file
   portal in the desktop. A real app needs all three, plus persistent storage
   instead of the volatile RAM disk.
4. **Input.** The prototype desktop is keyboard-only on this branch; a
   spreadsheet needs a pointer driver and mouse events.

Those items are tracked in the roadmap as application-runtime work
(`docs/roadmap.md`, Phase 4/5). Until they land, the desktop app is the
reviewable slice that proves the interaction, formula model, and boot gate.

## Authority

The application model has no ambient authority: it cannot open files, send IPC,
or read kernel memory. The compositor owns the framebuffer and runs in ring 0
in this prototype, so this remains a code-structure boundary, not an enforced
security domain. That limitation is the same one documented for the rest of the
desktop in [`desktop.md`](desktop.md).

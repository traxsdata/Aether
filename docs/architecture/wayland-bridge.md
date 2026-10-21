# Wayland bridge

**Status:** Gate 4 prototype. This document describes the in-app Wayland
client bridge and the kernel display endpoint. It is not a general Wayland
compositor or a production compatibility claim.

Related: [Linux layer](linux-layer.md),
[Linux compatibility](../specifications/linux-compatibility.md),
[ADR-017](../decisions/ADR-017-linux-app-runtime.md).

## Why a client bridge

The pinned GridCraft GUI is a standard Wayland client: winit/Smithay and Mesa's
Vulkan WSI call the `libwayland-client.so.0` ABI, create `wl_surface` and
`wl_shm` buffers, and wait on frame callbacks. Aether does not run a Unix
socket, an X server, D-Bus, or a GPU service in the first Gate 4 slice. Exposing
a Wayland wire server would add a guest-controlled protocol parser and a
descriptor-passing path to the kernel.

The bridge is therefore a drop-in `libwayland-client.so.0` that implements the
client ABI in the application's address space. It keeps the standard Wayland
object model (display, registry, compositor, shm, seat, output, xdg-shell,
keyboard, pointer, callback), but maps presentation and input to one narrow
device:

`/dev/aether-display`

The application remains unmodified. The bridge is a runtime component in the
signed rootfs, not part of the application.

## Data path

```
GridCraft (winit / wgpu / lavapipe)
        |
        | libwayland-client ABI (in-process bridge)
        v
/dev/aether-display  (one endpoint per granted Linux task)
        |
        | fixed 48-byte requests / 32-byte events
        v
kernel display service -> compositor window -> GOP framebuffer
        ^
        | PS/2 keyboard / pointer routed only while focused
```

- `CREATE` creates or resizes a compositor window.
- `PRESENT` validates a readable user range, copies pixels into a kernel-owned
  surface, damages the window, and emits a kernel-owned present marker.
- `SET_TITLE` / `DESTROY` update or retire the window.
- Input events carry configure, enter/leave, key (with modifiers), pointer
  motion, buttons, axes, and frame acknowledgements.
- `memfd_create` backs `wl_shm` pools and the keyboard keymap. The bridge maps
  the pool itself and passes only a validated pixel pointer to the kernel; the
  kernel never maps guest pages into its own address space.

## Security properties

- No ambient socket, `/dev/dri`, `/dev/input`, X11, D-Bus, or GPU device.
- One endpoint per granted app; an unrelated Linux app cannot open it.
- Every pointer/length is validated against the caller's page tables before a
  copy. A fault returns `EFAULT`; the kernel does not panic.
- Fixed bounds: 1920x1200 surface, 16 MiB present, 64-byte title, 32 queued
  input events, 16 MiB per memfd, bounded memfd count.
- Input is routed only while the Linux window is focused; window-relative
  coordinates are scaled by the kernel to surface pixels.
- The compositor adapter is still ring 0 in this prototype. A compromised
  compositor is in the trusted computing base; moving surfaces and input into
  a ring-3 service is follow-up work.

## Non-goals

- A Wayland wire server, XWayland, or arbitrary protocol extensions.
- D-Bus/XDG desktop portal UI (the gate uses an exact-file portal grant).
- GPU acceleration. Vulkan is lavapipe (LLVM software rasterizer).
- Multi-window application rendering; the first slice presents one toplevel
  plus ignores cursor surfaces.
- General Linux compatibility. Unsupported Wayland requests fail closed.

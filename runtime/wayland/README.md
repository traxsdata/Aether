# Aether Wayland bridge (Gate 4)

This directory builds a drop-in `libwayland-client.so.0` for the pinned
GridCraft GUI. The bridge implements the client ABI used by winit/Smithay and
Mesa's Vulkan Wayland WSI, but presents surfaces through the synthetic Aether
display endpoint instead of a Wayland socket:

- opens `/dev/aether-display` (falls back to an eventfd only for host tests)
- maps `wl_shm` pools and sends fixed `PRESENT` records to the kernel
- converts kernel key/pointer/configure events into Wayland protocol events
- embeds a US evdev keymap for `wl_keyboard.keymap`

It is not a Wayland wire server. See `docs/architecture/wayland-bridge.md`.

```sh
make BUILD=../../build/gate4-bridge
```

Host smoke test (requires a bare AF_UNIX listener at `$XDG_RUNTIME_DIR/wayland-0`
and the pinned AppImage payload; see `docs/reviews/gate4-wayland-spike.md`):

```sh
./run-host-spike.sh
```

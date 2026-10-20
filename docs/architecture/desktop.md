# Compositor and desktop prototype

The Phase 4 desktop is a software compositor with a fixed, bounded window set.
It exists to prove the graphics, input, portal, and recovery interfaces, not to
provide a production GUI toolkit.

## Framebuffer hand-off

The UEFI loader queries GOP and copies the current linear mode into
`BootInfo.framebuffer` (ABI version 2). The kernel validates the descriptor,
maps the framebuffer identity cache-disabled and non-executable, and exposes
pixel writes only through `kernel/src/graphics.rs`. `BootInfo` validation
rejects an inconsistent size, stride, or format.

QEMU with `-display none` does not expose GOP in the tested OVMF build, so the
boot harness starts a headless VNC display and an emulated VGA adapter. The
kernel works headless but does not print `AETHER_GRAPHICS_OK` or
`AETHER_DESKTOP_OK` in that configuration.

## Compositor

`kernel/src/desktop.rs` paints directly into the framebuffer:

- top bar with identity and uptime;
- six windows: Files, Security Center, File Portal, Settings, Recovery, System
  Log;
- focus outline, drag-by-keyboard movement, and taskbar;
- a 5×7 bitmap font and deterministic pixel checksum.

All geometry is clipped. Repainting is synchronous and takes no locks beyond
the single-CPU interrupt guard around the desktop state snapshot.

## Input

`kernel/src/input.rs` owns IRQ1. The PIC unmasks IRQ0 and IRQ1 after scheduler
setup; the IDT installs vector 33. The handler reads one set-1 make scancode and
pushes a decoded key into a 32-entry queue. The desktop consumes Tab, arrows,
Enter, Escape, and printable keys. A boot self-test injects a deterministic key
sequence and verifies the queue and focus changes.

## Authority

The desktop code runs in the kernel in this prototype, so it is not an isolated
security domain and must not be presented as one. Its logic is deliberately
kept non-authoritative: it reads directory contents through the filesystem
service, displays live supervisor/application/portal state, and asks the portal
for grants. It cannot mint a file capability or bypass the filesystem's root
check. The Security Center is a view over enforced state, not a policy engine.
Moving the compositor and desktop services to ring 3 is follow-up work; until
then the kernel module boundary is a code-structure rule, not a security
boundary.

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

## Live status windows

The Security Center and System Log are views over counters the kernel itself
recorded, not authored text. At paint time each window is assembled from:
installed package identity and root (`packaging::installed()`), service
counts, sandbox escape/access/quota denials, portal grants/uses/denials, user
task exits and faults, IPC denials, the update status, and the input queue's
drop counter. A closing "denied events" roll-up is drawn in the warning
colour. Because the data is recomputed on every repaint, a counter that stops
being driven by the boot self-test shows up as a wrong number. The boot gate
separately cross-checks the IPC send/receive/denial counters and the quota
counter, and a nonzero exit or user fault fails the boot before
`AETHER_USER_OK`; the remaining counters are descriptive.

## Input

`kernel/src/input.rs` owns IRQ1. The PIC unmasks IRQ0 and IRQ1 after scheduler
setup; the IDT installs vector 33. The handler reads one set-1 make scancode and
pushes a decoded key into a 32-entry queue. After boot the idle loop drains that
queue into the desktop, which consumes Tab, arrows, Enter, Escape, and printable
keys. A boot self-test injects a deterministic key sequence and verifies the
queue and focus changes. The `input` shell command reports received and dropped
counts.

There is no mouse or other pointing-device driver. IRQ12 and USB HID input are
not initialized, so the desktop is keyboard-only.

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

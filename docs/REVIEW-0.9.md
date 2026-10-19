# Review of the 0.9 work

0.9 starts from the merged 0.8 `main` and adds the hardware drivers the
physical Lenovo Yoga 9 14IAP7 (`82LU`) needs for input and networking. Wi-Fi
is explicitly still out of scope.

## What 0.9 changes

### USB xHCI

`kernel/usb.c` is a polled xHCI host-controller driver:

- maps BAR0 uncached through `vmm_map`, performs the USB Legacy Support
  BIOS/OS handoff, resets the controller and checks `CSTS.CNR`;
- owns one page each for the device-context array, command ring, event ring,
  event-ring segment table, endpoint rings and DMA buffers;
- runs Enable Slot, Address Device, Evaluate Context and Configure Endpoint
  commands, checking the completion code and slot ID;
- implements setup/data/status control transfers, normal bulk/interrupt TRBs,
  doorbells and event-ring dispatch;
- has no interrupt handler: `usb_poll()` drains events from the window manager
  loop, and every synchronous command/transfer has a PIT deadline.

Only root ports are enumerated. External hubs, USB storage, isochronous
transfers and streams are not implemented.

### USB HID

`kernel/usb_hid.c` drives boot-protocol keyboards and mice. It decodes the
eight-byte keyboard report into the kernel's `KEY_*`/`MOD_*` ABI, including
shifted symbols, caps lock and the arrow/home/end/page/delete keys, and the
three- or four-byte mouse report into `IN_MOUSE`. Reports are polled through
one interrupt-IN transfer at a time. Key repeat is not generated in software;
the PS/2 path still has the firmware repeat behavior.

### USB Ethernet

`kernel/usb_net.c` implements CDC-ECM. It takes the bulk-IN/bulk-OUT endpoints,
sets the directed+broadcast packet filter, parses the Ethernet MAC string,
keeps one receive transfer queued, and sends frames synchronously with a
bounded timeout. `net.c` prefers the built-in e1000/e1000e driver and falls
back to USB Ethernet.

QEMU's `usb-net` exposes **two configurations**: RNDIS first and CDC-ECM
second. The driver reads every configuration descriptor and only accepts a
configuration with a CDC Ethernet control interface (class 2, subclass 6,
protocol 0), so it cannot mistake the RNDIS data interface for ECM.

### I²C-HID touchpad and pen

`kernel/i2c_hid.c` drives the Yoga's LPSS I²C controllers:

- `PCI 00:15.1` is I²C1 and carries the touchpad at `0x2c` (Synaptics) or
  `0x15` (Elan); the 82LU DSDT's `HIDD` value is the descriptor register:
  `0x0001` for Elan and `0x0020` for Synaptics, so both are validated;
- `PCI 00:15.0` is I2C0 and carries the pen at `0x0a`;
- the LPSS private reset and remap registers are used before the Synopsys
  DesignWare controller is brought up;
- the controller runs in fast mode with timing counts for the 400 kHz bus
  declared by the machine's ACPI `I2cSerialBusV2` resource;
- the HID descriptor, report descriptor, command/data registers and input
  register follow HID over I²C, with little-endian 16-bit register addresses
  and repeated-start reads;
- the report parser finds Generic Desktop mouse fields or Digitizer touchpad
  fields, buttons, wheel, tip switch and contact count. Relative reports are
  sent directly; absolute reports are scaled to the framebuffer and converted
  to deltas. Physical buttons are used when present; a clickpad without button
  fields gets tap-to-click.

The kernel still has no ACPI AML interpreter or GPIO interrupt controller, so
the driver uses the known Yoga addresses and polls the input register at
125 Hz. This is a deliberate hardware-validation step, not a claim that the
ACPI resources are fully bound.

### Shared input queue

The input queue moved from `ps2.c` to `kernel/input.c`, so PS/2, USB HID and
I²C-HID feed one queue. `input_push` is the only cross-driver entry point.

### PIT time

`aether_pit_ms` now accumulates the actual 1.193182 MHz PIT counter delta
instead of incrementing once per call. This makes the NVMe, xHCI and I²C
deadlines meaningful even when interrupts are off.

## Security properties

- Every DMA address comes from `pmm_alloc`, `pmm_alloc_contig` or the driver's
  own page allocations. User pointers never reach hardware.
- Descriptor lengths, report IDs, endpoint counts, transfer lengths, queue
  indices, port numbers and slot IDs are checked before use. Report parsing
  is bounded by `HID_MAX_FIELDS`, `HID_MAX_REPORT_DESC`, `HID_MAX_INPUT` and
  the device's `wMaxInputLength`.
- USB control requests are built from fixed register values; the network path
  still enforces the IPv4/DHCP/DNS/firewall policy in `net.c`.
- No interrupt handler or allocator runs on the USB/I²C I/O path. A broken
  device gets a timeout, not an unbounded spin.
- I²C probing is limited to the three Yoga addresses from ACPI. The driver
  does not scan the whole bus.

## Evidence

Run on this machine (QEMU TCG; no KVM):

```text
make test                                      PASS
make unittest                                 57 kernel / 69 terminal, 0 failed
make test-usb                                  PASS
```

`make test-usb` boots with:

```text
-device qemu-xhci -device usb-kbd -device usb-mouse
-netdev user,id=usbnet0 -device usb-net,netdev=usbnet0
```

and checks xHCI enumeration, HID keyboard/mouse input, CDC-ECM selection, and
DHCP (`[net] address 10.0.2.15 ...`). The kernel unit tests include synthetic
boot-mouse and Precision-Touchpad report descriptors.

Follow-up fixes after the first 0.9 commit:

- `de69604` validates the HID descriptor at both Yoga registers (`0x1` and
  `0x20`) instead of assuming `0x1`;
- `bd30d83` queues the repeated-start read commands directly behind the
  register address, so the DesignWare controller does not see an empty TX
  FIFO between the address and data phases.

Not run here:

- **Physical Yoga hardware.** The I²C-HID addresses, LPSS reset, polling and
  report layout are taken from the machine's ACPI dump and the HID over I²C
  and DesignWare register contracts, but no Yoga was available to this
  environment. The first hardware run should check the serial log for
  `[i2c-hid] ...` and `log`/`[usb] ...` lines.
- **Wi-Fi.** AX211 firmware loading and 802.11/WPA2/WPA3 are a separate
  project.
- **External USB hubs, USB storage, camera, audio, Bluetooth, fingerprint,
  Thunderbolt/USB4.** The xHCI core can address root-port devices; those
  classes and the hub tier are not implemented.
- **`make test-nvme`** was not rerun in this slice; it needs the owner's
  release signing key and is unchanged by these drivers.

## Hardware bring-up notes

On a Yoga 9 14IAP7, the expected serial/log evidence is:

```text
[usb] xHCI 00:14.0, ... ports, ... slots
[usb] slot n: ... HID keyboard ready        (if a USB keyboard is attached)
[usb] slot n: USB Ethernet ...              (if a USB Ethernet dongle is attached)
[i2c-hid] 2c: ... report descriptor, absolute touchpad
[i2c-hid] touchpad input active
```

If the touchpad does not appear:

1. run `log` in the Terminal and look for the LPSS and I²C-HID lines;
2. the ACPI-selected address may be `0x15` on an Elan unit, which the driver
   also probes;
3. the machine may keep the touchpad powered down or held in reset through an
   EC/GPIO resource that this kernel does not interpret yet. That needs an
   ACPI GPIO/power-resource binding, not a blind bus scan.

A USB mouse is the fallback and is handled by the same HID driver without any
I²C support.

## Deliberate gaps

- No ACPI AML interpreter, so `_CRS`, `_PS0/_PS3`, GPIO interrupts and
  battery/lid events are not evaluated.
- Polling instead of the touchpad's GPIO interrupt; this costs a small amount
  of bus time every 8 ms.
- No software key repeat for USB keyboards.
- No external hub tier, USB storage, UVC, audio, Bluetooth, fingerprint or
  Thunderbolt.
- No Wi-Fi.
- No Secure Boot signing or loader update path (unchanged from 0.8).

## Review follow-up (at `512e10d`)

The changes requested by the review were addressed:

1. **Scratchpads.** `HCSPARAMS2` is decoded and the controller's scratchpad
   pointer array plus one zeroed page per buffer are allocated; `DCBAA[0]`
   points at the array. Controllers that report more than 1023 buffers are
   rejected.
2. **PORTSC writes.** Change acknowledgements preserve PP/PIC/WCE/WDE/WOE and
   write the change bits (CSC/PEC/WRC/OCC/PRC/PLC/CEC/CAS). PED and PR are
   never written back, so a reset or a port-status event cannot disable the
   port it just enabled.
3. **Context size.** `HCCPARAMS1.CSZ` is checked. 64-byte contexts are refused
   with a clear log line instead of writing 32-byte contexts to the wrong
   offsets.
4. **I2C identity and DMA.** LPSS probing now requires an Alder Lake-P/M I2C
   device ID and class, enables memory decoding only (bus mastering stays
   off), and restores the original PCI command register on every failure path.
5. **I2C backoff.** Consecutive input-read timeouts back off from 8 ms to 1 s.
   NACKs are treated as "no report", not as a stuck bus, so an idle touchpad
   keeps normal polling.
6. **HID usage pages and report IDs.** Usage pages are stored as 16 bits, and
   report IDs of 16 or more are skipped rather than filed under the previous
   report.
7. **Control-transfer lengths.** The data-stage TRB sets ISP, short-packet
   completions carry the residual length, and the scratch page is zeroed
   before every IN transfer.
8. **Timeouts.** Events are matched to the TRB that completed. A timed-out
   endpoint is stopped and its ring is reset with Set TR Dequeue Pointer; the
   command ring is marked failed after a command timeout so it cannot lap.
9. **Submit ordering.** `usb_ep_submit` checks `pending` before copying into
   the endpoint buffer.
10. **Endpoint intervals.** HS/SS use `bInterval - 1`; FS/LS use a bounded
    microframe exponent instead of shifting past a `uint8_t`.
11. **MAC validation.** A failed string parse falls back to a locally
    administered address, and multicast/all-zero/all-ones addresses are
    rejected.

The parser also clamps X/Y to their declared logical range and clamps pointer
deltas before narrowing to the 16-bit input ABI.

Evidence after the changes (QEMU TCG):

```text
make test                                      PASS
make unittest                                 57 kernel / 69 terminal, 0 failed
make test-usb                                  PASS
```

The scratchpad, CSZ, port-write and timeout paths need the physical Yoga to
exercise; QEMU reports no scratchpads, uses 32-byte contexts, and does not
enforce PED write-1-to-clear.

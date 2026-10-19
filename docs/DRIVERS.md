# Drivers in æther 0.9

0.8 moved the storage driver to Rust. 0.9 adds the USB and touchpad drivers
the physical Yoga 9 14IAP7 needs, while Wi-Fi remains a separate project.

## The Yoga 9 14IAP7 inventory

This is the machine the work is aimed at (Lenovo `82LU`, i7-1260P, 16 GiB,
512 GB SK hynix NVMe, Intel AX211 Wi-Fi, Iris Xe).

| Hardware | Linux name | 0.9 status | Next |
|---|---|---|---|
| NVMe SSD | SK hynix HFS512GEJ9X115N | **Rust driver** (`nvme0`) | hardware validation, namespaces, DMA alignment |
| Graphics | Alder Lake-P GT2 / Iris Xe | UEFI GOP framebuffer only | no modesetting/3D |
| Internal keyboard | i8042 / AT Translated Set 2 | PS/2 driver (existing) | validate on hardware |
| Touchpad and pen | I²C-HID on LPSS I2C1/I2C0 | **driver added**, polling, tap-to-click; hardware validation pending | GPIO interrupts, ACPI power/resource binding |
| Wi-Fi | Intel AX211 (`iwlwifi`) | **not supported** | firmware loading, 802.11/WPA2/WPA3, PCIe |
| Fingerprint | Goodix USB | **not supported** | USB xHCI + vendor protocol |
| Camera | Chicony USB UVC | **not supported** | USB xHCI + UVC |
| Audio | Intel HDA / SOF | **not supported** | HDA/ACPI |
| Bluetooth | AX211 USB | **not supported** | USB xHCI + Bluetooth stack |
| Battery, lid, tablet mode | ACPI / Intel HID events | **not supported** | ACPI AML interpreter |
| Thunderbolt 4 / USB4 | Intel TB4 | **not supported** | USB4/PCIe tunneling |
| USB ports | xHCI | **driver added** (root ports, HID, USB Ethernet) | external hub tier, USB storage |
| Wired network | USB Ethernet dongle | **CDC-ECM driver added** | RTL8153/AX88179 vendor protocols |

The netboot/update path now has two network choices on this laptop: a USB
Ethernet dongle (CDC-ECM) or a future Wi-Fi driver. The internal touchpad no
longer requires a PS/2 mouse path. The storage half was already in place.

An ISO built from `main` before 0.9 has none of these USB/I²C drivers, so it
still reaches the desktop with only the PS/2 keyboard and no pointer. Use an
ISO built from the 0.9 branch (or the Build ISO workflow) when testing on the
physical laptop.

### The ACPI resources used

The machine's DSDT (Lenovo `CB-01`, 82LU) places the touchpad at
`\_SB.PC00.I2C1` (`PCI 00:15.1`) with a 400 kHz bus. The ACPI `TPD0` device is
at `0x2c` for the Synaptics variant and `0x15` for the Elan variant; the pen
`TPL1` is on `I2C0` at `0x0a`. The driver probes those addresses, validates
the 30-byte HID descriptor (`bcdVersion` 1.00), reads the report descriptor,
and polls the input register. It does not interpret AML or claim the GPIO
interrupt; that is why it polls and why the next hardware step is an ACPI
resource binding.

## USB stack

`kernel/usb.c` is a polled xHCI 1.x host-controller driver:

- BIOS/OS handoff through the USB Legacy Support extended capability;
- one owned page each for the device-context array, command ring, event ring,
  event-ring segment table and endpoint rings;
- enable-slot, address-device, evaluate-context and configure-endpoint
  commands with checked completion codes;
- control, bulk and interrupt transfers with a PIT deadline;
- root ports only; external hubs are not parsed yet.

`kernel/usb_hid.c` implements boot-protocol keyboards and mice.
`kernel/usb_net.c` implements CDC-ECM: QEMU exposes RNDIS as one configuration
and CDC-ECM as another, and the driver selects the ECM configuration by its
control-interface subclass. The network stack still does all IPv4, DHCP, DNS,
TCP and firewall work.

## I²C-HID stack

`kernel/i2c_hid.c` has three layers:

1. the Intel LPSS private reset/remap registers and a Synopsys DesignWare I2C
   master (fast mode, 400 kHz class bus);
2. the HID-over-I2C descriptor/command/input registers, with the 2-byte
   little-endian register address and repeated-start reads;
3. a bounded HID report-descriptor parser that finds Generic Desktop mouse
   fields, Digitizer touchpad fields, buttons, wheel and contact state.

A touchpad with physical buttons uses them. A clickpad without button fields
uses tap-to-click after a short, nearly stationary contact. A pen on I2C0 uses
the tip switch as the primary button, so it can drag normally.

The parser and decoder are exercised by a synthetic mouse descriptor and a
synthetic Precision-Touchpad descriptor in `make unittest`.

## The Rust driver contract

Kernel drivers are ring-0 code, so the Rust boundary is deliberately narrow:

- `#![no_std]`, no allocator, no Rust runtime. The drivers are static
  libraries linked into the kernel.
- `#![forbid(unsafe_op_in_unsafe_fn)]`. MMIO and the C ABI are the only
  `unsafe` operations, and each is small enough to audit.
- Every DMA buffer comes from `pmm_alloc_contig`, is page-aligned, zeroed and
  owned by the driver. User pointers are never passed to hardware.
- Queue depths, doorbell strides, command sizes, PRP list lengths, namespace
  sizes and every LBA range are checked before use.
- Commands poll a bounded completion queue with a **PIT-based deadline**, not
  an unbounded spin. A broken device cannot hang the kernel forever.
- A completion status is checked; an error becomes `-E_IO`, never a guessed
  success.
- The release profile enables `overflow-checks`, disables unwinding and uses
  a panic handler that only stops the CPU.
- The driver never logs on the I/O path; log strings are bounded and
  NUL-terminated before they reach the C `kprintf`.

The NVMe driver is `drivers/nvme/src/lib.rs`; the C helpers are
`kernel/drivers.c`. The build is `tools/build-kernel-drivers.sh` and needs the
`x86_64-unknown-none` Rust target:

```sh
rustup target add x86_64-unknown-none
tools/make-signing-key.py --key /secure/aether-release.pem
AETHER_SIGNING_KEY=/secure/aether-release.pem make images
AETHER_SIGNING_KEY=/secure/aether-release.pem make test-nvme
```

## Testing NVMe

QEMU can attach an NVMe namespace and boot the installed test layout:

```sh
make images
tools/mkdisk.py --output build/nvme.img --boot build/boot.img \
    --system build/system.img --size-mib 2048
qemu-system-x86_64 -machine q35 -m 1G \
    -drive if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd \
    -drive file=build/nvme.img,format=raw,if=none,id=dn \
    -device nvme,drive=dn,serial=TESTNVME \
    -serial stdio
```

The serial log should contain the Rust driver lines, `[blk] nvme0: … sectors`,
and the installed system should reach the lock screen. The driver was
validated this way against QEMU's NVMe controller, including reads, writes,
flush, GPT parsing and formatting the encrypted data partition.

## Testing USB

`make test-usb` boots a live ISO with `qemu-xhci`, `usb-kbd`, `usb-mouse` and
`usb-net`, sends a key and pointer motion through QEMU's monitor, and requires:

```text
[usb] xHCI ...
HID keyboard ready
HID mouse ready
HID keyboard input active
HID mouse input active
[net] USB Ethernet ready
[net] address 10.0.2.15 ...
```

The full network path also passes with the ordinary `make test` e1000e card.

## Driver roadmap and security notes

1. **USB xHCI** (done): control, bulk and interrupt transfers, root ports,
   device addressing and endpoint configuration. The next step is an external
   hub tier and USB storage.
2. **USB HID** (done): boot keyboard and mouse, used by the live ISO and the
   desktop on machines without a PS/2 mouse.
3. **USB Ethernet** (done for CDC-ECM): the physical netboot/update path with
   a USB dongle. RTL8153/AX88179 vendor protocols are future work.
4. **I²C-HID** (driver added): touchpad and pen over Intel LPSS I2C. The next
   step is binding the ACPI `_CRS`/GPIO interrupt and power resources instead
   of polling and probing the known addresses.
5. **Wi-Fi**: firmware licensing/loading, a full 802.11 MAC state machine,
   WPA2/WPA3, and only then a Settings UI.

Each of these follows the same rules: no third-party driver code, DMA only
from owned pages, bounded queues, explicit timeouts, and a Rust `unsafe`
surface kept to MMIO/ABI access.

# Roadmap: from here to a real laptop

## Done

- **1.4** Signed app packages and a publisher key; Secure Boot loader
  signing; reproducible image builds; a release/key-rotation checklist.

- **1.0** Session lock and idle auto-lock; per-thread system dialogs; focus
  theft, window-slot and DLP metadata fixes; hostile-superblock and truncate
  hardening; boot-image pinning in the signed system image; app updates that
  cannot widen permissions; UDP/DNS and private-literal network hardening;
  crash-safe save ordering and a Settings Security page.

- **0.1** UEFI loader, framebuffer text, memory map.
- **0.2** Higher-half kernel loaded anywhere; GDT/TSS/IDT with crash screen;
  physical, virtual and heap allocators; PIC/PIT timer; PS/2 keyboard and
  mouse; RTC clock; isolated user-mode processes with capabilities and
  preemptive scheduling; system calls with pointer validation; W^X, NX,
  SMEP, SMAP, UMIP, stack canaries, guard pages; compositing window
  manager; Terminal and Calculator; original font.
- **0.3** PCI and AHCI (SATA) disks; from-scratch crypto (SHA-256, HMAC,
  PBKDF2, HKDF, ChaCha20-Poly1305) and a CSPRNG; an encrypted, authenticated
  file system with an unlock screen; DLP labels that follow the data, a
  kernel-drawn file picker and label dialog, and an audit log; trust levels
  with syscall allowlists, memory quotas and no executable memory for
  untrusted apps; Notes (untrusted).
- **0.4** Threads, futexes and made-on-touch memory; a Linux system call
  layer for static Rust programs, with æther's sandbox and DLP rules; app
  packages read from the ISO and checked by SHA-256; an eframe stand-in that
  draws egui in software; GridCraft and PhotoCraft running unmodified.
- **0.5** Networking: an Intel PRO/1000 driver, ARP, IPv4, DHCP, DNS and a
  TCP client, with a firewall that asks per app and site and nothing
  listening. A white-room browser in Rust (HTML/CSS engine, TLS 1.3, no
  JavaScript). Files, the file explorer.
- **0.6** Settings (network, firewall, privacy, about) and App Store as Rust
  apps; App Store installs and updates app packages from GitHub; persistent
  `/system/apps`; static IPv4 configuration; live updates with an "update
  pending" title note. Calculator, Notes, Files, Probe and Terminal remain
  native C. Core hardening from the 0.6 review.

## 0.9 (in progress)

- **USB on the physical laptop**: a polled xHCI root-port driver with control,
  bulk and interrupt transfers; boot-protocol USB HID keyboard/mouse; and
  CDC-ECM USB Ethernet. `make test-usb` covers all three in QEMU.
- **I²C-HID touchpad/pen**: LPSS I2C1/I2C0, HID-over-I2C descriptor and report
  parsing, relative mouse and absolute Precision-Touchpad decoding, physical
  buttons and tap-to-click. Polling replaces the missing ACPI GPIO interrupt;
  hardware validation on the Yoga is the next step.
- Still open on this laptop: Wi-Fi (AX211), external USB hubs, USB storage,
  camera/audio/Bluetooth/fingerprint, battery/lid/backlight, and Secure Boot.

## 0.8 (done)

- **Netboot installer**: a stable, small ISO that runs from RAM, writes a GPT
  (ESP, CRC-checked boot control, two 256 MiB system slots, encrypted data),
  and pulls the boot and system images from the latest GitHub release.
- **Self-updating installs**: App Store's OS tab writes a hash-checked system
  image to the inactive A/B slot; the loader makes two attempts and rolls
  back to the last good slot. Encrypted data and the running slot are never
  touched.
- **Rust NVMe driver**: the first ring-0 Rust driver, with owned DMA, checked
  queues/PRPs/LBAs and PIT-based timeouts. QEMU-tested for read, write, flush,
  GPT parsing and formatting an encrypted partition.

## 0.7 (done)

- **Browser**: a Chrome-like tab strip (new/close/switch tabs, per-tab
  history and script choice), a new-tab page, and real picture rendering.
  The clean-room engine now decodes PNG, baseline and progressive JPEG, GIF
  and BMP with hard size limits; `<img>` uses the picture's intrinsic size,
  data URLs work, and an https page never loads an http picture. Still to do:
  CSS background images, WebP/AVIF/SVG, downloads, and a process per site.
- **Icons**: original æther icons for Browser, Settings and App Store, and
  the Craft apps' own icons from <https://getartcraft.com/apps>. Packages
  carry an RGBA icon with its own SHA-256; the dock and App Store draw it.
- **ArtCraft packages**: publish `*-aether-x86_64.app` assets from the Craft
  repositories (the installer already understands them). Add the remaining
  Craft apps' shells and software paths for the GPU-heavy ones (EffectCraft,
  LightCraft, FilmCraft, CADCraft's paint callbacks), and æther versions of
  rfd, arboard, zbus and cpal.
- **More release hardening**: sign the installed app packages and add Secure
  Boot signing for the loader/kernel. OS system images are already
  RSA-2048-signed and verified by the loader and kernel.
- **Wi-Fi**: drivers for real laptops, then the Settings Wi-Fi page becomes a
  real scan/connect UI.
- **Network follow-ups**: IPv6, TCP window scaling, a process per browser
  site, downloads.
- **App security**: signed packages (authenticity as well as integrity) and
  Secure Boot signing.

## Later

1. **Hardening**: KASLR and app ASLR, per-process random canaries, Secure
   Boot signing, kernel page-table isolation.
2. **Interrupts the modern way**: parse ACPI (MADT), switch from the legacy
   PIC to the Local APIC and I/O APIC, use the HPET / APIC timer.
3. **Storage**: NVMe (almost every laptop), GPT partitions, a journal for
   crash safety.
4. **More apps**: a system monitor.
   Shared memory and message-passing IPC between apps.
5. **SMP**: start the other CPU cores.
6. **Laptop hardware**: USB (xHCI) keyboard/mouse/touchpad, I2C-HID touchpads,
   battery and lid via ACPI (needs an AML interpreter), backlight.

## Trying real hardware

The ISO already boots on UEFI PCs: write it to a USB stick (Rufus in "DD
image" mode, or `dd`), disable Secure Boot, and pick the stick in the boot
menu. Expect the keyboard and touchpad to work only if the laptop routes
them through PS/2-compatible hardware; USB support is item 6.

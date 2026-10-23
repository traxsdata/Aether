# Firefox ESR

Work toward running Firefox on Aether is tracked in issue #118. This page
covers the staged package (issue #103) and the milestone 1 headless discovery
run (issue #107). Neither target is part of `make ci`, because the Firefox
download is about 86 MB and the unpacked tree is about 311 MB.

## Version and licence

| Field | Value |
| --- | --- |
| Release | Firefox ESR 153.5.1esr, Linux x86_64, en-US |
| Source | `https://archive.mozilla.org/pub/firefox/releases/153.5.1esr/` |
| Pin | [`tools/pins/firefox-esr.pin`](../../tools/pins/firefox-esr.pin) (file name, URL, SHA-256 from Mozilla's `SHA256SUMS`) |
| App id | `org.mozilla.firefox`; record version `153.5.1` (`linuxd` accepts only dotted numeric versions) |
| Import policy | `memory=jit` (SpiderMonkey needs writable and executable mappings) |

Firefox is licensed under the [Mozilla Public License 2.0](https://www.mozilla.org/MPL/2.0/).
Aether does not modify or redistribute it: the binaries are downloaded from
Mozilla at build time, verified against the pin, and repackaged unchanged
inside a local AppImage container. The container's runtime stub and `AppRun`
are Aether's own (Apache-2.0). "Firefox" is a Mozilla trademark. A build
redistributed to others would need to follow Mozilla's trademark and
distribution policy. This repository does not publish Firefox images.

To move to a new ESR, change the version, file name, URL and digest in the pin
file together, in one commit.

## Staging: `make firefox-stage`

```sh
make firefox-stage
```

[`tools/stage-firefox.py`](../../tools/stage-firefox.py):

1. Downloads the pinned tarball into `build/firefox/cache/`, or reuses it if
   it is cached and still matches the digest. A mismatch fails the build.
   `--offline` never downloads.
2. Unpacks `firefox/` to `opt/firefox/` (Mozilla's usual path, and one
   `linuxd`'s launch policy allows) and adds `AppRun` (which execs
   `opt/firefox/firefox`), `firefox.desktop`, and an icon.
3. Builds a zstd SquashFS with fixed owners and timestamps, then prepends a
   416-byte x86-64 ELF runtime stub with `AI\x02` in its identity bytes. This
   is the AppImage type 2 shape that `apps/linux/appimage` accepts. The stub
   only prints how to extract the image; Aether never runs it, and there is no
   FUSE runtime. To inspect the image on Linux, run
   `unsquashfs -o 416 build/firefox/firefox.AppImage`.
4. Imports the tree on the host with `squashfs-extract`, which applies the
   kernel's `Limits::default()`. This proves the in-OS importer will accept the
   tree. For example, symlinks containing `..` are rejected, so the image has
   no `usr/bin/firefox` link.
5. Signs `build/firefox/firefox.import` (a `linux-appimage` record with
   `memory=jit`, signed with the test publisher key) and verifies it against
   the image.

Outputs: `build/firefox/firefox.AppImage` (about 112 MB),
`build/firefox/firefox.import`, `build/firefox/firefox.metadata`.

## Milestone 1 discovery: `make firefox-headless-test`

```sh
export AETHER_VNC_DISPLAY=33        # pick a free display if QEMU is shared
make firefox-headless-test          # TCG; FIREFOX_TIMEOUT=1800 by default
```

This boots Aether with the staged Firefox and runs

```
/opt/firefox/firefox-bin --headless --screenshot /tmp/out.png \
    file:///usr/share/firefox-test/index.html
```

with `MOZ_HEADLESS=1`, the four `MOZ_DISABLE_*_SANDBOX=1` variables,
`MOZ_FORCE_DISABLE_E10S=153.5.1`, and a default-prefs file that sets
`browser.tabs.remote.autostart=false`. The kernel is built with
`large-apps gate4-runtime`, which gives Linux-sized fd, thread and mapping
limits and a 1.5 GiB import heap, and the guest gets 6 GiB of RAM (the contiguous heap does not fit below the
4 GiB PCI hole with less). The argv and
environment come from `FIREFOX_ARGS` and `FIREFOX_ENV`. They are compiled into
the kernel and given only to the app named by `LINUX_APPIMAGE_ARGS_APP`.

**The target does not pass yet.** It is a measurement harness. Read the serial
log in `build/serial.log` to see how far Firefox got. The current blockers are
listed in issue #107.

Two workarounds in [`tools/firefox-headless-root.sh`](../../tools/firefox-headless-root.sh)
are test scaffolding, not product behaviour:

- The kernel keeps only the selected program from an imported AppImage, so the
  Firefox tree is also copied into the runtime rootfs at `/opt/firefox`.
- `libxul.so` links GTK 3, X11, fontconfig, glib, dbus and libstdc++, even
  when headless. The Gate 3 root carries only glibc, so the host copies of
  those libraries are added, in the same host-derived way the Gate 3 glibc pin
  is built.

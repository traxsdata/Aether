#!/usr/bin/env python3
"""mkapp.py - wrap a program's ELF image into an æther app package (name.app).

The package is a 512-byte header (see struct app_header in kernel/apps.h)
followed by the ELF, then an optional RGBA icon. The kernel checks the header,
the image's SHA-256 and the icon before it lists the app. Installed apps always
run untrusted, with at most a window, the file picker, a private folder and
outgoing connections, whatever the header asks for.

  tools/mkapp.py --name gridcraft --title GridCraft --version 1.0.0 \\
      --source github.com/storytold/gridcraft --abi linux --quota 1024 \\
      --icon assets/icons/craft/gridcraft.png \\
      target/x86_64-unknown-linux-musl/release/gridcraft build/apps/gridcraft.app

The icon must be a non-interlaced 8-bit PNG (gray, RGB, palette, gray+alpha or
RGBA), at most 256x256. It is stored as RGBA8888 at the end of the package.
"""
import argparse, hashlib, re, struct, sys, zlib

CAP_WINDOW, CAP_PICK, CAP_STORAGE, CAP_NET = 0x01, 0x40, 0x80, 0x100
ICON_MAX = 256


def field(s, n):
    b = s.encode()
    if len(b) >= n:
        sys.exit(f"mkapp: '{s}' is longer than {n - 1} bytes")
    return b.ljust(n, b"\0")


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else (b if pb <= pc else c)


def read_png(path):
    """A small, strict PNG reader: 8-bit, non-interlaced, no gamma games."""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"mkapp: {path} is not a PNG")
    pos = 8
    ihdr = None
    palette = None
    trns = None
    idat = bytearray()
    while pos + 12 <= len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        typ = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + n]
        if pos + 12 + n > len(data):
            sys.exit(f"mkapp: {path} is truncated")
        pos += 12 + n
        if typ == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif typ == b"PLTE":
            palette = body
        elif typ == b"tRNS":
            trns = body
        elif typ == b"IDAT":
            idat += body
            if len(idat) > 16 << 20:
                sys.exit(f"mkapp: {path} is too big")
        elif typ == b"IEND":
            break
    if not ihdr or not idat:
        sys.exit(f"mkapp: {path} has no image data")
    w, h, depth, color, comp, filt, interlace = ihdr
    if not (1 <= w <= ICON_MAX and 1 <= h <= ICON_MAX):
        sys.exit(f"mkapp: icon is {w}x{h}; the limit is {ICON_MAX}x{ICON_MAX}")
    if depth != 8 or comp != 0 or filt != 0 or interlace != 0:
        sys.exit(f"mkapp: icon must be 8-bit and non-interlaced (got depth {depth}, interlace {interlace})")
    channels = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color)
    if channels is None:
        sys.exit(f"mkapp: unsupported PNG colour type {color}")
    if color == 3 and (not palette or len(palette) % 3):
        sys.exit(f"mkapp: palette PNG without a usable PLTE chunk")
    try:
        raw = zlib.decompress(bytes(idat))
    except zlib.error as e:
        sys.exit(f"mkapp: icon data is corrupt ({e})")
    stride = w * channels
    if len(raw) != h * (stride + 1):
        sys.exit(f"mkapp: icon data has the wrong size")
    rows = []
    prev = bytearray(stride)
    p = 0
    for _ in range(h):
        f = raw[p]
        row = bytearray(raw[p + 1:p + 1 + stride])
        p += 1 + stride
        for i in range(stride):
            a = row[i - channels] if i >= channels else 0
            b = prev[i]
            c = prev[i - channels] if i >= channels else 0
            if f == 1:
                row[i] = (row[i] + a) & 0xFF
            elif f == 2:
                row[i] = (row[i] + b) & 0xFF
            elif f == 3:
                row[i] = (row[i] + ((a + b) >> 1)) & 0xFF
            elif f == 4:
                row[i] = (row[i] + paeth(a, b, c)) & 0xFF
            elif f != 0:
                sys.exit(f"mkapp: unknown PNG filter {f}")
        rows.append(bytes(row))
        prev = row
    out = bytearray(w * h * 4)
    o = 0
    for row in rows:
        if color == 0:
            for i in range(w):
                g = row[i]
                out[o:o + 4] = bytes((g, g, g, 255))
                o += 4
        elif color == 2:
            for i in range(w):
                r, g, b = row[i * 3:i * 3 + 3]
                out[o:o + 4] = bytes((r, g, b, 255))
                o += 4
        elif color == 3:
            for i in range(w):
                k = row[i]
                if k * 3 + 2 >= len(palette):
                    sys.exit(f"mkapp: palette index {k} is out of range")
                r, g, b = palette[k * 3:k * 3 + 3]
                a = trns[k] if trns and k < len(trns) else 255
                out[o:o + 4] = bytes((r, g, b, a))
                o += 4
        elif color == 4:
            for i in range(w):
                g, a = row[i * 2:i * 2 + 2]
                out[o:o + 4] = bytes((g, g, g, a))
                o += 4
        else:
            for i in range(w):
                out[o:o + 4] = row[i * 4:i * 4 + 4]
                o += 4
    return w, h, bytes(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--name", required=True)
    ap.add_argument("--title")
    ap.add_argument("--version", default="")
    ap.add_argument("--source", default="")
    ap.add_argument("--abi", choices=["aether", "linux"], default="linux")
    ap.add_argument("--quota", type=int, default=1024, help="memory limit in MiB")
    ap.add_argument("--color", default="38E1F5", help="dock icon colour when there is no icon, hex RGB")
    ap.add_argument("--icon", help="PNG icon for the dock and App Store")
    ap.add_argument("--no-files", action="store_true", help="no file picker")
    ap.add_argument("--no-window", action="store_true", help="a console program: no window, not in the dock")
    ap.add_argument("--no-storage", action="store_true", help="no private folder")
    ap.add_argument("--net", action="store_true", help="ask for outgoing connections (CAP_NET, checked by the firewall)")
    ap.add_argument("elf")
    ap.add_argument("out")
    a = ap.parse_args()
    if not re.fullmatch(r"[a-z0-9-]{1,15}", a.name):
        sys.exit("mkapp: name must be 1-15 characters of a-z, 0-9 and '-'")
    elf = open(a.elf, "rb").read()
    if elf[:4] != b"\x7fELF":
        sys.exit("mkapp: not an ELF file")
    icon = None
    if a.icon:
        iw, ih, icon = read_png(a.icon)
    caps = (0 if a.no_storage else CAP_STORAGE) | (0 if a.no_files else CAP_PICK) | (0 if a.no_window else CAP_WINDOW)
    caps |= CAP_NET if a.net else 0
    hdr = b"AETHAPP1" + struct.pack("<II", 512, 1)
    hdr += field(a.name, 32) + field(a.title or a.name, 32) + field(a.version, 32) + field(a.source, 96)
    hdr += struct.pack("<6I", caps, 2, a.quota, 1 if a.abi == "linux" else 0, int(a.color, 16), 0)
    hdr += struct.pack("<Q", len(elf)) + hashlib.sha256(elf).digest()
    if icon:
        offset = 512 + len(elf)
        hdr += struct.pack("<5I", 1, iw, ih, offset, len(icon)) + hashlib.sha256(icon).digest()
    else:
        hdr += struct.pack("<5I", 0, 0, 0, 0, 0) + b"\0" * 32
    hdr = hdr.ljust(512, b"\0")
    assert len(hdr) == 512
    with open(a.out, "wb") as f:
        f.write(hdr + elf + (icon or b""))
    extra = f", icon {iw}x{ih}" if icon else ""
    print(f"mkapp: {a.out} ({len(elf) // 1024} KiB, {a.abi}{extra})")


if __name__ == "__main__":
    main()

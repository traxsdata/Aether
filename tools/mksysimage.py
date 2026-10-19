#!/usr/bin/env python3
"""mksysimage.py - build an æther system image for an A/B slot.

Layout (all offsets from the start of the image):

    0 ... 4095      struct sysimage_header (common/sysimage.h)
    4096            kernel.elf
    ...             each .app package, aligned to 4096
    image_size      padded to 4096

The header's payload_sha256 covers everything after the header. The loader
checks it before mapping the kernel or handing any app package to the kernel.

  tools/mksysimage.py --output build/system.img --version 0.8.0 \\
      --kernel build/kernel.elf build/apps/settings.app build/apps/installer.app
"""
import argparse
import hashlib
import os
import re
import struct
import subprocess
import tempfile
import time

HEADER = 4096
MAGIC = b"AETHERS1"
APPS_MAX = 32
NAME = 32


def field(s, n):
    b = s.encode()
    return b[: n - 1].ljust(n, b"\0")


def app_name(path, data):
    if len(data) >= 48 and data[:8] == b"AETHAPP1":
        raw = data[16:48].split(b"\0", 1)[0]
        if raw:
            return raw.decode("ascii", "replace")
    return os.path.basename(path).removesuffix(".app")[: NAME - 1]


def align(n, a=4096):
    return (n + a - 1) // a * a


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--output", required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--build", default=os.environ.get("AETHER_BUILD", time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime())))
    ap.add_argument("--kernel", required=True)
    ap.add_argument("--boot", help="ESP boot image whose SHA-256 is pinned in the signed header")
    ap.add_argument("--key", default=os.environ.get("AETHER_SIGNING_KEY"),
                    help="RSA-2048 private key (PEM) for the system image signature")
    ap.add_argument("apps", nargs="*")
    a = ap.parse_args()

    kernel = open(a.kernel, "rb").read()
    if kernel[:4] != b"\x7fELF":
        raise SystemExit("mksysimage: the kernel is not an ELF file")
    if len(a.apps) > APPS_MAX:
        raise SystemExit(f"mksysimage: at most {APPS_MAX} apps")

    pieces = []
    offset = align(HEADER + len(kernel))
    entries = []
    for path in a.apps:
        data = open(path, "rb").read()
        if data[:8] != b"AETHAPP1":
            raise SystemExit(f"mksysimage: {path} is not an app package")
        name = app_name(path, data)
        entries.append((name, offset, len(data)))
        pieces.append((offset, data))
        offset = align(offset + len(data))
    image_size = align(offset)

    image = bytearray(image_size)
    image[HEADER:HEADER + len(kernel)] = kernel
    for off, data in pieces:
        image[off:off + len(data)] = data

    if not a.key:
        raise SystemExit("mksysimage: --key (or AETHER_SIGNING_KEY) is required; "
                         "system images are RSA-signed and the loader rejects unsigned ones")
    # Refuse to sign with a key the loader will not trust.
    key_mod = subprocess.check_output(["openssl", "rsa", "-in", a.key, "-noout", "-modulus"],
                                      stderr=subprocess.DEVNULL).decode().strip().split("=", 1)[1].lower()
    header_path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "common", "release_key.h")
    try:
        header = open(header_path).read()
    except OSError:
        raise SystemExit(f"mksysimage: cannot read {header_path}")
    mod_body = header.split("aether_release_modulus", 1)[1]
    trusted = bytes(int(x, 16) for x in re.findall(r"0x([0-9a-fA-F]{2})", mod_body))
    if len(trusted) != 256 or trusted.hex() != key_mod:
        raise SystemExit("mksysimage: this key doesn't match common/release_key.h; "
                         "the loader would reject every image it signs")
    payload_hash = hashlib.sha256(image[HEADER:]).digest()
    boot_hash = b"\0" * 32
    if a.boot:
        boot_image = open(a.boot, "rb").read()
        if len(boot_image) < 512 or boot_image[510:512] != b"\x55\xaa":
            raise SystemExit("mksysimage: --boot is not a FAT boot image")
        boot_hash = hashlib.sha256(boot_image).digest()
    hdr = bytearray(HEADER)
    hdr[0:8] = MAGIC
    struct.pack_into("<IIQQQII", hdr, 8, 1, HEADER, image_size, HEADER, len(kernel), len(entries), 0)
    hdr[48:48 + 32] = field(a.version, 32)
    hdr[80:80 + 32] = field(a.build, 32)
    for i, (name, off, size) in enumerate(entries):
        base = 112 + i * 48
        hdr[base:base + 32] = field(name, 32)
        struct.pack_into("<QQ", hdr, base + 32, off, size)
    hdr[1648:1680] = payload_hash
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", a.version)
    if not m:
        raise SystemExit("mksysimage: --version must be numeric major.minor.patch")
    vcode = (int(m.group(1)) << 16) | (int(m.group(2)) << 8) | int(m.group(3))
    struct.pack_into("<I", hdr, 1976, vcode)
    hdr[1980:2012] = boot_hash
    # The canonical signed/hashed form has image_sha256 and signature zeroed.
    struct.pack_into("<II", hdr, 1712, 1, 256)  # RSA-2048 / PKCS#1 v1.5 SHA-256
    canonical = bytes(hdr) + bytes(image[HEADER:])
    image_hash = hashlib.sha256(canonical).digest()
    os.makedirs(os.path.dirname(a.output) or ".", exist_ok=True)
    hdr[1680:1712] = image_hash
    with tempfile.NamedTemporaryFile(prefix="aether-sign-", dir=os.path.dirname(a.output) or ".", delete=False) as tf:
        tf.write(canonical)
        canonical_path = tf.name
    sig_path = canonical_path + ".sig"
    try:
        subprocess.run(["openssl", "dgst", "-sha256", "-sign", a.key, "-out", sig_path, canonical_path],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        signature = open(sig_path, "rb").read()
    except subprocess.CalledProcessError as e:
        raise SystemExit(f"mksysimage: signing failed: {e.stderr.decode(errors='replace').strip()}")
    finally:
        os.unlink(canonical_path)
        if os.path.exists(sig_path):
            os.unlink(sig_path)
    if len(signature) != 256:
        raise SystemExit(f"mksysimage: expected a 256-byte RSA-2048 signature, got {len(signature)}")
    hdr[1720:1976] = signature
    image[0:HEADER] = hdr

    os.makedirs(os.path.dirname(a.output) or ".", exist_ok=True)
    with open(a.output, "wb") as f:
        f.write(image)
    print(f"mksysimage: {a.output} ({image_size // 1024} KiB, kernel {len(kernel) // 1024} KiB, "
          f"{len(entries)} app{'s' if len(entries) != 1 else ''}, {payload_hash.hex()[:16]}...)")


if __name__ == "__main__":
    main()
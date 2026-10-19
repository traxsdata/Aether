#!/usr/bin/env python3
"""mkdisk.py - make a test/installation disk with the 0.8 A/B layout.

This is the same layout update_install() writes, implemented independently so
the kernel side and the loader can be tested without a network install:

    ESP (64 MiB) | boot control (1 MiB) | system A (256 MiB)
                 | system B (256 MiB)   | AFS data (the rest)

  tools/mkdisk.py --output build/installed.img --boot build/boot.img \\
      --system build/system.img --size-mib 2048
"""
import argparse
import hashlib
import os
import struct
import zlib

SECTOR = 512
ESP = 0
BOOT = 1
SYS_A = 2
SYS_B = 3
DATA = 4
ENTRIES_LBA = 2
ENTRY_SIZE = 128
ENTRIES = 128
ARRAY_SECTORS = ENTRIES * ENTRY_SIZE // SECTOR

TYPE_ESP = bytes.fromhex("c12a7328f81f11d2ba4b00a0c93ec93b")
TYPE_BOOT = bytes.fromhex("a0e7b0015e4d4c8f9b216a7d3c2f1e01")
TYPE_A = bytes.fromhex("a0e7b0025e4d4c8f9b216a7d3c2f1e01")
TYPE_B = bytes.fromhex("a0e7b0035e4d4c8f9b216a7d3c2f1e01")
TYPE_DATA = bytes.fromhex("a0e7b0045e4d4c8f9b216a7d3c2f1e01")


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def layout(sectors):
    sizes = [64 << 20 >> 9, 1 << 20 >> 9, 256 << 20 >> 9, 256 << 20 >> 9]
    out = []
    at = 2048
    for n in sizes:
        out.append((at, at + n - 1))
        at = at + n
        at = (at + 2047) // 2048 * 2048
    last_usable = sectors - ARRAY_SECTORS - 2
    out.append((at, last_usable))
    # Every partition must fit before the backup GPT, and the data partition
    # needs room to be useful. Refuse a too-small image instead of writing a
    # table whose entries point outside the disk.
    if at >= last_usable or last_usable - at < (64 << 20 >> 9):
        raise ValueError("disk is too small for the ESP, two system slots and a data partition")
    if out[3][1] >= last_usable:
        raise ValueError("system slots overlap the backup GPT")
    return out, last_usable


def part_entry(typ, guid, first, last, attrs, name):
    e = bytearray(128)
    e[0:16] = typ
    e[16:32] = guid
    struct.pack_into("<QQQ", e, 32, first, last, attrs)
    name = name.encode()[:35]
    for i, c in enumerate(name):
        e[56 + i * 2] = c
    return bytes(e)


def header(my, alt, entries_lba, disk_guid, first_usable, last_usable, entries_crc):
    h = bytearray(512)
    h[0:8] = b"EFI PART"
    struct.pack_into("<IIIIQQQQ", h, 8, 0x00010000, 92, 0, 0, my, alt, first_usable, last_usable)
    h[56:72] = disk_guid
    struct.pack_into("<QIII", h, 72, entries_lba, ENTRIES, ENTRY_SIZE, entries_crc)
    struct.pack_into("<I", h, 16, 0)
    struct.pack_into("<I", h, 16, crc32(h[:92]))
    return bytes(h)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--output", required=True)
    ap.add_argument("--boot", required=True)
    ap.add_argument("--system", required=True)
    ap.add_argument("--size-mib", type=int, default=2048)
    a = ap.parse_args()
    sectors = a.size_mib * 2048
    try:
        positions, _last_usable = layout(sectors)
    except ValueError as e:
        raise SystemExit(f"mkdisk: {e}")
    first = [p[0] for p in positions]
    last = [p[1] for p in positions]
    boot_img = open(a.boot, "rb").read()
    system = open(a.system, "rb").read()
    if len(boot_img) > (last[ESP] - first[ESP] + 1) * SECTOR:
        raise SystemExit("mkdisk: boot image is larger than the ESP")
    if len(system) > (last[SYS_A] - first[SYS_A] + 1) * SECTOR:
        raise SystemExit("mkdisk: system image is larger than a system slot")

    with open(a.output, "wb") as f:
        f.truncate(sectors * SECTOR)
        f.seek(first[ESP] * SECTOR)
        f.write(boot_img)
        f.seek(first[SYS_A] * SECTOR)
        f.write(system)

        # Boot control: three copies, slot A active and good.
        bc = bytearray(512)
        bc[0:8] = b"AETHBOOT"
        struct.pack_into("<IIIIIIQ", bc, 8, 1, 0, 0, 0xFFFFFFFF, 0, 0, 1)
        struct.pack_into("<QQ", bc, 40, len(system), 0)
        bc[56:88] = system[1680:1712]  # canonical image digest from the signed header
        struct.pack_into('<I', bc, 28, struct.unpack_from('<I', system, 1976)[0])
        struct.pack_into("<I", bc, 508, crc32(bc[:508]))
        f.seek(first[BOOT] * SECTOR)
        f.write(bytes(bc) * 3)

        # GPT
        disk_guid = os.urandom(16)
        guids = [os.urandom(16) for _ in range(5)]
        entries = bytearray(ENTRIES * ENTRY_SIZE)
        specs = [
            (TYPE_ESP, guids[0], "AETHER ESP", 0),
            (TYPE_BOOT, guids[1], "AETHER BOOT", 0),
            (TYPE_A, guids[2], "AETHER SYSTEM A", 0),
            (TYPE_B, guids[3], "AETHER SYSTEM B", 0),
            (TYPE_DATA, guids[4], "AETHER DATA", 0),
        ]
        for i, (typ, guid, name, attrs) in enumerate(specs):
            entries[i * ENTRY_SIZE:(i + 1) * ENTRY_SIZE] = part_entry(typ, guid, first[i], last[i], attrs, name)
        entries_crc = crc32(entries)

        mbr = bytearray(512)
        mbr[446] = 0
        mbr[447:450] = bytes((0, 2, 0))
        mbr[450] = 0xEE
        mbr[451:454] = bytes((0xFE, 0xFF, 0xFF))
        struct.pack_into("<II", mbr, 454, 1, min(sectors - 1, 0xFFFFFFFF))
        mbr[510:512] = b"\x55\xaa"
        f.seek(0)
        f.write(mbr)
        f.seek(ENTRIES_LBA * SECTOR)
        f.write(entries)
        # The backup entry array sits immediately before the backup header.
        backup_entries = sectors - ARRAY_SECTORS - 1
        f.seek(backup_entries * SECTOR)
        f.write(entries)
        f.seek(SECTOR)
        f.write(header(1, sectors - 1, ENTRIES_LBA, disk_guid, 34, sectors - ARRAY_SECTORS - 2, entries_crc))
        f.seek((sectors - 1) * SECTOR)
        f.write(header(sectors - 1, 1, backup_entries, disk_guid, 34, sectors - ARRAY_SECTORS - 2, entries_crc))

    print(f"mkdisk: {a.output} ({a.size_mib} MiB, ESP at {first[ESP]}, slots at {first[SYS_A]}/{first[SYS_B]}, "
          f"data at {first[DATA]})")


if __name__ == "__main__":
    main()
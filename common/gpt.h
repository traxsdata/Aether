/*
 * gpt.h - GUID Partition Table, read and written from scratch.
 *
 * The same code runs in the UEFI loader and in the kernel. Both hand it a
 * tiny block-I/O interface, so the parser has no dependency on either side.
 * Only 512-byte logical sectors are accepted: every disk æther installs on
 * uses them, and mixing sector sizes with the kernel's block layer would be
 * an unnecessary source of bugs.
 *
 * A GPT is four pieces:
 *   LBA 0          protective MBR (one 0xEE partition covering the disk)
 *   LBA 1          primary header (CRC32 over the header and entry array)
 *   LBA 2..33      128 partition entries of 128 bytes
 *   last LBA       backup header, with the entry array just before it
 *
 * The primary copy is authoritative. If it fails its checks, the backup is
 * used. Writing always updates the entries before the headers, and writes
 * the backup after the primary, so a torn write can fall back to an older
 * consistent copy rather than to a random one.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#define GPT_SECTOR 512
#define GPT_ENTRIES 128
#define GPT_ENTRY_SIZE 128
#define GPT_ENTRIES_LBA 2
#define GPT_HEADER_LBA 1
#define GPT_FIRST_USABLE 34

/* GPT type GUIDs, in the byte order the on-disk entries use. */
extern const uint8_t gpt_type_esp[16];
extern const uint8_t gpt_type_aether_boot[16];
extern const uint8_t gpt_type_aether_system_a[16];
extern const uint8_t gpt_type_aether_system_b[16];
extern const uint8_t gpt_type_aether_data[16];

#define GPT_FLAG_LEGACY_BOOTABLE (1ull << 2) /* bit 2 of the attributes field */

struct gpt_partition {
    uint8_t type[16];
    uint8_t guid[16];
    uint64_t first_lba, last_lba; /* inclusive, in 512-byte sectors */
    uint64_t attributes;
    char name[37]; /* UTF-16 on disk; we keep ASCII and cap it */
};

struct gpt_new_partition {
    const uint8_t *type; /* one of gpt_type_* */
    const uint8_t *guid; /* 16 random bytes, or 0 for an all-zero GUID */
    uint64_t first_lba, last_lba;
    uint64_t attributes;
    const char *name;
};

struct gpt_io {
    void *ctx;
    uint64_t sectors; /* whole disk, 512-byte sectors */
    /* Both return 0 on success. lba and count are in 512-byte sectors. */
    int (*read)(void *ctx, uint64_t lba, uint32_t count, void *buf);
    int (*write)(void *ctx, uint64_t lba, uint32_t count, const void *buf);
    uint8_t disk_guid[16];        /* gpt_create input */
    uint8_t loaded_disk_guid[16]; /* gpt_load output */
};

uint32_t gpt_crc32(const void *data, size_t len);

/* Read the primary GPT, or the backup if the primary is damaged. Returns the
 * number of entries (0..GPT_ENTRIES), or a negative value:
 *   -1 not a GPT, -2 no header passed its checks, -3 bad entry array. */
int gpt_load(struct gpt_io *io, struct gpt_partition *out, int max);

/* Fill `out` and return 1 if a partition of this type exists. */
int gpt_find_type(const struct gpt_partition *parts, int count, const uint8_t type[16], int skip,
                  struct gpt_partition *out);

/* Replace the partition table with `parts` (0..GPT_ENTRIES entries). The
 * caller is responsible for making the partitions empty before this is used
 * on a disk that already holds data. Returns 0 or a negative value. */
int gpt_create(struct gpt_io *io, const struct gpt_new_partition *parts, int count);

/* A convenient layout for the installer: ESP, boot control, system A,
 * system B and data. Offsets are aligned to 1 MiB. `out` must hold 5. */
#define GPT_LAYOUT_ESP      0
#define GPT_LAYOUT_BOOT     1
#define GPT_LAYOUT_SYSTEM_A 2
#define GPT_LAYOUT_SYSTEM_B 3
#define GPT_LAYOUT_DATA     4
int gpt_layout(uint64_t disk_sectors, uint64_t out_first[5], uint64_t out_last[5]);
